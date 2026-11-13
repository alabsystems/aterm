// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn env_sh_round_trips_every_field() {
    let r = Run {
        tag: "t1".into(),
        run: "/s/runs/t1".into(),
        w: "/s/runs/t1/scratch".into(),
        sockdir: "/tmp/at.AbCd".into(),
        sock: "/tmp/at.AbCd/a.sock".into(),
        proj: "/tmp/at.AbCd/proj".into(),
        port: "50130".into(),
        pport: "50131".into(),
        proxy: "http://127.0.0.1:50131".into(),
        stub_pid: 11,
        instance_pid: 12,
        sampler_pid: 13,
        claude_bin: "/Users//o/Library/Application Support/aterm/pkg/store/claude/1/bin/claude"
            .into(),
        claude_ver: "2.1.300".into(),
        cols: "144".into(),
        rows: "50".into(),
        seam: "0".into(),
        route_seam: "1".into(),
        codex_bin: "/Users//o/Library/Application Support/aterm/pkg/store/codex/1/bin/codex"
            .into(),
        codex_ver: "0.163.0".into(),
        agy_bin: "/Users//o/Library/Application Support/aterm/pkg/store/agy/1000001000004000000/bin/agy"
            .into(),
        agy_ver: "1.4.0".into(),
        muse_bin: "/Users//o/Library/Application Support/aterm/pkg/store/muse/2001004010006427002/bin/muse"
            .into(),
        muse_ver: "1.4.10-R6427.1".into(),
        grok_bin: "/s/gk/home/Library/Application Support/aterm/pkg/store/grok/1000001000000000070/bin/grok"
            .into(),
        grok_ver: "1.0.70".into(),
        kimi_bin: "/s/gk/store/kimi/1000002000004000000/bin/kimi".into(),
        kimi_ver: "2.4.0".into(),
        agent: "codex".into(),
        codex_mode: "chatgpt".into(),
        aterm: "/Users//o/Developer/aterm/target/debug/aterm".into(),
        aterm_build: "0.150.0+g859f55cf4a7e-dirty".into(),
        mark: "lab.jail.t1.0123456789abcdef".into(),
        instance_env: "TMPDIR=/tmp ATERM_DEBUG_PASTE_FILE=/s/runs/t1/paste.txt".into(),
        clock: "1".into(),
        window: "1".into(),
        reroute: "1".into(),
        home_image: "/s/runs/t1/home.sparseimage".into(),
        jail_scan: "1".into(),
        fabric: "/tmp/at.AbCd/fabric".into(),
        broker_pid: 14,
        tab_strip: "1".into(),
        claude_login: STORED_LOGIN.into(),
    };
    assert_eq!(Run::parse(&r.render()), r);
    assert_eq!(r.fixture_prefix(), "codex-0.163.0");
    for (agent, prefix) in [
        ("agy", "agy-1.4.0"),
        ("muse", "muse-1.4.10-R6427.1"),
        ("grok", "grok-1.0.70"),
        ("kimi", "kimi-2.4.0"),
        ("claude", "claude-2.1.300"),
        ("", "claude-2.1.300"),
        ("bogus", "claude-2.1.300"),
    ] {
        let r = Run {
            agent: agent.into(),
            ..r.clone()
        };
        assert_eq!(r.fixture_prefix(), prefix, "{agent}");
    }
    let unread = Run {
        agent: "muse".into(),
        muse_ver: String::new(),
        ..r.clone()
    };
    assert_eq!(unread.fixture_prefix(), "muse-?");

    // NEGATIVE CONTROL: a quoted field containing a newline and `KEY=VALUE` text must round-trip
    // as part of its own field and never overwrite `ATERM` or `AGENT`.
    let multiline = Run {
        proj: "/tmp/at.AbCd/dir'with\nATERM=/evil/spoof\r\nAGENT=codex\\tail".into(),
        agent: "agy".into(),
        ..r
    };
    let parsed = Run::parse(&multiline.render());
    assert_eq!(parsed, multiline);
    assert_eq!(parsed.aterm, "/Users//o/Developer/aterm/target/debug/aterm");
    assert_eq!(parsed.agent, "agy");
}

/// AN OLDER LAB'S GEMINI CLI TAB IS REFUSED, never read as Claude Code's: the env.sh an
/// aterm-lab with Gemini CLI wrote for a `lab gemini` tab parses as written, every verb that
/// drives the agent ([`Run::load`]) refuses it naming agy and `down`, and the words the lab
/// still runs (the empty one of a lab older than `AGENT`, and an unknown one) are admitted as
/// before.
#[test]
fn an_older_labs_gemini_cli_tab_is_refused_never_read_as_claude() {
    let old = "TAG=g1\nRUN=/s/runs/g1\nW=/s/runs/g1/scratch\nCLAUDE_VER=2.1.300\n\
               GEMINI_BIN=/opt/homebrew/bin/gemini\nGEMINI_VER=0.46.0\nAGENT=gemini\n";
    let r = Run::parse(old);
    assert_eq!((r.tag.as_str(), r.agent.as_str()), ("g1", "gemini"));
    for agent in ["gemini", "Gemini", "gemini-cli"] {
        let e = Run {
            agent: agent.into(),
            ..r.clone()
        }
        .admitted()
        .unwrap_err();
        assert!(e.contains(aterm_phase::RETIRED_AGENT), "{agent}: {e}");
        assert!(e.contains(&format!("AGENT={agent}")), "{agent}: {e}");
        assert!(e.contains(&format!("{LAB} down g1")), "{agent}: {e}");
        assert!(e.contains(&format!("{LAB} agy g1")), "{agent}: {e}");
    }
    for agent in [
        "claude", "codex", "agy", "muse", "grok", "kimi", "", "bogus",
    ] {
        let r = Run {
            agent: agent.into(),
            ..r.clone()
        };
        assert_eq!(r.clone().admitted(), Ok(r), "{agent}");
    }
}

#[test]
fn a_verb_whose_one_argument_is_its_tag_takes_a_lab_that_is_down() {
    let gone = "ki1016-gone-lab";
    assert!(!is_run(gone));
    let mut a = vec![gone.to_string(), "--keep-dumps".to_string()];
    assert_eq!(take_only_tag(&mut a), Ok(gone.to_string()));
    assert_eq!(a, vec!["--keep-dumps".to_string()]);
    let mut flags = vec!["--keep-dumps".to_string()];
    assert_eq!(take_only_tag(&mut flags), take_tag(&mut Vec::new()));
    assert_eq!(flags, vec!["--keep-dumps".to_string()]);
    assert!(take_only_tag(&mut vec!["no/tag".to_string()]).is_err());
}

#[test]
fn reads_the_env_sh_printf_q_wrote() {
    let r = Run::parse(
        "TAG=base1\nCLAUDE_BIN=/Users//o/Library/Application\\ Support/aterm/pkg/store/claude/1/bin/claude\nSTUB_PID=54527\n",
    );
    assert_eq!(r.tag, "base1");
    assert_eq!(
        r.claude_bin,
        "/Users//o/Library/Application Support/aterm/pkg/store/claude/1/bin/claude"
    );
    assert_eq!(r.stub_pid, 54527);
}
