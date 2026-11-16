// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Count caller-thread allocation requests, including mandatory JSON output.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static COUNT: Cell<Option<usize>> = const { Cell::new(None) };
    static REQUEST_BYTES: Cell<Option<usize>> = const { Cell::new(None) };
}

struct Counting;

fn requested(size: usize) {
    let _ = COUNT.try_with(|count| {
        if let Some(n) = count.get() {
            count.set(Some(n.wrapping_add(1)));
        }
    });
    let _ = REQUEST_BYTES.try_with(|bytes| {
        if let Some(n) = bytes.get() {
            bytes.set(Some(n.wrapping_add(size)));
        }
    });
}

// SAFETY: every operation delegates the original pointer and layout to System.
// The constant, drop-free thread-local counter neither allocates nor unwinds.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        requested(layout.size());
        // SAFETY: the caller supplies the allocator's required valid layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        requested(layout.size());
        // SAFETY: the caller supplies the allocator's required valid layout.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        requested(size);
        // SAFETY: the caller supplies a live System allocation and valid new size.
        unsafe { System.realloc(ptr, layout, size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller supplies a live System allocation and its layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn repeated_row_summary_does_not_allocate_per_match() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut terminal =
        aterm_wasm::AtermTerminal::new(1, 256, font, 14.0, 0, 0, 0, 0).expect("bundled face");
    let row = "a ".repeat(128);
    terminal.process(row.as_bytes());

    COUNT.with(|count| count.set(Some(0)));
    let summary = terminal
        .search_summary("a", true, false, 0)
        .expect("summary");
    let requests = COUNT.with(|count| count.replace(None).expect("counting"));

    let matches = (0..128)
        .map(|i| {
            format!(
                "{{\"absRow\":0,\"col\":{},\"len\":1,\"snippet\":\"{}\"}}",
                i * 2,
                row
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        summary,
        format!("{{\"matches\":[{matches}],\"total\":128,\"incomplete\":false}}")
    );
    eprintln!(
        "search summary: {requests} allocation requests, {} output bytes",
        summary.len()
    );
    assert!(
        requests <= 64,
        "repeated-row summary made {requests} allocation requests"
    );
}

#[test]
fn narrow_row_export_allocates_only_text_and_output() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut terminal =
        aterm_wasm::AtermTerminal::new(64, 80, font, 14.0, 0, 0, 0, 0).expect("bundled face");
    terminal.process(("line\r\n".repeat(63) + "line").as_bytes());

    COUNT.with(|count| count.set(Some(0)));
    let output = terminal.row_range_json(0, 64).expect("visible rows");
    let requests = COUNT.with(|count| count.replace(None).expect("counting"));

    let row = "{\"text\":\"line\",\"wrapped\":false,\"len\":4}";
    assert_eq!(output, format!("[{}]", vec![row; 64].join(",")));
    eprintln!(
        "narrow row export: {requests} allocation requests, {} output bytes",
        output.len()
    );
    assert!(
        requests <= 128,
        "narrow row export made {requests} allocation requests"
    );
}

#[test]
fn osc_event_drain_does_not_allocate_per_event() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut terminal =
        aterm_wasm::AtermTerminal::new(1, 80, font, 14.0, 0, 0, 0, 0).expect("bundled face");
    assert!(terminal.take_osc_events().is_none());
    for i in 0..64 {
        terminal.process(format!("\x1b]7;file:///fixture/{i}\x07").as_bytes());
    }

    COUNT.with(|count| count.set(Some(0)));
    let output = terminal.take_osc_events().expect("queued cwd reports");
    let requests = COUNT.with(|count| count.replace(None).expect("counting"));

    let records = (0..64)
        .map(|i| format!("[7,\"/fixture/{i}\"]"))
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(output, format!("[{records}]"));
    assert!(terminal.take_osc_events().is_none(), "fully drained");
    eprintln!(
        "OSC event drain: {requests} allocation requests, {} output bytes",
        output.len()
    );
    assert!(
        requests <= 64,
        "OSC event drain made {requests} allocation requests"
    );
}

#[test]
#[ignore = "manual before/after sparse row export measurement; run alone"]
fn sparse_row_export_cost() {
    use std::hint::black_box;
    use std::time::Instant;

    let cols: u16 = std::env::var("ATERM_SPARSE_EXPORT_COLS")
        .map_or(Ok(4096), |value| value.parse())
        .expect("column count");
    assert!((4..=4096).contains(&cols));
    let occupied: u16 = std::env::var("ATERM_SPARSE_EXPORT_TEXT_COLS")
        .map_or(Ok(4), |value| value.parse())
        .expect("occupied column count");
    assert!((1..=cols).contains(&occupied));
    let text = if occupied == 4 {
        "line".to_owned()
    } else {
        "a".repeat(usize::from(occupied))
    };
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let terminal = {
        let mut t =
            aterm_wasm::AtermTerminal::new(32, cols, font, 14.0, 0, 0, 0, 0).expect("bundled face");
        t.process(((text.clone() + "\r\n").repeat(31) + &text).as_bytes());
        t
    };
    let row = format!("{{\"text\":\"{text}\",\"wrapped\":false,\"len\":{occupied}}}");
    let expected = format!("[{}]", vec![row; 32].join(","));
    assert_eq!(
        terminal.row_range_json(0, 32).as_deref(),
        Some(expected.as_str())
    );
    eprintln!(
        "sparse row export: rows=32 cols={cols} occupied={occupied} batches=20 exports_per_batch=100 bytes={}",
        expected.len()
    );
    for batch in 0..20 {
        let start = Instant::now();
        for _ in 0..100 {
            let output = black_box(&terminal)
                .row_range_json(0, 32)
                .expect("visible rows");
            assert_eq!(black_box(&output), &expected);
        }
        eprintln!(
            "sparse row export batch={batch} elapsed_ns={}",
            start.elapsed().as_nanos()
        );
    }
}

#[test]
fn command_output_export_allocation_cost() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut t =
        aterm_wasm::AtermTerminal::new(8, 80, font, 14.0, 0, 0, 0, 0).expect("bundled face");
    t.process("\x1b]133;A\x07$ cmd\x1b]133;B\x07\r\n\x1b]133;C\x07a\"b\\漢\r\ntail\r\n\x1b]133;D;42\x07\x1b]133;A\x07".as_bytes());
    COUNT.with(|count| count.set(Some(0)));
    let output = t.last_command_output().expect("sealed output block");
    let requests = COUNT.with(|count| count.replace(None).expect("counting"));
    assert_eq!(
        output,
        r#"{"status":"ok","text":"a\"b\\漢\ntail","exitCode":42}"#
    );
    eprintln!(
        "command output export: {requests} allocation requests, {} output bytes",
        output.len()
    );
    assert!(
        requests <= 6,
        "command output export made {requests} allocation requests"
    );
}

#[test]
fn live_cell_width_query_does_not_materialize_graphemes() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut t = aterm_wasm::AtermTerminal::new(4, 160, font, 14.0, 0, 0, 0, 0).unwrap();
    t.process("a".repeat(159).as_bytes());
    COUNT.with(|count| count.set(Some(0)));
    let wide = t.cell_is_wide(0, 80);
    let requests = COUNT.with(|count| count.replace(None).unwrap());
    assert_eq!(wide, Some(false));
    eprintln!("cold live cell width: {requests} allocation requests");
    assert_eq!(
        requests, 0,
        "width-only metadata must not allocate grapheme strings"
    );
}

#[test]
fn theme_state_export_avoids_field_strings() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut t = aterm_wasm::AtermTerminal::new(4, 80, font, 14.0, 0, 0, 0, 0).unwrap();
    t.set_theme_by_name("Nord").unwrap();
    COUNT.with(|count| count.set(Some(0)));
    let state = t.theme_state();
    let requests = COUNT.with(|count| count.replace(None).unwrap());
    assert_eq!(
        state,
        r#"{"selector":"Nord","appearance":"dark","id":"Nord","side":"dark","reported":"dark","source":"builtin"}"#
    );
    eprintln!(
        "theme state: {requests} allocation requests, {} output bytes",
        state.len()
    );
    assert!(requests <= 2, "theme state allocated {requests} times");
}

#[test]
fn theme_catalog_export_uses_identity_metadata() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut t = aterm_wasm::AtermTerminal::new(4, 80, font, 14.0, 0, 0, 0, 0).unwrap();
    t.register_theme("name = Custom\nbackground = #101820\nforeground = #e0e0e0\n")
        .unwrap();
    COUNT.with(|count| count.set(Some(0)));
    let catalog = t.theme_catalog();
    let requests = COUNT.with(|count| count.replace(None).unwrap());
    assert!(catalog
        .starts_with(r#"{"unset":"Default","themes":[{"id":"Default","display_name":"Default""#));
    assert!(catalog.contains(r#""id":"Custom","display_name":"Custom""#));
    eprintln!(
        "theme catalog: {requests} allocation requests, {} output bytes",
        catalog.len()
    );
    assert!(
        requests <= 32,
        "catalog made {requests} allocation requests"
    );
}

#[test]
fn live_cell_text_query_only_allocates_its_result() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut t = aterm_wasm::AtermTerminal::new(4, 160, font, 14.0, 0, 0, 0, 0).unwrap();
    t.process("a".repeat(159).as_bytes());
    COUNT.with(|count| count.set(Some(0)));
    let text = t.cell_text(0, 80);
    let requests = COUNT.with(|count| count.replace(None).unwrap());
    assert_eq!(text, "a");
    eprintln!("cold live cell text: {requests} allocation requests");
    assert_eq!(
        requests, 1,
        "a live cell lookup must only allocate its result"
    );
}

#[test]
fn theme_registration_reply_allocation_cost() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut t = aterm_wasm::AtermTerminal::new(4, 80, font, 14.0, 0, 0, 0, 0).unwrap();
    COUNT.with(|count| count.set(Some(0)));
    let reply = t
        .register_theme("name = Custom\nbackground = #101820\nforeground = #e0e0e0\n")
        .unwrap();
    let requests = COUNT.with(|count| count.replace(None).unwrap());
    assert_eq!(
        reply,
        r#"{"id":"Custom","appearance":"dark","appearance_source":"measured","warnings":[]}"#
    );
    eprintln!(
        "theme registration: {requests} allocation requests, {} output bytes",
        reply.len()
    );
    assert!(
        requests <= 22,
        "theme registration allocated {requests} times"
    );
}

#[test]
fn smart_link_lookup_allocation_cost() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut t = aterm_wasm::AtermTerminal::new(4, 80, font, 14.0, 0, 0, 0, 0).unwrap();
    t.process(b"https://example.test/path");
    assert_eq!(t.link_at(0, 8).unwrap().kind(), 1);
    COUNT.with(|count| count.set(Some(0)));
    let hit = t.link_at(0, 8).unwrap();
    let requests = COUNT.with(|count| count.replace(None).unwrap());
    assert_eq!(hit.kind(), 1);
    assert_eq!(hit.url(), "https://example.test/path");
    eprintln!("warmed smart link lookup: {requests} allocation requests");
    assert!(requests <= 2, "link lookup allocated {requests} times");
}

#[test]
fn message_log_reply_allocation_cost() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut t = aterm_wasm::AtermTerminal::new(4, 80, font, 14.0, 0, 0, 0, 0).unwrap();
    for i in 0..32 {
        let response = t.notice(&format!("post perf synthetic log line {i} 漢🙂"));
        assert!(response.starts_with("OK "), "{response}");
    }
    let expected = t.messages_log_since(0.0);
    assert!(expected.lines().count() > 32, "at least32encodedrecords");
    COUNT.with(|count| count.set(Some(0)));
    let output = t.messages_log_since(0.0);
    let requests = COUNT.with(|count| count.replace(None).unwrap());
    assert_eq!(output, expected);
    eprintln!(
        "message log reply: {requests} allocation requests, {} output bytes",
        output.len()
    );
    assert!(requests <= 16, "log reply allocated {requests} times");
}

fn selector_terminal() -> aterm_wasm::AtermTerminal {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    aterm_wasm::AtermTerminal::new(4, 80, font, 14.0, 0, 0, 0, 0).unwrap()
}

#[test]
fn theme_selector_explicit_install_cost() {
    let mut t = selector_terminal();
    let expected = t.set_theme_by_name("Nord").unwrap();
    COUNT.with(|count| count.set(Some(0)));
    let output = t.set_theme_by_name("  Nord  ").unwrap();
    let requests = COUNT.with(|count| count.replace(None).unwrap());
    assert_eq!(output, expected);
    assert_eq!(
        output,
        r#"{"selector":"Nord","appearance":"dark","id":"Nord","side":"dark","reported":"dark","source":"builtin"}"#
    );
    eprintln!(
        "theme selector explicit: {requests} allocation requests, {} output bytes",
        output.len()
    );
    assert!(
        requests <= 6,
        "theme selector explicit allocated {requests} times"
    );
}

#[test]
fn theme_selector_appearance_refresh_cost() {
    let mut t = selector_terminal();
    t.set_theme_by_name("dark:Gruvbox Dark,light:Gruvbox Light")
        .unwrap();
    t.set_color_scheme(false);
    let expected = t.theme_state();
    t.set_color_scheme(true);
    COUNT.with(|count| count.set(Some(0)));
    t.set_color_scheme(false);
    let requests = COUNT.with(|count| count.replace(None).unwrap());
    let output = t.theme_state();
    assert_eq!(output, expected);
    assert_eq!(
        output,
        r#"{"selector":"dark:Gruvbox Dark,light:Gruvbox Light","appearance":"light","id":"Gruvbox Light","side":"light","reported":"light","source":"builtin"}"#
    );
    eprintln!(
        "theme selector appearance: {requests} allocation requests, {} output bytes",
        output.len()
    );
    assert!(
        requests <= 5,
        "theme selector appearance allocated {requests} times"
    );
}

#[test]
fn theme_selector_active_registration_refresh_cost() {
    const CONF: &str = "name = Custom\nbackground = #101820\nforeground = #e0e0e0\n";
    let mut t = selector_terminal();
    let expected_reply = t.register_theme(CONF).unwrap();
    t.set_theme_by_name("Custom").unwrap();
    let expected_state = t.theme_state();
    COUNT.with(|count| count.set(Some(0)));
    let output = t.register_theme(CONF).unwrap();
    let requests = COUNT.with(|count| count.replace(None).unwrap());
    assert_eq!(output, expected_reply);
    assert_eq!(t.theme_state(), expected_state);
    assert_eq!(
        output,
        r#"{"id":"Custom","appearance":"dark","appearance_source":"measured","warnings":[]}"#
    );
    eprintln!(
        "theme selector registration: {requests} allocation requests, {} output bytes",
        output.len()
    );
    assert!(
        requests <= 29,
        "theme selector registration allocated {requests} times"
    );
}

#[test]
fn theme_catalog_reservation_avoids_repeated_growth() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    for (registered, max_requests, max_bytes, capacity) in [
        (0, 2, 5864, 4096),
        (1, 3, 14056, 8192),
        (128, 5, 63208, 32768),
    ] {
        let mut t = aterm_wasm::AtermTerminal::new(4, 80, font, 14.0, 0, 0, 0, 0).unwrap();
        for i in 0..registered {
            t.register_theme(&format!(
                "name = Custom{i}\nbackground = #101820\nforeground = #e0e0e0\n"
            ))
            .unwrap();
        }
        COUNT.with(|count| count.set(Some(0)));
        REQUEST_BYTES.with(|bytes| bytes.set(Some(0)));
        let catalog = t.theme_catalog();
        let requests = COUNT.with(|count| count.replace(None).unwrap());
        let bytes = REQUEST_BYTES.with(|bytes| bytes.replace(None).unwrap());
        eprintln!("CATALOG_PROFILE registered={registered} requests={requests} requested_bytes={bytes} output_bytes={} capacity={}", catalog.len(), catalog.capacity());
        if std::env::var_os("ATERM_CATALOG_PROFILE").is_some() {
            println!("CATALOG {registered} {catalog}");
        }
        assert!(
            requests <= max_requests,
            "{registered} registered: {requests} allocation requests"
        );
        assert!(
            bytes <= max_bytes,
            "{registered} registered: {bytes} requested bytes"
        );
        assert_eq!(
            catalog.capacity(),
            capacity,
            "{registered} registered: retained capacity"
        );
    }
}

#[test]
fn tier_capability_json_only_allocates_its_output() {
    COUNT.with(|count| count.set(Some(0)));
    REQUEST_BYTES.with(|bytes| bytes.set(Some(0)));
    let json = aterm_wasm::AtermTerminal::tier_capabilities_json();
    let requests = COUNT.with(|count| count.replace(None).unwrap());
    let bytes = REQUEST_BYTES.with(|bytes| bytes.replace(None).unwrap());
    eprintln!("TIER_PROFILE requests={requests} requested_bytes={bytes} output_bytes={} capacity={} json={json}", json.len(), json.capacity());
    assert_eq!(requests, 1);
    assert_eq!(bytes, json.len());
    assert_eq!(json.capacity(), json.len());
}
