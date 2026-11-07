// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `setup-token-lab`'s pure parts: the sentinel and its needle, each scanner over planted and
//! near-miss bytes, the probe's report and its look at its own descriptors, the judges of lanes A
//! and C, the MCP probe's answers, and lane B's stand-in frames read by the capture's own grammar.

use super::*;

/// The needle is the sentinel's own, the stub reads the sentinel as `oat:lab-t`, and nothing the
/// stub logs of it holds the needle.
#[test]
fn the_sentinel_is_the_stubs_labelled_token_and_only_it_holds_the_needle() {
    let token = sentinel();
    assert!(token.starts_with(&needle()), "{}", token.len());
    assert_eq!(
        crate::stub::auth_label(Some(&format!("Bearer {token}"))),
        format!("oat:{LABEL}")
    );
    assert!(!holds(
        crate::stub::auth_label(Some(&token)).as_bytes(),
        needle().as_bytes()
    ));
    assert!(
        !holds(crate::lab::oauth_token().as_bytes(), needle().as_bytes()),
        "the lab's unlabelled token is no sentinel"
    );
    assert!(!holds(b"", b""), "an empty needle is never held");
}

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("setup-token-lab-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch")
}

/// A tree scan finds a planted token (one across a read's boundary too), skips a link to one, a
/// skipped subtree and a file older than `since`.
/// NEGATIVE CONTROL: the needle less its last byte, planted alone, is not found.
#[test]
fn a_tree_scan_finds_a_planted_token_and_nothing_else() {
    let dir = scratch();
    let root = dir.path();
    let needle = needle();
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    std::fs::write(root.join("a/b/planted.txt"), format!("x{}y", sentinel())).unwrap();
    let mut big = vec![b'.'; (1 << 20) - 5];
    big.extend_from_slice(needle.as_bytes());
    std::fs::write(root.join("a/across.bin"), &big).unwrap();
    std::fs::write(
        root.join("near.txt"),
        &needle.as_bytes()[..needle.len() - 1],
    )
    .unwrap();
    std::os::unix::fs::symlink(root.join("a/b/planted.txt"), root.join("link")).unwrap();
    std::fs::create_dir_all(root.join("skipped")).unwrap();
    std::fs::write(root.join("skipped/planted.txt"), sentinel()).unwrap();
    let scan = scan_tree(root, needle.as_bytes(), None, &[root.join("skipped")]);
    assert_eq!(
        scan.hits,
        vec![root.join("a/across.bin"), root.join("a/b/planted.txt")]
    );
    assert_eq!(scan.files, 3, "the link and the skipped file are not read");
    let later = SystemTime::now() + Duration::from_secs(3600);
    assert!(
        scan_tree(root, needle.as_bytes(), Some(later), &[])
            .hits
            .is_empty(),
        "nothing changed since"
    );
}

/// `ps -E`'s rows holding the needle give their pids; another row and a row with no pid do not.
#[test]
fn ps_rows_holding_the_needle_give_their_pids() {
    let n = needle();
    let text =
        format!("  101 /bin/sleep 30 LAB_PLANTED={n}AAAA\n  102 /bin/zsh HOME=/x\nnot-a-pid {n}\n");
    assert_eq!(ps_hits(&text, &n), vec![101]);
}

/// A report reads back as it was written, and one with no role is none.
#[test]
fn a_probe_report_reads_back_as_written() {
    let r = ProbeReport {
        role: "mcp".into(),
        pid: 42,
        fds: vec!["0:pipe".into(), "3:sock".into()],
        read: vec!["3:107".into()],
        found_fd: Some(3),
        found_env: false,
        found_input: true,
        descriptor_env: true,
        marker: Some("aterm-claude-token-ac-0123456789abcdef".into()),
        input_keys: vec!["model".into(), "rate_limits".into()],
    };
    assert_eq!(ProbeReport::parse(&r.render()), Some(r));
    assert_eq!(ProbeReport::parse("pid=1\n"), None);
    let bare = ProbeReport::parse("role=hook\npid=7\nfound_fd=-\nmarker=-\n").unwrap();
    assert!(!bare.found() && bare.marker.is_none());
}

/// A probe finds a token waiting unread on a socket it inherited, peeking, so the socket still
/// holds it after.
/// NEGATIVE CONTROL: the same socket holding the needle less its last byte is read and not found.
#[test]
fn a_probe_finds_a_token_on_an_inherited_socket_and_takes_nothing() {
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsRawFd as _;
    let (mut ours, mut theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    ours.write_all(format!("{}\n", sentinel()).as_bytes())
        .unwrap();
    let report = probe_self("hook", needle().as_bytes());
    assert_eq!(report.found_fd, Some(theirs.as_raw_fd()), "{report:?}");
    theirs.set_nonblocking(true).unwrap();
    let mut buf = vec![0u8; 4096];
    let n = theirs.read(&mut buf).unwrap();
    assert_eq!(
        &buf[..n],
        format!("{}\n", sentinel()).as_bytes(),
        "nothing was taken"
    );

    let whole = needle();
    let near = &whole.as_bytes()[..whole.len() - 1];
    ours.write_all(near).unwrap();
    let report = probe_self("hook", needle().as_bytes());
    assert_eq!(report.found_fd, None, "{report:?}");
    assert!(
        report
            .read
            .contains(&format!("{}:{}", theirs.as_raw_fd(), near.len())),
        "{report:?}"
    );
}

fn report(role: &str, found_fd: Option<i32>, descriptor_env: bool) -> ProbeReport {
    ProbeReport {
        role: role.into(),
        pid: 9,
        found_fd,
        descriptor_env,
        ..ProbeReport::default()
    }
}

/// Lane C's judge: the gate passes only with every role reporting and none holding the token
/// (on a descriptor, in its environment or in its input) or the descriptor variable.
#[test]
fn lane_c_judges_every_child() {
    let clean = [
        report("hook", None, false),
        report("mcp", None, false),
        report("statusline", None, false),
    ];
    assert_eq!(judge_children(&clean).0, Verdict::Ok);
    assert_eq!(
        judge_children(&clean[..2]).0,
        Verdict::Unproven,
        "statusline missing"
    );
    let leaked = [
        report("hook", None, false),
        report("mcp", Some(5), false),
        report("statusline", None, false),
    ];
    let (v, why) = judge_children(&leaked);
    assert_eq!(v, Verdict::Fail);
    assert!(why.contains("mcp") && why.contains("descriptor 5"), "{why}");
    let var = [
        report("hook", None, true),
        report("mcp", None, false),
        report("statusline", None, false),
    ];
    assert_eq!(
        judge_children(&var).0,
        Verdict::Fail,
        "the variable inherited"
    );
    let in_env = [
        ProbeReport {
            found_env: true,
            ..report("hook", None, false)
        },
        report("mcp", None, false),
        report("statusline", None, false),
    ];
    let (v, why) = judge_children(&in_env);
    assert_eq!(v, Verdict::Fail);
    assert!(why.contains("its environment"), "{why}");
    let in_input = [
        report("hook", None, false),
        report("mcp", None, false),
        ProbeReport {
            found_input: true,
            ..report("statusline", None, false)
        },
    ];
    let (v, why) = judge_children(&in_input);
    assert_eq!(v, Verdict::Fail);
    assert!(
        why.contains("statusline") && why.contains("its input"),
        "{why}"
    );
}

/// Lane C's positive control passes only where the probe found the token all three ways, the
/// socket at the very descriptor it was handed.
/// NEGATIVE CONTROL: each way missed alone, a found token at another descriptor, and no report.
#[test]
fn lane_c_control_must_find_every_way() {
    let all = ProbeReport {
        found_env: true,
        found_input: true,
        ..report("hook", Some(7), false)
    };
    assert_eq!(judge_control(Some(&all), 7).0, Verdict::Ok);
    assert_eq!(
        judge_control(Some(&all), 8).0,
        Verdict::Fail,
        "found at another descriptor"
    );
    let no_fd = ProbeReport {
        found_fd: None,
        ..all.clone()
    };
    assert!(judge_control(Some(&no_fd), 7).1.contains("descriptor 7"));
    let no_env = ProbeReport {
        found_env: false,
        ..all.clone()
    };
    assert!(judge_control(Some(&no_env), 7).1.contains("environment"));
    let no_input = ProbeReport {
        found_input: false,
        ..all
    };
    assert!(judge_control(Some(&no_input), 7).1.contains("input"));
    assert_eq!(judge_control(None, 7).0, Verdict::Unproven);
}

/// `lsof -F ft` reads as the numbered descriptors with their types; the named rows are left out.
#[test]
fn lsof_fields_read_as_numbered_descriptors() {
    let text = "p42\nfcwd\ntDIR\nftxt\ntREG\nf0\ntCHR\nf1\ntunix\nf12\ntIPv4\nf13\n";
    assert_eq!(lsof_table(text), ["0:CHR", "1:unix", "12:IPv4"]);
}

/// The MCP probe answers `initialize` with the client's own version, `tools/list` with none,
/// `ping`, and any other request with an error; a notification and a line that is no JSON get
/// nothing.
#[test]
fn the_mcp_probe_answers_what_claude_code_asks() {
    let init = mcp_reply(
        r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
    )
    .unwrap();
    let j = J::parse(&init).unwrap();
    assert_eq!(
        j.get("result")
            .and_then(|r| r.get("protocolVersion"))
            .and_then(J::as_str),
        Some("2025-06-18")
    );
    let list =
        J::parse(&mcp_reply(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap()).unwrap();
    assert_eq!(
        list.get("result").and_then(|r| r.get("tools")),
        Some(&J::Arr(Vec::new()))
    );
    let other =
        J::parse(&mcp_reply(r#"{"jsonrpc":"2.0","id":"x","method":"resources/list"}"#).unwrap())
            .unwrap();
    assert!(other.get("error").is_some());
    assert_eq!(
        mcp_reply(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
        None
    );
    assert_eq!(mcp_reply("not json"), None);
}

/// Lane A passes only where the capture stopped at the paste prompt it was asked at, having told
/// the person where the sign-in opened; a token taken, a refused URL, a hang, or a stop with no
/// ask fails.
#[test]
fn lane_a_passes_only_a_stop_at_the_prompt() {
    let told = vec!["a browser window opened on claude.com: sign in as x there".to_string()];
    let stopped = Err(setup_token::Error::Stopped);
    assert_eq!(judge_frames(&stopped, true, true, &told).0, Verdict::Ok);
    assert_eq!(judge_frames(&stopped, false, false, &told).0, Verdict::Fail);
    assert_eq!(
        judge_frames(&stopped, true, false, &told).0,
        Verdict::Fail,
        "another prompt"
    );
    assert_eq!(
        judge_frames(&stopped, true, true, &[]).0,
        Verdict::Fail,
        "never told"
    );
    for e in [
        setup_token::Error::Request,
        setup_token::Error::Hang,
        setup_token::Error::NoToken,
    ] {
        assert_eq!(judge_frames(&Err(e), true, true, &told).0, Verdict::Fail);
    }
}

/// The lanes: none named is all; a tag; `--keep`; an unknown flag refused.
#[test]
fn the_verb_parses_its_lanes() {
    let all = Lanes {
        a: true,
        b: true,
        c: true,
    };
    assert_eq!(parse(&[]).unwrap(), (all, "setup-token".to_string(), false));
    let words = |w: &[&str]| w.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert_eq!(
        parse(&words(&["c", "mine", "--keep"])).unwrap(),
        (
            Lanes {
                a: false,
                b: false,
                c: true
            },
            "mine".to_string(),
            true
        )
    );
    assert!(parse(&words(&["--bogus"])).is_err());
}

/// The scratch listing names each file under a capture's folder by its path and size, never its
/// bytes, and reads no other folder of the store.
#[test]
fn the_scratch_listing_names_paths_and_sizes_only() {
    let dir = scratch();
    let folder = dir
        .path()
        .join(format!("{}1-2", setup_token::SCRATCH_PREFIX));
    std::fs::create_dir_all(folder.join("sub")).unwrap();
    std::fs::write(folder.join("sub/.claude.json"), "SECRETBYTES").unwrap();
    std::fs::create_dir_all(dir.path().join("other")).unwrap();
    std::fs::write(dir.path().join("other/x"), "y").unwrap();
    let listing = scratch_listing(dir.path());
    assert_eq!(listing, vec!["sub/.claude.json (11 B)".to_string()]);
    assert!(!listing.join("").contains("SECRET"));
}

/// Lane B's stand-in shows frames the capture's own grammar takes: its URL is admitted, its
/// paste prompt read, and its success frame mints the sentinel, printed as one year.
/// NEGATIVE CONTROL: the success frame alone, no admitted URL before it, is refused unchecked.
#[test]
fn the_stand_ins_frames_mint_the_sentinel_under_the_captures_grammar() {
    use aterm_agent::harness::setup_token::{Cleaner, Judge, Verdict as Said};
    let feed = |raw: &str| {
        let mut out = Vec::with_capacity(setup_token::BUFFER);
        let mut cleaner = Cleaner::default();
        let mut judge = Judge::new(EMAIL);
        assert!(cleaner.feed(raw.as_bytes(), &mut out));
        let said = judge.judge(&out, true);
        (said, judge.url_ok, judge.asks)
    };
    let waiting = ls::setup_token_waiting_frame(&ls::setup_token_url());
    let success = ls::setup_token_success_frame(&sentinel(), "1 year");
    let (said, url_ok, asks) = feed(&format!("{waiting}{success}"));
    assert!(url_ok && asks);
    match said {
        Said::Minted(minted) => {
            assert_eq!(minted.token.as_slice(), sentinel().as_bytes());
            assert_eq!(minted.lifetime, setup_token::Lifetime::Days(365));
        }
        other => panic!("not minted: {}", matches!(other, Said::Pending)),
    }
    let (said, _, _) = feed(&success);
    assert!(matches!(said, Said::Refused(setup_token::Error::Unchecked)));
}

/// A served stand-in's note names the descriptor's number, whether it matched and the marker,
/// and never a token.
#[test]
fn the_served_line_is_value_free() {
    assert_eq!(
        ls::served_line(
            Some(4),
            true,
            Some("aterm-claude-token-ac-0123456789abcdef")
        ),
        "descriptor=4 matched=1 marker=aterm-claude-token-ac-0123456789abcdef"
    );
    assert_eq!(
        ls::served_line(None, false, None),
        "descriptor=- matched=0 marker=-"
    );
}

/// Where a tree scan's hit is, by its path.
#[test]
fn a_hit_is_placed_by_its_path() {
    assert_eq!(
        place_of(Path::new("/r/scratch/home/.secrets/x")),
        "~/.secrets"
    );
    assert_eq!(
        place_of(Path::new("/r/scratch/state/journal/a.jsonl")),
        "the journal"
    );
    assert_eq!(place_of(Path::new("/r/scratch/aterm.log")), "aterm's logs");
    assert_eq!(
        place_of(Path::new("/r/scratch/home/.claude/settings.json")),
        "the scratch config folder"
    );
    assert_eq!(place_of(Path::new("/r/scratch/x.bin")), "another file");
}
