// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::terminal::Terminal;
use aterm_policy::{engine::PolicyEngine, profiles};

#[test]
fn sanitize_title_passes_legitimate_text_through() {
    for text in [
        "Hello World",
        "Col1\tCol2",
        "\u{65E5}\u{672C}\u{8A9E}",
        "\u{0627}\u{0644}\u{0639}",
        "\u{05D0}\u{05D1}\u{05D2}",
        "a\u{2029}b",
        "a\u{202F}b",
        "a\u{2065}b",
        "a\u{206A}b",
    ] {
        assert_eq!(sanitize_title(text), text, "{text:?}");
    }
}

#[test]
fn sanitize_title_strips_c0_and_c1_controls() {
    assert_eq!(sanitize_title("a\x00b\x01c"), "abc");
    assert_eq!(sanitize_title("a\x1bESCb"), "aESCb");
    assert_eq!(sanitize_title("a\x0Ab"), "ab");
    assert_eq!(sanitize_title("a\x0Db"), "ab");
    assert_eq!(sanitize_title("a\u{0080}b"), "ab");
    assert_eq!(sanitize_title("a\u{009B}31mb"), "a31mb");
    assert_eq!(sanitize_title("a\u{009F}b"), "ab");
}

#[test]
fn osc_0_title_strips_all_nine_bidi_overrides() {
    let mut term = Terminal::new(24, 80);
    let payload = "X\u{202A}\u{202B}\u{202C}\u{202D}\u{202E}\u{2066}\u{2067}\u{2068}\u{2069}Y";
    let seq = format!("\x1b]0;{payload}\x07");
    term.process(seq.as_bytes());
    assert_eq!(term.title(), "XY", "all 9 BiDi overrides must be stripped");
    assert_eq!(
        term.icon_name(),
        "XY",
        "OSC 0 icon name must also be sanitized"
    );
}

#[test]
fn title_epoch_bumps_on_real_change_not_on_noop() {
    let mut term = Terminal::new(24, 80);
    let start = term.title_epoch();

    term.process(b"\x1b]2;first\x07");
    assert_eq!(term.title(), "first");
    let after_first = term.title_epoch();
    assert!(
        after_first > start,
        "title_epoch must increase on a real title change ({start} -> {after_first})"
    );

    term.process(b"\x1b]2;first\x07");
    assert_eq!(
        term.title_epoch(),
        after_first,
        "title_epoch must NOT advance when the title value is unchanged"
    );

    term.process(b"\x1b]2;second\x07");
    assert_eq!(term.title(), "second");
    assert!(
        term.title_epoch() > after_first,
        "title_epoch must increase when the title value actually changes"
    );

    let before_api = term.title_epoch();
    term.set_title("second");
    assert_eq!(term.title_epoch(), before_api);
    term.set_title("third");
    assert!(term.title_epoch() > before_api);
}

#[test]
fn title_epoch_bumps_on_real_cwd_change_not_on_reprompt() {
    let mut term = Terminal::new(24, 80);
    let start = term.title_epoch();

    term.process(b"\x1b]7;file://localhost/tmp/one\x07");
    assert_eq!(term.current_working_directory(), Some("/tmp/one"));
    let after_one = term.title_epoch();
    assert!(
        after_one > start,
        "title_epoch must increase on a real cwd change ({start} -> {after_one})"
    );

    term.process(b"\x1b]7;file://localhost/tmp/one\x07");
    assert_eq!(
        term.title_epoch(),
        after_one,
        "title_epoch must NOT advance when the reported cwd is unchanged"
    );

    term.process(b"\x1b]7;file://localhost/tmp/two\x07");
    assert_eq!(term.current_working_directory(), Some("/tmp/two"));
    let after_two = term.title_epoch();
    assert!(
        after_two > after_one,
        "title_epoch must increase when the reported cwd actually changes"
    );

    term.process(b"\x1b]7;\x07");
    assert_eq!(term.current_working_directory(), None);
    assert!(
        term.title_epoch() > after_two,
        "title_epoch must increase when the cwd is cleared"
    );
}

fn assert_host_only_cwd_change_is_observable(report: &[u8], event: Option<&str>) {
    let mut term = Terminal::new(4, 20);
    term.set_osc_event_polling(true);
    term.process(b"\x1b]2;fixed title\x07source text\r\nkept row\x1b[2;5H");
    let remote = b"\x1b]7;file://build-box.example/c/work\x07";
    term.process(remote);
    assert_eq!(term.current_working_directory(), Some("/c/work"));
    assert_eq!(term.reported_cwd_host(), Some("build-box.example"));
    assert_eq!(term.reported_cwd_seq(), Some(1));
    assert_eq!(
        term.take_osc_event(),
        Some((7, "//build-box.example/c/work".into()))
    );
    assert_eq!(term.take_osc_event(), None);
    let epoch = term.title_epoch();
    term.process(remote);
    assert_eq!(term.title_epoch(), epoch, "the same owner is a no-op");
    assert_eq!(term.reported_cwd_seq(), Some(2));
    assert_eq!(
        term.take_osc_event(),
        Some((7, "//build-box.example/c/work".into()))
    );
    term.process(b"\x1b]7;file://elsewhere/c/wo%00rk\x07");
    assert_eq!(term.current_working_directory(), Some("/c/work"));
    assert_eq!(term.reported_cwd_host(), Some("build-box.example"));
    assert_eq!(
        term.reported_cwd_seq(),
        Some(2),
        "the invalid report was refused"
    );
    assert_eq!(term.take_osc_event(), None);
    assert_eq!(
        term.title_epoch(),
        epoch,
        "a refusal moves no observer clock"
    );
    let grid = (
        term.rows(),
        term.cols(),
        term.cursor(),
        term.visible_content(),
        term.content_seq(),
    );

    term.process(report);

    assert_eq!(term.title(), "fixed title");
    assert_eq!(term.current_working_directory(), Some("/c/work"));
    assert_eq!(term.reported_cwd_host(), None, "the new report is local");
    assert_eq!(term.reported_cwd_seq(), Some(3));
    assert_eq!(term.take_osc_event(), event.map(|path| (7, path.into())));
    assert_eq!(term.take_osc_event(), None);
    let changed_epoch = term.title_epoch();
    for (index, local) in [
        "file:///c/work",
        "file://localhost/c/work",
        "file://LocalHost/c/work",
    ]
    .into_iter()
    .enumerate()
    {
        term.process(format!("\x1b]7;{local}\x07").as_bytes());
        assert_eq!(term.current_working_directory(), Some("/c/work"));
        assert_eq!(term.reported_cwd_host(), None);
        assert_eq!(term.reported_cwd_seq(), Some(4 + index as u64));
        assert_eq!(term.take_osc_event(), Some((7, "/c/work".into())));
        assert_eq!(term.take_osc_event(), None);
        assert_eq!(
            term.title_epoch(),
            changed_epoch,
            "normalized local owner unchanged"
        );
    }
    assert_eq!(term.title(), "fixed title");
    assert_eq!(
        (
            term.rows(),
            term.cols(),
            term.cursor(),
            term.visible_content(),
            term.content_seq()
        ),
        grid,
        "CWD ownership changes neither grid, cursor nor content"
    );
    assert!(
        changed_epoch > epoch,
        "a new CWD host must be visible to title-epoch observers even when the raw path and title stay the same"
    );
}

#[test]
fn title_epoch_observes_an_osc_7_host_change_with_the_same_path() {
    assert_host_only_cwd_change_is_observable(
        b"\x1b]7;file://localhost/c/work\x07",
        Some("/c/work"),
    );
}

#[test]
fn title_epoch_observes_an_osc_633_host_reset_with_the_same_path() {
    assert_host_only_cwd_change_is_observable(b"\x1b]633;P;Cwd=/c/work\x07", None);
}

#[test]
fn osc_0_invalid_utf8_title_is_lossy_decoded() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b]0;Title\x9cMore\x07");
    assert_eq!(
        term.title(),
        "Title\u{FFFD}More",
        "invalid UTF-8 title payload must be lossily decoded"
    );
    assert_eq!(
        term.icon_name(),
        "Title\u{FFFD}More",
        "invalid UTF-8 icon payload must be lossily decoded"
    );

    term.process(b"\x1b]0;Recovery\x07");
    assert_eq!(term.title(), "Recovery");
    assert_eq!(term.icon_name(), "Recovery");
}

#[test]
fn osc_8_url_with_each_of_nine_bidi_overrides_rejected() {
    let codepoints = [
        '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}',
        '\u{2068}', '\u{2069}',
    ];
    for cp in codepoints {
        let mut term = Terminal::new(24, 80);
        let url = format!("https://example.com/{cp}path");
        let seq = format!("\x1b]8;;{url}\x07");
        term.process(seq.as_bytes());
        assert!(
            term.current_hyperlink().is_none(),
            "OSC 8 URL containing U+{:04X} must be rejected",
            cp as u32
        );
    }
}

#[test]
fn osc_8_refused_uri_ends_the_open_link() {
    let big = format!("https://a.example/{}", "a".repeat(MAX_HYPERLINK_URL_BYTES));
    for refused in [
        "file:///etc/passwd",
        "http://attacker\u{202E}example.com",
        &big,
    ] {
        let mut term = Terminal::new(24, 80);
        let seq = format!("\x1b]8;;https://a.example/\x1b\\A\x1b]8;;{refused}\x1b\\B");
        term.process(seq.as_bytes());
        assert_eq!(
            term.hyperlink_at_visible(0, 0).as_deref(),
            Some("https://a.example/")
        );
        assert_eq!(term.hyperlink_at_visible(0, 1), None, "{refused:.40}");
    }
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b]8;id=first:id=second;https://a.example/\x07");
    assert_eq!(
        term.transient
            .current_hyperlink
            .as_ref()
            .and_then(|link| link.id.as_deref()),
        Some("second")
    );
    term.process(b"\x1b]8;id=first:id=;https://a.example/\x07");
    assert!(
        term.transient
            .current_hyperlink
            .as_ref()
            .unwrap()
            .id
            .is_none()
    );
}

#[test]
fn shared_link_payload_keeps_cells_and_later_writes_independent() {
    let mut term = Terminal::new(2, 20);
    term.process(b"\x1b]8;id=original;https://example.com\x1b\\A");
    term.process("日".as_bytes());
    term.process(b"B");
    let first = term.grid_mut().cell_extra_mut(0, 0);
    first.set_hyperlink(Some(std::sync::Arc::from("https://edited.example")));
    first.set_hyperlink_id(Some(std::sync::Arc::from("edited")));
    term.process(b"C\x1b]8;;\x1b\\x\x1b]8;id=new;https://example.com\x1b\\D");
    for (col, url, id) in [
        (0, Some("https://edited.example"), Some("edited")),
        (1, Some("https://example.com"), Some("original")),
        (2, Some("https://example.com"), Some("original")),
        (3, Some("https://example.com"), Some("original")),
        (4, Some("https://example.com"), Some("original")),
        (5, None, None),
        (6, Some("https://example.com"), Some("new")),
    ] {
        let extra = term.grid().cell_extra(0, col);
        assert_eq!(
            extra.and_then(|extra| extra.hyperlink()).map(AsRef::as_ref),
            url,
            "column {col}"
        );
        assert_eq!(
            extra
                .and_then(|extra| extra.hyperlink_id())
                .map(AsRef::as_ref),
            id,
            "column {col}"
        );
    }
    assert_eq!(
        term.current_hyperlink().map(AsRef::as_ref),
        Some("https://example.com")
    );
}

#[test]
fn osc_7_keeps_the_host_it_names_beside_the_path() {
    let mut term = Terminal::new(24, 80);
    assert_eq!(term.reported_cwd_host(), None, "no report, no host");
    term.process(b"\x1b]7;file://build-box.example/srv/app\x07");
    assert_eq!(term.current_working_directory(), Some("/srv/app"));
    assert_eq!(term.reported_cwd_host(), Some("build-box.example"));
    for local in [
        &b"\x1b]7;file:///srv/app\x07"[..],
        b"\x1b]7;file://localhost/srv/app\x07",
        b"\x1b]7;file://LocalHost/srv/app\x07",
    ] {
        term.process(b"\x1b]7;file://build-box.example/srv/app\x07");
        term.process(local);
        assert_eq!(term.current_working_directory(), Some("/srv/app"));
        assert_eq!(term.reported_cwd_host(), None, "{local:?}");
    }
    term.process(b"\x1b]7;file://build-box.example/srv/app\x07");
    term.process(b"\x1b]7;file://elsewhere/tmp/a%00b\x07");
    assert_eq!(term.current_working_directory(), Some("/srv/app"));
    assert_eq!(term.reported_cwd_host(), Some("build-box.example"));
    term.process(b"\x1bc");
    assert_eq!(term.current_working_directory(), Some("/srv/app"));
    assert_eq!(term.reported_cwd_host(), Some("build-box.example"));
    term.process(b"\x1b]633;P;Cwd=/home/me\x07");
    assert_eq!(term.current_working_directory(), Some("/home/me"));
    assert_eq!(term.reported_cwd_host(), None);
    term.process(b"\x1b]7;file://build-box.example/srv/app\x07");
    term.process(b"\x1b]7;\x07");
    assert_eq!(term.current_working_directory(), None);
    assert_eq!(term.reported_cwd_host(), None);
    term.process(b"\x1b]7;file://build-box.example/srv/app\x07");
    let restored = Terminal::from_checkpoint(&term.checkpoint());
    assert_eq!(restored.current_working_directory(), Some("/srv/app"));
    assert_eq!(restored.reported_cwd_host(), None);
}

#[test]
fn every_stored_cwd_report_has_its_own_number() {
    let mut term = Terminal::new(24, 80);
    assert_eq!(term.reported_cwd_seq(), Some(0), "no report yet");
    let remote = b"\x1b]7;file://build-box.example/srv/app\x07";
    term.process(remote);
    assert_eq!(term.reported_cwd_seq(), Some(1));
    term.process(remote);
    assert_eq!(term.current_working_directory(), Some("/srv/app"));
    assert_eq!(term.reported_cwd_host(), Some("build-box.example"));
    assert_eq!(term.reported_cwd_seq(), Some(2), "the same report again");
    term.process(b"\x1b]7;file://elsewhere/tmp/a%00b\x07");
    assert_eq!(term.reported_cwd_seq(), Some(2), "a refused report");
    term.process(b"\x1b]633;P;Cwd=/home/me\x07");
    assert_eq!(term.reported_cwd_seq(), Some(3), "the shell's own word");
    term.process(b"\x1bc");
    assert_eq!(term.reported_cwd_seq(), Some(3), "a RIS keeps the count");
    let mut restored = Terminal::from_checkpoint(&term.checkpoint());
    assert_eq!(restored.current_working_directory(), Some("/home/me"));
    assert_eq!(restored.reported_cwd_seq(), None);
    restored.process(remote);
    assert_eq!(restored.reported_cwd_seq(), Some(1));
    let carry = Terminal::new(24, 80).checkpoint();
    term.restore_checkpoint(&carry);
    assert_eq!(term.reported_cwd_seq(), None);
    term.process(remote);
    assert_eq!(term.reported_cwd_seq(), Some(4));
}

#[test]
fn osc_7_oversized_cwd_path_not_queued() {
    let mut term = Terminal::new(24, 80);
    term.set_osc_event_polling(true);

    term.process(b"\x1b]7;file:///home/user\x07");
    assert_eq!(term.current_working_directory(), Some("/home/user"));
    assert_eq!(
        term.take_osc_event(),
        Some((7, "/home/user".to_string())),
        "a legitimate OSC 7 cwd must still be queued for poll-based hosts"
    );

    let big = "a".repeat(MAX_CWD_PATH_BYTES + 1);
    let seq = format!("\x1b]7;file:///{big}\x07");
    term.process(seq.as_bytes());
    assert_eq!(
        term.current_working_directory(),
        Some("/home/user"),
        "oversized OSC 7 path must not overwrite the prior cwd"
    );
    assert!(
        term.take_osc_event().is_none(),
        "oversized OSC 7 path must not be queued"
    );
}

#[test]
fn osc_633_p_oversized_cwd_rejected() {
    let mut term = Terminal::new(24, 80);

    term.process(b"\x1b]633;P;Cwd=/home/user\x07");
    assert_eq!(term.current_working_directory(), Some("/home/user"));

    let big = "a".repeat(MAX_CWD_PATH_BYTES + 1);
    let seq = format!("\x1b]633;P;Cwd={big}\x07");
    term.process(seq.as_bytes());
    assert_eq!(
        term.current_working_directory(),
        Some("/home/user"),
        "oversized OSC 633 P Cwd must not overwrite the prior cwd"
    );
}

#[test]
fn osc_7_cwd_with_a_nul_byte_is_ignored() {
    let mut term = Terminal::new(24, 80);
    term.set_osc_event_polling(true);
    term.process(b"\x1b]7;file:///home/user\x07");
    assert_eq!(term.current_working_directory(), Some("/home/user"));
    assert!(
        term.take_osc_event().is_some(),
        "the baseline report queues"
    );
    let epoch = term.title_epoch();

    term.process(b"\x1b]7;file:///tmp/a%00b\x07");
    assert_eq!(
        term.current_working_directory(),
        Some("/home/user"),
        "a cwd containing NUL must not overwrite the prior cwd"
    );
    assert!(
        term.take_osc_event().is_none(),
        "a cwd the engine refuses to store is not announced either"
    );
    assert_eq!(
        term.title_epoch(),
        epoch,
        "an ignored report is not a tab-label change"
    );

    let mut fresh = Terminal::new(24, 80);
    fresh.process(b"\x1b]7;file:///tmp/a%00b\x07");
    assert_eq!(fresh.current_working_directory(), None);
}

#[test]
fn osc_633_e_oversized_commandline_rejected() {
    let mut term = Terminal::new(24, 80);

    term.process(b"\x1b]633;A\x07");

    term.process(b"\x1b]633;E;ls -la\x07");
    assert_eq!(
        term.current_block().and_then(|b| b.commandline.as_deref()),
        Some("ls -la"),
        "a normal OSC 633 E command line is stored on the block"
    );

    let big = "a".repeat(crate::terminal::MAX_COMMANDLINE_BYTES + 1);
    let seq = format!("\x1b]633;E;{big}\x07");
    term.process(seq.as_bytes());
    assert_eq!(
        term.current_block().and_then(|b| b.commandline.as_deref()),
        Some("ls -la"),
        "oversized OSC 633 E command line must not overwrite the prior one"
    );
}

#[test]
fn osc_8_refuses_oversized_and_control_char_uris() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b]8;;https://example.com/path\x07");
    assert_eq!(
        term.current_hyperlink().map(|s| s.as_ref()),
        Some("https://example.com/path"),
        "a valid OSC 8 hyperlink is accepted"
    );

    let big = "a".repeat(MAX_HYPERLINK_URL_BYTES + 1);
    let seq = format!("\x1b]8;;https://example.com/{big}\x07");
    term.process(seq.as_bytes());
    assert!(
        term.current_hyperlink().is_none(),
        "oversized OSC 8 URI must be rejected"
    );

    let mut term = Terminal::new(24, 80);
    term.process("\x1b]8;;https://example.com/\u{7f}x\x07".as_bytes());
    assert!(
        term.current_hyperlink().is_none(),
        "OSC 8 URI containing a control char must be rejected"
    );
}

#[test]
fn osc_8_custom_scheme_rejected_without_host_mint() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b]8;;orca://focus/w1\x07");
    assert!(
        term.current_hyperlink().is_none(),
        "an unminted custom scheme must be rejected at parse time"
    );
}

#[test]
fn osc_8_custom_scheme_accepted_after_authorize_hyperlink_scheme() {
    let mut term = Terminal::new(24, 80);
    assert!(term.authorize_hyperlink_scheme("orca"));
    term.process(b"\x1b]8;;orca://focus/w1\x07");
    assert_eq!(
        term.current_hyperlink().map(|s| s.as_ref()),
        Some("orca://focus/w1"),
        "a minted scheme must be accepted"
    );

    term.process(b"\x1b]8;;\x07");
    term.process(b"\x1b]8;;ORCA://focus/w2\x07");
    assert_eq!(
        term.current_hyperlink().map(|s| s.as_ref()),
        Some("ORCA://focus/w2"),
        "extra-scheme matching must be ASCII-case-insensitive"
    );

    term.process(b"\x1b]8;;\x07");
    term.process(b"\x1b]8;;orca://run/w1?cmd=a;b\x07");
    assert_eq!(
        term.current_hyperlink().map(|s| s.as_ref()),
        Some("orca://run/w1?cmd=a;b"),
        "semicolon reconstruction must apply to extra-scheme URIs too"
    );

    term.process(b"\x1b]8;;\x07");
    term.process(b"\x1b]8;;orca%3A://x\x07");
    assert!(
        term.current_hyperlink().is_none(),
        "a scheme with smuggled chars must fail the shared RFC 3986 shape walk"
    );

    term.process(b"\x1b]8;;\x07");
    term.revoke_hyperlink_scheme("orca");
    term.process(b"\x1b]8;;orca://focus/w3\x07");
    assert!(
        term.current_hyperlink().is_none(),
        "a revoked scheme must be refused again"
    );
}

#[test]
fn osc_8_extra_scheme_still_rejects_bidi_and_control_chars() {
    let mut term = Terminal::new(24, 80);
    assert!(term.authorize_hyperlink_scheme("orca"));
    term.process("\x1b]8;;orca://safe\u{202E}live\x07".as_bytes());
    assert!(
        term.current_hyperlink().is_none(),
        "BiDi override in an extra-scheme URI must still be rejected"
    );
    term.process("\x1b]8;;orca://x/\u{7f}y\x07".as_bytes());
    assert!(
        term.current_hyperlink().is_none(),
        "control char in an extra-scheme URI must still be rejected"
    );
}

#[test]
fn osc_8_extra_scheme_still_capped_by_max_url_bytes() {
    let mut term = Terminal::new(24, 80);
    assert!(term.authorize_hyperlink_scheme("orca"));
    let big = "a".repeat(MAX_HYPERLINK_URL_BYTES + 1);
    let seq = format!("\x1b]8;;orca://x/{big}\x07");
    term.process(seq.as_bytes());
    assert!(
        term.current_hyperlink().is_none(),
        "the byte cap must apply to extra-scheme URIs unchanged"
    );
}

#[test]
fn reset_clears_extra_schemes_is_a_host_choice() {
    let mut term = Terminal::new(24, 80);
    assert!(term.authorize_hyperlink_scheme("orca"));
    term.process(b"\x1b[!p");
    term.process(b"\x1bc");
    assert!(
        term.is_hyperlink_scheme_authorized("orca"),
        "extra schemes survive app-triggered resets (host-owned state)"
    );
    term.process(b"\x1b]8;;orca://focus/w1\x07");
    assert!(
        term.current_hyperlink().is_some(),
        "the minted scheme still accepts after reset"
    );
    let fresh = Terminal::new(24, 80);
    assert_eq!(fresh.hyperlink_extra_scheme_count(), 0);
}

#[test]
fn osc_52_standard_policy_wildcard_does_not_overgrant_revoked_set() {
    use crate::terminal::ClipboardAccess;
    use crate::terminal::ClipboardOperation;
    use std::sync::{Arc, Mutex};

    let mut term = Terminal::new(24, 80);
    term.authorize_clipboard_access(ClipboardAccess::Write);
    let captured = Arc::new(Mutex::new(None::<String>));
    let captured_clone = Arc::clone(&captured);
    term.set_clipboard_callback(move |op| {
        if let ClipboardOperation::Set { content, .. } = op {
            *captured_clone.lock().expect("poisoned") = Some(content);
        }
        None
    });
    term.process(b"\x1b]52;c;SGVsbG8=\x07");
    assert_eq!(
        captured.lock().expect("poisoned").take().as_deref(),
        Some("Hello"),
        "authorized Write works before revocation"
    );

    term.revoke_clipboard_access(ClipboardAccess::Write);
    term.apply_policy_engine(PolicyEngine::new(profiles::standard()));

    term.process(b"\x1b]52;c;SGVsbG8=\x07");

    assert_eq!(*captured.lock().expect("poisoned"), None);
}

#[test]
fn osc52_query_answers_every_clipboard_it_admits() {
    for len in [
        1_024,
        49_144,
        49_145,
        64 * 1024 - 1,
        super::super::MAX_OSC52_QUERY_RESPONSE_BYTES,
    ] {
        let mut term = Terminal::new(24, 80);
        term.authorize_clipboard_access(crate::terminal::clipboard_auth::ClipboardAccess::Query);
        term.set_clipboard_callback(move |op| match op {
            crate::terminal::ClipboardOperation::Query { .. } => Some("x".repeat(len)),
            _ => None,
        });
        term.apply_policy_engine(PolicyEngine::new(profiles::standard()));

        term.process(b"\x1b]52;c;?\x07");

        let response = term.take_response().unwrap_or_else(|| {
            panic!(
                "a {len}-byte clipboard is within the handler's own \
                 {}-byte cap, so it must be ANSWERED, not silently dropped",
                super::super::MAX_OSC52_QUERY_RESPONSE_BYTES
            )
        });
        assert!(
            response.starts_with(b"\x1b]52;c;"),
            "the answer is an OSC 52 reply"
        );
    }
}

#[test]
fn osc52_query_still_refuses_a_clipboard_over_the_cap() {
    let mut term = Terminal::new(24, 80);
    term.authorize_clipboard_access(crate::terminal::clipboard_auth::ClipboardAccess::Query);
    let len = super::super::MAX_OSC52_QUERY_RESPONSE_BYTES + 1;
    term.set_clipboard_callback(move |op| match op {
        crate::terminal::ClipboardOperation::Query { .. } => Some("x".repeat(len)),
        _ => None,
    });
    term.apply_policy_engine(PolicyEngine::new(profiles::standard()));

    term.process(b"\x1b]52;c;?\x07");

    assert!(
        term.take_response().is_none(),
        "a clipboard past the cap is refused"
    );
}

#[test]
fn osc_52_standard_policy_wildcard_does_not_overgrant_revoked_query() {
    use crate::terminal::ClipboardAccess;

    let mut term = Terminal::new(24, 80);
    term.authorize_clipboard_access(ClipboardAccess::Query);
    term.set_clipboard_callback(|op| match op {
        crate::terminal::ClipboardOperation::Query { .. } => Some("secret".to_string()),
        _ => None,
    });
    term.process(b"\x1b]52;c;?\x07");
    assert!(
        term.take_response().is_some(),
        "authorized Query answers before revocation"
    );

    term.revoke_clipboard_access(ClipboardAccess::Query);
    term.apply_policy_engine(PolicyEngine::new(profiles::standard()));

    term.process(b"\x1b]52;c;?\x07");

    assert!(
        term.take_response().is_none(),
        "standard wildcard Execute must not reopen OSC 52 query"
    );
}

#[test]
fn osc_52_set_pre_decode_bound_rejects_oversized_without_decoding() {
    use crate::terminal::ClipboardAccess;
    use crate::terminal::ClipboardOperation;
    use std::sync::{Arc, Mutex};

    let mut term = Terminal::new(24, 80);
    term.set_osc_event_polling(true);
    term.authorize_clipboard_access(ClipboardAccess::Write);
    let captured = Arc::new(Mutex::new(None::<String>));
    let captured_clone = Arc::clone(&captured);
    term.set_clipboard_callback(move |op| {
        if let ClipboardOperation::Set { content, .. } = op {
            *captured_clone.lock().expect("poisoned") = Some(content);
        }
        None
    });

    let threshold = MAX_OSC52_QUERY_RESPONSE_BYTES / 3 * 4 + 4;
    let oversized = "A".repeat(threshold + 4);
    assert!(oversized.len() > threshold);
    assert_eq!(oversized.len() % 4, 0, "valid base64 length (no padding)");

    let seq = format!("\x1b]52;c;{oversized}\x07");
    term.process(seq.as_bytes());

    assert_eq!(
        *captured.lock().expect("poisoned"),
        None,
        "over-cap encoded payload must be rejected pre-decode"
    );
    assert!(
        term.take_osc_event().is_none(),
        "over-cap OSC 52 set must not queue a decoded clipboard event"
    );
}

#[test]
fn osc_52_set_pre_decode_bound_passes_exact_cap_payload() {
    use crate::terminal::ClipboardAccess;

    let mut term = Terminal::new(24, 80);
    term.set_osc_event_polling(true);
    term.authorize_clipboard_access(ClipboardAccess::Write);

    let content = vec![0u8; MAX_OSC52_QUERY_RESPONSE_BYTES];
    let encoded = base64::encode(&content).expect("encode cap-sized payload");
    assert_eq!(encoded.len(), MAX_OSC52_QUERY_RESPONSE_BYTES / 3 * 4 + 4);

    let seq = format!("\x1b]52;c;{encoded}\x07");
    term.process(seq.as_bytes());

    let expected = String::from_utf8(content).expect("NUL bytes are valid UTF-8");
    assert_eq!(
        term.take_osc_event(),
        Some((52, expected)),
        "exactly-cap OSC 52 set must still reach the clipboard"
    );
}
