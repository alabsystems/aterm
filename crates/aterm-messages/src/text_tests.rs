// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The title form is `attention`'s rule, fault for fault and in its order;
/// a lone symbol spends no word, and the words check wins when both fail.
#[test]
fn glass_title_fault_is_the_attention_rule() {
    assert_eq!(glass_title_fault("Updating aterm"), None);
    assert_eq!(glass_title_fault("Settings \u{25b8} Messages & more"), None);
    assert_eq!(title_words("Settings \u{25b8} Messages & more"), 3);
    assert_eq!(
        title_words("Typing slowed by yes in tab 2"),
        6,
        "a number is no word"
    );
    assert_eq!(glass_title_fault("Typing slowed by yes in tab 2"), None);
    assert_eq!(
        glass_title_fault("one two three four five six seven"),
        Some("a glass title over six words")
    );
    assert_eq!(
        glass_title_fault(&"x".repeat(GLASS_TITLE_CHARS + 1)),
        Some("a glass title over 48 characters")
    );
    assert_eq!(glass_title_fault(&"x".repeat(GLASS_TITLE_CHARS)), None);
    for seam in ["a \u{2014} b", "a; b", "Build: failed"] {
        assert_eq!(
            glass_title_fault(seam),
            Some("a clause in a glass title"),
            "{seam}"
        );
    }
    assert_eq!(
        glass_title_fault("Build failed."),
        Some("a sentence for a glass title")
    );
    assert_eq!(
        glass_title_fault(&format!("{} a b c d e f", "y".repeat(60))),
        Some("a glass title over six words"),
        "words first"
    );
    assert_eq!(
        glass_title_fault(""),
        None,
        "emptiness is the parser's to refuse"
    );
}

/// Every piece within the cap, none empty, and rejoined at the joints the
/// cut used, the sanitized sentence itself; a message at the line cap
/// still round-trips the log codec.
#[test]
fn a_long_sentence_splits_at_its_joints_and_loses_nothing() {
    let clause = |n: usize| format!("clause {n} {}", "w".repeat(40));
    let long: String = (0..20).map(clause).collect::<Vec<_>>().join("; ");
    assert!(long.chars().count() > 900);
    let lines = split_sentence(&long, 240);
    assert!(lines.len() > 3, "{lines:?}");
    assert!(lines.iter().all(|l| !l.is_empty() && char_width(l) <= 240));
    assert_eq!(lines.join("; "), long);
    let run: String = (0..100)
        .map(|n| format!("word{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    let lines = split_sentence(&run, 80);
    assert!(lines.iter().all(|l| char_width(l) <= 80), "{lines:?}");
    assert_eq!(lines.join(" "), run);
    let hard = "x".repeat(500);
    let lines = split_sentence(&hard, 240);
    assert_eq!(lines.concat(), hard, "no joint: a hard cut, nothing lost");
    assert_eq!(split_sentence("short", 240), vec!["short"]);
    assert!(split_sentence("", 240).is_empty());
    let dots = format!("{}. {}", "a".repeat(30), "b".repeat(30));
    assert_eq!(
        split_sentence(&dots, 40),
        vec![format!("{}.", "a".repeat(30)), "b".repeat(30)]
    );
    assert_eq!(split_sentence("a\u{1b}[31mb", 240), vec!["a[31mb"]);
    let huge: String = (0..400_000)
        .map(|n| format!("w{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(huge.len() > 3_000_000);
    let lines = split_sentence(&huge, 240);
    assert!(lines.len() > 10_000, "{}", lines.len());
    assert!(lines.iter().all(|l| char_width(l) <= 240));
    assert_eq!(lines.join(" "), huge);
    let msg =
        crate::model::Message::new(crate::tags::UPDATE, crate::Severity::Warn, "t").sentence(&long);
    assert_eq!(msg.detail.join("; "), long);
}

const CRASH: &str = "crash log at ~/Library/Logs/aterm/crash-signal-8123-1758470000.log.seen";

/// A path atom is cut from the left and keeps its file name.
#[test]
fn a_path_is_cut_from_the_left_and_keeps_its_file_name() {
    assert_eq!(CRASH.chars().count(), 71);
    assert_eq!(shape_detail(CRASH, 71), CRASH);
    let at_54 = shape_detail(CRASH, 54);
    assert_eq!(
        at_54,
        "crash log at \u{2026}/crash-signal-8123-1758470000.log.seen"
    );
    assert_eq!(at_54.chars().count(), 52);
    assert_eq!(
        shape_detail(CRASH, 60),
        "crash log at \u{2026}/aterm/crash-signal-8123-1758470000.log.seen"
    );
    let at_40 = shape_detail(CRASH, 40);
    assert_eq!(at_40.chars().count(), 40);
    assert!(
        at_40.starts_with("crash log at \u{2026}") && at_40.ends_with(".log.seen"),
        "{at_40}"
    );
    assert_eq!(shape_detail(CRASH, 14), "crash log at\u{2026}");
    assert_eq!(shape_path("/a/b/c.log", 100), "/a/b/c.log");
    assert_eq!(shape_path("/a/b/c.log", 7), "\u{2026}/c.log");
    assert_eq!(shape_path("/a/b/c.log", 9), "\u{2026}/b/c.log");
    assert_eq!(shape_path("/a/b/c.log", 1), "\u{2026}");
    assert_eq!(shape_path("/a/b/c.log", 0), "");
    let prose = "skipping \"cmd+shift+k\": unknown action \"foo\"";
    assert_eq!(
        shape_detail(prose, 42),
        "skipping \"cmd+shift+k\": unknown action\u{2026}"
    );
    let staged = "build 1234 \u{2014} verified; auto-apply is off \u{2014} apply it";
    assert_eq!(
        shape_detail(staged, 26),
        "build 1234 \u{2014} verified;\u{2026}"
    );
    for cap in 0..80 {
        assert!(shape_detail(prose, cap).chars().count() <= cap, "cap {cap}");
        assert!(shape_detail(CRASH, cap).chars().count() <= cap, "cap {cap}");
    }
}

/// A detail of ` · ` pieces sheds its trailing pieces whole before it cuts
/// inside one; a cut detail keeps its command and the prose carries the
/// ellipsis.
#[test]
fn a_command_survives_the_cut() {
    let pointer = "undo: `aterm pkg doctor` prints the revert";
    let revert = "defaults -currentHost delete com.apple.universalcontrol Disable; \
                  defaults -currentHost delete com.apple.universalcontrol DisableMagicEdges";
    let detail = format!("{pointer} \u{00b7} {revert}");
    assert!(revert.chars().count() > 100);
    assert_eq!(shape_detail(&detail, 300), detail);
    let whole = detail.chars().count();
    for cap in [whole - 1, whole - 40, pointer.chars().count()] {
        assert_eq!(shape_detail(&detail, cap), pointer, "cap {cap}");
    }
    assert_eq!(
        shape_detail(&detail, 35),
        "`aterm pkg doctor` prints the\u{2026}"
    );
    assert_eq!(
        shape_detail(&detail, 30),
        "`aterm pkg doctor` prints the\u{2026}"
    );
    assert_eq!(shape_detail(&detail, 25), "`aterm pkg doctor`\u{2026}");
    let cut = shape_detail(&detail, 30);
    assert!(!cut.contains("defaults"), "{cut}");
    let managed =
        "what `claude` and `codex` run in every aterm tab, this one too \u{00b7} build 2026091001";
    let d = shape_detail(managed, 40);
    assert!(d.contains("`claude`") && d.contains('\u{2026}'), "{d}");
    assert!(d.chars().count() <= 40, "{d}");
    let run = "the shims are stale. Run `aterm pkg repair` to relay them \u{00b7} 3 tabs";
    let d = shape_detail(run, 40);
    assert!(d.contains("Run `aterm pkg repair`"), "{d}");
    let d = shape_detail("see `a very long command that goes on and on`", 12);
    assert!(d.starts_with("`a very") && d.ends_with('\u{2026}'), "{d}");
    assert_eq!(d.chars().count(), 12);
}

/// A head fragment shorter than two words or twelve cells is dropped whole
/// and the command stands alone; a clause survives with the ellipsis.
#[test]
fn a_stub_head_gives_way_to_the_command_whole() {
    let command = "`. ~/.aterm/shell.d/00-atpkg.zsh`";
    let installed = format!(
        "ay, trust \u{2014} ready in every tab opened since this update \u{00b7} \
         1 tab from before it picks them up with {command}"
    );
    let cmd_len = command.chars().count();
    assert_eq!(cmd_len, 33);
    for cap in cmd_len..cmd_len + 19 {
        assert_eq!(shape_detail(&installed, cap), command, "cap {cap}");
    }
    assert_eq!(
        shape_detail(&installed, cmd_len + 19),
        format!("1 tab from before\u{2026} {command}")
    );
    assert_eq!(
        shape_detail(&installed, cmd_len + 22),
        format!("1 tab from before it\u{2026} {command}")
    );
    let run = "the shims are stale. Run `aterm pkg repair` to relay them";
    assert_eq!(
        shape_detail(run, 40),
        "Run `aterm pkg repair` to relay them"
    );
    for cap in 0..100 {
        assert!(
            shape_detail(&installed, cap).chars().count() <= cap,
            "cap {cap}"
        );
    }
}

/// `is_hidden_char` spells out `char::is_control` (not a `const fn` on
/// stable Rust): every control character is hidden, at every code point.
#[test]
fn every_control_character_is_hidden() {
    for c in (0..=u32::from(char::MAX)).filter_map(char::from_u32) {
        if c.is_control() {
            assert!(is_hidden_char(c), "{c:?}");
        }
    }
}

/// Escape sequences, bells and other control characters never reach a
/// cell string.
#[test]
fn hostile_text_is_sanitized_before_it_becomes_a_message() {
    let bad = "bad\u{1b}[31m thing\u{7}\r\n\ttail";
    let clean = sanitize(bad, usize::MAX);
    assert_eq!(clean, "bad[31m thingtail");
    assert!(clean.chars().all(|c| !c.is_control()));
    assert_eq!(
        sanitize(
            "a\u{202e}b\u{200b}c\u{feff}d\u{2066}e\u{2028}f\u{200d}g\u{2029}h",
            100
        ),
        "abcdefgh",
        "bidi overrides, zero-width characters and line separators are stripped"
    );
    assert_eq!(
        sanitize(
            "a\u{034f}b\u{115f}c\u{3164}d\u{ffa0}e\u{180b}f\u{17b4}g\u{1bca0}h\u{1d173}i\u{e0000}j",
            100
        ),
        "abcdefghij",
        "every invisible character the metadata gate strips"
    );
    assert_eq!(
        sanitize("h\u{e9}llo \u{6f22}", 100),
        "h\u{e9}llo \u{6f22}",
        "printable non-ASCII stays"
    );
    assert_eq!(
        sanitize("abcdef", 3),
        "abc\u{2026}",
        "atpkg's cap-plus-ellipsis shape"
    );
    assert_eq!(clip("abcdef", 3), "ab\u{2026}", "clip fits INSIDE the cap");
    assert_eq!(clip("abc", 3), "abc");
    assert_eq!(truncate("abc", 0), "");
    assert_eq!(truncate("héllo wörld", 6), "héllo\u{2026}");
    assert_eq!(trim_words("a b c", 5), "a b c");
    assert_eq!(trim_words("aaaa bbbb", 6), "aaaa\u{2026}");
    assert_eq!(trim_words("aaaaaaaa", 4), "aaa\u{2026}");
    assert_eq!(trim_words("aaaa", 0), "");
}
