// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::vendor_drift::*;
#[cfg(unix)]
use crate::vendor_files::*;

const MACHO_HEADER: usize = 32;
const MACHO_SEGMENT: usize = 72;
const MACHO_SECTION: usize = 80;
const MACHO_TEXT: usize = MACHO_HEADER + MACHO_SEGMENT;
const MACHO_LAST: usize = MACHO_TEXT + MACHO_SEGMENT + 2 * MACHO_SECTION;
const MACHO_LAST_SIZE: usize = 24;

/// A thin 64-bit Mach-O laid out as a real build is: a segment of no sections (`__PAGEZERO`), a
/// segment of one code section and one data section, then a command the walk passes over. That
/// last command comes after the code, so a walk that trusted a bad one would still have blanked.
fn macho(code: &[u8], data: &[u8]) -> Vec<u8> {
    let code_at = MACHO_LAST + MACHO_LAST_SIZE;
    let data_at = code_at + code.len();
    let mut file = vec![0; data_at + data.len()];
    let mut put = |at: usize, value: usize| {
        let value = u32::try_from(value).unwrap();
        file[at..at + 4].copy_from_slice(&value.to_le_bytes());
    };
    put(0, 0xfeed_facf);
    put(16, 3);
    put(20, code_at - MACHO_HEADER);
    put(MACHO_HEADER, 0x19);
    put(MACHO_HEADER + 4, MACHO_SEGMENT);
    put(MACHO_TEXT, 0x19);
    put(MACHO_TEXT + 4, MACHO_LAST - MACHO_TEXT);
    put(MACHO_TEXT + 64, 2);
    put(MACHO_LAST, 0x1b);
    put(MACHO_LAST + 4, MACHO_LAST_SIZE);
    let sections = [(code_at, code.len(), 0x8000_0400), (data_at, data.len(), 0)];
    for (index, (at, size, flags)) in sections.into_iter().enumerate() {
        let section = MACHO_TEXT + MACHO_SEGMENT + index * MACHO_SECTION;
        put(section + 40, size);
        put(section + 48, at);
        put(section + 64, flags);
    }
    file[code_at..data_at].copy_from_slice(code);
    file[data_at..].copy_from_slice(data);
    file
}

/// Claude Code 2.1.316 and 2.1.317 passed `box.ide_diff.mark` on three bytes of ARM64 code.
#[test]
fn an_anchor_only_machine_code_holds_is_missing() {
    let table = [lit("glyph", "⧉", "2.1.280")];
    let mut build = macho("⧉".as_bytes(), b"no such text");
    assert!(missing(&build, &table).is_empty(), "the code matches whole");
    blank_code(&mut build);
    assert_eq!(missing(&build, &table), ["glyph"]);
}

/// Claude Code's bundle spells the same glyph `\u29C9` in every build.
#[test]
fn a_bundle_holds_a_glyph_as_its_source_spells_it() {
    let table = [lit("glyph", "a ⧉", "2.1.280")];
    let mut build = macho(b"", br"Opened changes in a \u29C9");
    blank_code(&mut build);
    assert!(missing(&build, &table).is_empty());
    assert_eq!(missing(br"a \u29C8", &table), ["glyph"]);
}

/// Every part of every anchor is answered in turn, so one anchor's missing part cannot shift
/// the next anchor's answer.
#[test]
fn each_anchor_answers_for_all_its_parts() {
    let table = [
        lit("first", "aaaa · bbbb", "2.1.280"),
        lit("second", "cccc", "2.1.280"),
        lit("third", "dddd · ↑ eeee", "2.1.280"),
    ];
    assert_eq!(missing(b"aaaa cccc", &table), ["first", "third"]);
    assert_eq!(
        missing(br"bbbb dddd \u2191 eeee", &table),
        ["first", "second"]
    );
    assert!(missing(br"aaaa bbbb cccc dddd \u2191 eeee", &table).is_empty());
}

#[test]
fn a_character_is_escaped_as_a_bundle_spells_it() {
    assert_eq!(js_escaped("plain"), "plain");
    assert_eq!(js_escaped("a·b"), r"a\xB7b");
    assert_eq!(js_escaped("↑/↓"), r"\u2191/\u2193");
    assert_eq!(js_escaped("😀"), r"\uD83D\uDE00");
}

/// A build loses its code and keeps its data; a file this walk cannot follow keeps every byte.
#[test]
fn only_a_mach_o_the_walk_can_follow_loses_its_code() {
    // Long enough that a section read past its command lands in the file, not off its end.
    let data = [b"data".as_slice(), &[0; 128]].concat();
    let whole = macho(b"code", &data);
    let mut blanked = whole.clone();
    blank_code(&mut blanked);
    assert!(!contains(&blanked, "code"), "the control: the code goes");
    assert!(contains(&blanked, "data"), "the control: the data stays");
    let code_section = MACHO_TEXT + MACHO_SEGMENT;
    let past_the_file = u32::try_from(whole.len()).unwrap();
    for (what, at, value) in [
        ("a 32-bit magic", 0, 0xfeed_face),
        ("one command too many", 16, 4),
        ("every command at once", 16, u32::MAX),
        ("commands past the file", 20, past_the_file),
        ("a command past the commands", MACHO_LAST + 4, 4096),
        ("an empty command", MACHO_LAST + 4, 0),
        ("sections past their command", MACHO_TEXT + 64, 3),
        ("code past the file", code_section + 48, past_the_file),
    ] {
        let mut file = whole.clone();
        file[at..at + 4].copy_from_slice(&value.to_le_bytes());
        let before = file.clone();
        blank_code(&mut file);
        assert_eq!(file, before, "{what}");
    }
}

#[test]
#[should_panic(expected = "no anchor")]
fn an_unknown_id_panics() {
    let _ = anchor("prompt.nope");
}

#[test]
fn a_guard_regex_escapes_metacharacters_and_dots_its_spaces() {
    assert_eq!(
        guard_regex("Do you want to proceed?"),
        "Do.you.want.to.proceed\\?"
    );
    assert_eq!(
        guard_regex("Run a (dynamic) x.y"),
        "Run.a.\\(dynamic\\).x\\.y"
    );
    let meta = "a\\b.c+d*e?f(g)h|i[j]k{1}l^m$n o";
    let re =
        aterm_regex::Regex::new(&format!("^{}$", guard_regex(meta))).expect("guard_regex compiles");
    assert!(re.is_match(meta));
    assert!(re.is_match(&meta.replace(' ', "\u{00a0}")));
    assert!(!re.is_match("a\\bXc+d*e?f(g)h|i[j]k{1}l^m$n o"));
    assert!(!re.is_match("a\\b.c+d*ef(g)h|i[j]k{1}l^m$n o"));
}

/// The canary's version is the store record's, read through a `current`
/// link, and the build is never started: its stand-in would leave a
/// mark if it ran.
#[cfg(unix)]
#[test]
fn the_canary_reads_the_build_record_and_never_runs_the_build() {
    let root = aterm_tempfile::TempDir::new().expect("a private store");
    let root = root.path();
    let build = root.join("store/agy/1000001000004000000");
    std::fs::create_dir_all(build.join("bin")).expect("a bin directory");
    let mark = root.join("ran");
    let bin = build.join("bin/agy");
    std::fs::write(&bin, format!("#!/bin/sh\ntouch '{}'\n", mark.display())).expect("a build");
    std::fs::set_permissions(&bin, {
        let mut p = std::fs::metadata(&bin).expect("its mode").permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut p, 0o755);
        p
    })
    .expect("executable");
    let record = root.join("store/agy/1000001000004000000.vendor");
    std::fs::write(
        &record,
        "schema = 1\nprogram = \"agy\"\nversion = \"1.4.0\"\n",
    )
    .expect("a record");
    std::os::unix::fs::symlink(&build, root.join("store/agy/current")).expect("a link");
    let via_link = root.join("store/agy/current/bin/agy");
    let managed = format!("agy → {} — managed 1.4.0", via_link.display());
    assert_eq!(canary_path(&managed, Some(root)), Some(via_link.clone()));
    assert_eq!(
        canary_path("agy — absent from PATH", None),
        None,
        "no unmanaged path is guessed without a default store"
    );
    assert_eq!(
        build_version(&via_link.to_string_lossy()).as_deref(),
        Some("1.4.0")
    );
    assert!(!mark.exists(), "the build was never started");
    let outside = root.join("agy");
    std::os::unix::fs::symlink(&bin, &outside).expect("a link from outside the store");
    assert_eq!(
        build_version(&outside.to_string_lossy()).as_deref(),
        Some("1.4.0")
    );
    std::fs::write(&record, "schema = 1\nprogram = \"agy\"\n").expect("a record");
    assert_eq!(build_version(&bin.to_string_lossy()), None);
    std::fs::remove_file(&record).expect("removed");
    assert_eq!(build_version(&bin.to_string_lossy()), None);
    assert!(!mark.exists(), "the build was never started");
}

/// An anchor a release dropped is drawn from its `since` up to that release, not by it or
/// any later one, and not by a build of no known version (taken for the newest); one no
/// release dropped stays drawn from its `since` on. CONTROL: below `since` neither is drawn.
#[test]
fn an_anchor_a_release_dropped_is_drawn_only_before_it() {
    let kept = lit("kept", "kept", "2.3.3");
    let dropped = gone(lit("dropped", "dropped", "2.3.3"), "2.5.0");
    for (version, drawn) in [
        (Some("2.3.1"), (false, false)),
        (Some("2.3.3"), (true, true)),
        (Some("2.4.1"), (true, true)),
        (Some("2.5.0"), (true, false)),
        (Some("2.5.1"), (true, false)),
        (None, (true, false)),
    ] {
        assert_eq!(
            (kept.drawn_by(version), dropped.drawn_by(version)),
            drawn,
            "{version:?}"
        );
    }
}

/// Kimi Code 2.5.0 and 2.5.1 carry none of 2.3.3's quota message patterns, only its code and
/// the OpenAI-type code (measured on the store's 2.5.0 and 2.5.1 binaries): a build owes
/// exactly the `kimi.wall.*` anchors its classifier carries, so the `vendor_drift` canary
/// holds each build to its own words.
#[test]
fn a_kimi_build_owes_only_the_quota_words_its_classifier_carries() {
    let owed = |version: Option<&str>| -> Vec<&str> {
        KIMI_ANCHORS
            .iter()
            .filter(|a| a.id.starts_with("kimi.wall.") && a.drawn_by(version))
            .map(|a| a.id)
            .collect()
    };
    let codes = ["kimi.wall.quota_code"];
    assert_eq!(owed(Some("2.5.0")), codes);
    assert_eq!(owed(Some("2.5.1")), codes);
    assert_eq!(
        owed(None),
        codes,
        "a build of no known version is the newest"
    );
    assert_eq!(
        owed(Some("2.4.0")),
        [
            "kimi.wall.quota",
            "kimi.wall.balance",
            "kimi.wall.insufficient",
            "kimi.wall.recharge",
            "kimi.wall.arrears",
            "kimi.wall.quota_code",
        ],
        "CONTROL: 2.3.3's classifier, through the last build before 2.5.0"
    );
}

#[test]
fn newer_than_orders_versions_by_number() {
    for (a, b) in [
        ("2.1.300", "2.1.30"),
        ("2.1.300", "2.1.299"),
        ("2.1.10", "2.1.9"),
        ("2.1.294.1", "2.1.294"),
        ("1.4.9-R6297.1", "1.4.9"),
        ("1.4.10-R6427.1", "1.4.9-R6297.1"),
        ("3", "2.9.9"),
    ] {
        assert!(newer_than(a, b), "{a} > {b}");
        assert!(!newer_than(b, a), "{b} > {a}");
    }
    assert!(!newer_than("2.1.300", "2.1.300"));
    assert!("2.1.9" > "2.1.10" && !newer_than("2.1.9", "2.1.10"));
}

#[test]
fn the_record_keys_are_literals_the_lookups_read() {
    for (prefix, table) in [
        ("transcript.", TRANSCRIPT_KEYS),
        ("kimi.wire.", KIMI_WIRE_KEYS),
    ] {
        for a in table {
            assert_eq!(a.kind, AnchorKind::Literal, "{}", a.id);
            assert_eq!(anchor(a.id), a.text);
            assert_eq!(anchor_text(a.id), a.text);
            assert_eq!(a.id.strip_prefix(prefix), Some(a.text));
        }
    }
    let mut ids: Vec<&str> = ANCHORS
        .iter()
        .chain(AGY_ANCHORS)
        .chain(MUSE_ANCHORS)
        .chain(GROK_ANCHORS)
        .chain(KIMI_ANCHORS)
        .chain(KIMI_WIRE_KEYS)
        .chain(TRANSCRIPT_KEYS)
        .chain(ROLLOUT_KEYS)
        .map(|a| a.id)
        .collect();
    let all = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), all, "an id is two anchors'");
}

#[test]
fn a_muse_version_is_read_with_its_revision() {
    assert_eq!(
        version_in("Muse Code 1.4.9 (1.4.9-R6297.1)\n").as_deref(),
        Some("1.4.9-R6297.1")
    );
    let since = MUSE_ANCHORS[0].since;
    assert!(!newer_than(
        since,
        &version_in("Muse Code 1.4.9 (1.4.9-R6297.1)").unwrap()
    ));
    assert!(newer_than(since, "1.4.9"), "why the revision is read");
    let boxes = MUSE_ANCHORS
        .iter()
        .find(|a| a.id == "muse.box.shell")
        .map(|a| a.since)
        .expect("the shell box's anchor");
    let built = version_in("Muse Code 1.4.10 (1.4.10-R6427.1)").unwrap();
    assert_eq!(built, "1.4.10-R6427.1");
    assert!(!newer_than(boxes, &built));
    assert!(!newer_than(since, &built));
    assert!(newer_than(boxes, "1.4.9-R6297.1"));
    for (line, want) in [
        ("2.1.286 (Claude Code)", "2.1.286"),
        ("codex-cli 0.130.0", "0.130.0"),
        ("1.4.0\n", "1.4.0"),
        ("Muse Code 1.4.9 (1.4.9-Rnext)", "1.4.9"),
    ] {
        assert_eq!(version_in(line).as_deref(), Some(want), "{line}");
    }
    assert_eq!(version_in("Muse Code"), None);
}

/// aterm-agent's committed step testdata, beside this package.
fn rollouts() -> std::path::PathBuf {
    aterm_tempfile::manifest_dir().join("../aterm-agent/src/harness/testdata/steps")
}

fn committed_rollouts(build: &str) -> Vec<u8> {
    let stem = rollout_reduction_stem(build);
    let leg = format!("{stem}-");
    let dir = rollouts();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("the step testdata")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            n.strip_suffix(".jsonl")
                .is_some_and(|b| b == stem || b.starts_with(&leg))
        })
        .collect();
    names.sort();
    let head = format!("# codex {build} ");
    let mut out = Vec::new();
    for n in names {
        let text = std::fs::read_to_string(dir.join(&n)).expect("a reduction");
        assert!(text.starts_with(&head), "{n} is a reduction of {build}");
        out.extend_from_slice(text.as_bytes());
    }
    out
}

#[test]
fn every_rollout_key_is_in_its_committed_rollout() {
    let quoted = |a: &Anchor| -> Vec<u8> { format!("\"{}\"", a.text).into_bytes() };
    for a in ROLLOUT_KEYS {
        assert_eq!(a.kind, AnchorKind::Literal, "{}", a.id);
        assert_eq!(anchor(a.id), a.text);
        assert_eq!(anchor_text(a.id), a.text);
        assert_eq!(a.id.strip_prefix("rollout."), Some(a.text));
        let rollouts = committed_rollouts(a.since);
        assert!(!rollouts.is_empty(), "{}: no rollout of {}", a.id, a.since);
        assert!(
            contains_bytes(&rollouts, &quoted(a)),
            "{}: {:?} is not in the committed rollout of {}",
            a.id,
            a.text,
            a.since
        );
    }
    let builds = committed_alab_builds();
    assert!(
        builds.iter().any(|b| b == ALAB_BUILD),
        "the codex-alab reductions are read: {builds:?}"
    );
    for build in &builds {
        let alab = committed_rollouts(build);
        for key in [
            "rollout.session_meta",
            "rollout.turn_context",
            "rollout.task_complete",
            "rollout.thread_settings_applied",
            "rollout.token_count",
            "rollout.function_call",
            "rollout.function_call_output",
            "rollout.cli_version",
            "rollout.turn_id",
            "rollout.call_id",
            "rollout.duration_ms",
            "rollout.last_token_usage",
            "rollout.total_token_usage",
            "rollout.model",
            "rollout.codex_error_info",
            "rollout.usage_limit_exceeded",
            "rollout.exec_command",
            "rollout.item_completed",
            "rollout.item",
            "rollout.exit_code",
            "rollout.task_started",
            "rollout.turn_attribution",
            "rollout.turn_trigger",
        ] {
            let a = ROLLOUT_KEYS.iter().find(|a| a.id == key).expect("a key");
            assert!(contains_bytes(&alab, &quoted(a)), "{key} on {build}");
        }
        assert!(!contains_bytes(&alab, &quoted(&PLANTED_ROLLOUT_KEY[0])));
    }
    let rollouts = committed_rollouts("0.163.0");
    assert!(!contains_bytes(&rollouts, &quoted(&PLANTED_ROLLOUT_KEY[0])));
    assert!(committed_rollouts("9.9.999").is_empty());
}

const ALAB_BUILD: &str = "0.164.0-dev.12521+g3d3eb12d8";

/// Every codex-alab build a committed reduction's header names, so a build is held to the keys the
/// fold reads the moment its reductions are committed, with no list to keep beside them.
fn committed_alab_builds() -> Vec<String> {
    let mut builds: Vec<String> = std::fs::read_dir(rollouts())
        .expect("the step testdata")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("codex-"))
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path()).ok()?;
            let build = text.strip_prefix("# codex ")?.split(' ').next()?.to_owned();
            rollout_reduction_stem(&build)
                .starts_with("codex-alab-")
                .then_some(build)
        })
        .collect();
    builds.sort();
    builds.dedup();
    builds
}

#[test]
fn a_rollout_reduction_is_named_by_its_build() {
    assert_eq!(rollout_reduction_stem("0.163.0"), "codex-0.163.0");
    assert_eq!(
        rollout_reduction_stem(ALAB_BUILD),
        "codex-alab-0.164.0-dev.12521"
    );
    assert!(!committed_rollouts(ALAB_BUILD).is_empty());
    for e in std::fs::read_dir(rollouts())
        .expect("the step testdata")
        .filter_map(Result::ok)
    {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("codex-") {
            continue;
        }
        let text = std::fs::read_to_string(e.path()).expect("a reduction");
        let build = text
            .strip_prefix("# codex ")
            .and_then(|t| t.split(' ').next())
            .unwrap_or_else(|| panic!("{name} opens with the reducer's line"));
        let stem = rollout_reduction_stem(build);
        assert!(
            name == format!("{stem}.jsonl") || name.starts_with(&format!("{stem}-")),
            "{name} is a reduction of {build}"
        );
    }
    assert_eq!(
        rollout_reduction_stem("1.0.0+g3d3eb12"),
        "codex-1.0.0+g3d3eb12"
    );
    assert_eq!(
        rollout_reduction_stem("0.164.0-dev.1+build7"),
        "codex-0.164.0-dev.1+build7"
    );
}

#[test]
fn the_reset_templates_are_built_from_the_readers_rows() {
    type Templates = Vec<(&'static str, Vec<String>)>;
    let templates = reset_templates();
    let mut names: Vec<&str> = templates.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), templates.len(), "a name is two templates'");
    assert!(
        templates
            .iter()
            .all(|(_, spellings)| !spellings.is_empty() && spellings.iter().all(|t| !t.is_empty()))
    );
    let spelled = |templates: &[(&'static str, Vec<String>)]| {
        templates
            .iter()
            .flat_map(|(_, spellings)| spellings.iter().map(|t| t.replace(JS_NAME, "a")))
            .collect::<Vec<_>>()
            .join("|")
            .into_bytes()
    };
    let bytes = spelled(&templates);
    assert!(missing_templates(&bytes, &templates).is_empty());
    let one = |pick: fn(&[String]) -> &String| -> Templates {
        templates
            .iter()
            .map(|(name, spellings)| (*name, vec![pick(spellings).clone()]))
            .collect()
    };
    let (older, newest) = (one(|s| &s[0]), one(|s| &s[s.len() - 1]));
    assert!(missing_templates(&spelled(&older), &templates).is_empty());
    assert!(missing_templates(&spelled(&newest), &templates).is_empty());
    let strip = templates
        .iter()
        .find(|(name, _)| *name == "model limit names' strip")
        .map(|(_, spellings)| spellings.as_slice());
    assert_eq!(
        strip,
        Some(
            &[
                ".replace(/ limit$/,\"\"));break;case\"seven_day_sonnet\":".to_string(),
                ",\" limit\"));break;case\"seven_day_sonnet\":".to_string(),
            ][..]
        )
    );
    let no_helper: Templates = newest
        .iter()
        .filter(|(name, _)| *name != "model limit names' strip helper")
        .cloned()
        .collect();
    assert_eq!(
        missing_templates(&spelled(&no_helper), &templates),
        ["model limit names' strip helper"]
    );
    let reworded: Templates = templates
        .iter()
        .map(|(name, spellings)| {
            (
                *name,
                spellings
                    .iter()
                    .map(|t| t.replace(anchor("reset.yes"), "Yes, spend my"))
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        missing_templates(&spelled(&reworded), &templates),
        ["confirmation yes"]
    );
    let planted = [(
        "planted",
        vec![format!(
            "{} planted-by-no-build reset?",
            anchor("reset.row")
        )],
    )];
    assert_eq!(missing_templates(&bytes, &planted), ["planted"]);
}

/// The auto notice is whole in a build that draws it as one string (2.1.311) or in parts
/// (2.1.312, its word behind a flag compiled off), and in no build where the flag is on or the
/// word changed.
#[test]
fn the_auto_notice_is_held_whole_or_by_its_parts_and_its_flag_off() {
    let templates: Vec<_> = composed_templates()
        .into_iter()
        .filter(|(name, _)| name.starts_with("effort"))
        .collect();
    let older = b"Wb={label:\"Effort level set to auto\",f:1}".as_slice();
    assert!(missing_templates(older, &templates).is_empty());
    let parts = "function Re(){return!1}function E1(){return Re()?\"adaptive\":\"auto\"}\
                 function q(){return`Effort level set to ${E1()}${u}`}";
    assert!(missing_templates(parts.as_bytes(), &templates).is_empty());
    let on = parts.replace("return!1", "return!0");
    assert_eq!(
        missing_templates(on.as_bytes(), &templates),
        ["effort auto word"]
    );
    let other = parts.replace("\"auto\"}", "\"automatic\"}");
    assert_eq!(
        missing_templates(other.as_bytes(), &templates),
        ["effort auto word"]
    );
    let called = parts.replace("${E1()}", "${level}");
    assert_eq!(
        missing_templates(called.as_bytes(), &templates),
        ["effort auto notice"]
    );
}

/// The title as 2.1.317 and 2.1.318 hold it, minified names apart. The glyph's other uses in
/// the bundle are no evidence for it: a title that drops the glyph is missing beside them.
#[test]
fn the_ide_diff_title_is_held_whole_with_its_glyph() {
    let title: Vec<_> = composed_templates()
        .into_iter()
        .filter(|(name, _)| *name == "IDE diff title")
        .collect();
    for held in [
        r#"pn=h.showingDiffInIDE?Mxi`Opened changes in ${$le(h.ideName??"IDE")} \u29C9`:void 0"#,
        r#"Tn=h.showingDiffInIDE?r1i`Opened changes in ${rue(h.ideName??"IDE")} \u29C9`:void 0"#,
    ] {
        assert!(
            missing_templates(held.as_bytes(), &title).is_empty(),
            "{held}"
        );
    }
    let dropped = r#"r1i`Opened changes in ${rue(h.ideName??"IDE")}`,x="\u29C9 open files""#;
    assert_eq!(
        missing_templates(dropped.as_bytes(), &title),
        ["IDE diff title"]
    );
}

/// `template` with its slots filled by `names` in turn, and where each fill stands.
fn filled(template: &str, names: [&str; 4]) -> (String, Vec<std::ops::Range<usize>>) {
    let mut names = names.into_iter();
    let (mut out, mut slots) = (String::new(), Vec::new());
    for (i, piece) in template.split(JS_NAME).enumerate() {
        if i > 0 {
            let name = names.next().expect("a name per slot");
            slots.push(out.len()..out.len() + name.len());
            out.push_str(name);
        }
        out.push_str(piece);
    }
    (out, slots)
}

/// A slot takes the minifier's renaming and no other change: in the limit
/// names' switch as 2.1.308 and 2.1.309 spell it, each word changed in
/// turn leaves the template whole only where that word is a slot's name,
/// and the switch holds no spelling of the template with one of its own
/// words changed.
#[test]
fn a_minifiers_name_fills_a_templates_slot() {
    let templates = reset_templates();
    let (_, spellings) = templates
        .iter()
        .find(|(name, _)| *name == "limit names")
        .expect("the limit names' switch");
    let names = &spellings[0];
    assert_eq!(names.matches(JS_NAME).count(), 4, "{names:?}");
    let switch = |set, reset| filled(names, [set, set, set, reset]);
    let built = |set, reset| {
        format!(
            "function sr(x){{let s=new Set;for(let _ of x.clears)switch(_){{{}}}}}",
            switch(set, reset).0
        )
    };
    let v308 = built("s", "a");
    let v309 = built("s", "i");
    assert!(v308.contains("s.add(a.givesBackWeekly?\"Fable\":\"weekly\")"));
    assert!(v309.contains("s.add(i.givesBackWeekly?\"Fable\":\"weekly\")"));
    let one = [("limit names", vec![names.clone()])];
    for ok in [v308.as_str(), v309.as_str(), &built("$e", "_Ab9")] {
        assert!(missing_templates(ok.as_bytes(), &one).is_empty(), "{ok}");
        assert!(!contains(ok.as_bytes(), names), "the mutant holds in none");
    }
    for bad in [
        built("s", "9a"),
        built("s", "f()"),
        built("s", ""),
        built("s.x", "a"),
    ] {
        assert_eq!(
            missing_templates(bad.as_bytes(), &one),
            ["limit names"],
            "{bad}"
        );
    }
    for (switch, slots) in [switch("s", "a"), switch("s", "i")] {
        let words = words(&switch);
        assert!(slots.iter().all(|s| words.contains(s)), "{slots:?}");
        for (word, changed) in words.iter().zip(reworded(&switch)) {
            assert_eq!(
                missing_templates(changed.as_bytes(), &one).is_empty(),
                slots.contains(word),
                "{changed}"
            );
        }
        for template in reworded(names) {
            assert!(!holds(switch.as_bytes(), &template), "{template}");
        }
    }
    assert!(holds(b"a.b", "a"), "a template with no slot is a literal");
    assert!(!holds(b"a.b", "c"));
}

#[test]
fn a_template_holds_past_its_heads_earlier_places() {
    let templates = reset_templates();
    let (_, spellings) = templates
        .iter()
        .find(|(name, _)| *name == "limit names")
        .expect("the limit names' switch");
    let names = &spellings[0];
    let (switch, _) = filled(names, ["s", "s", "s", "i"]);
    let decoy = |k: usize| format!("switch(w{k}){{case\"five_hour\":return\"5h\";default:}}");
    let decoys = |range: std::ops::Range<usize>| range.map(decoy).collect::<String>();
    let one = [("limit names", vec![names.clone()])];
    let bundle = format!("{}{switch}{}", decoys(0..8), decoys(8..9));
    assert_eq!(bundle.matches("case\"five_hour\":").count(), 10);
    assert!(
        bundle.find(&switch) > bundle.find("case\"five_hour\":"),
        "the switch is not the head's first place"
    );
    assert!(missing_templates(bundle.as_bytes(), &one).is_empty());
    assert_eq!(
        missing_templates(decoys(0..9).as_bytes(), &one),
        ["limit names"]
    );
}

#[test]
#[should_panic(expected = "a slot touches an identifier's char")]
fn a_slot_glued_to_a_word_is_refused() {
    holds(b"x=fooabc;", &format!("x={JS_NAME}abc;"));
}

#[test]
#[should_panic(expected = "a slot touches an identifier's char")]
fn a_word_glued_before_a_slot_is_refused() {
    holds(b"let sa=1", &format!("let s{JS_NAME}=1"));
}
