// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn v(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn a_login_a_credential_or_a_server_never_runs_in_the_lab() {
    for bad in [
        "login",
        "logout",
        "auth",
        "mcp",
        "serve",
        "session-message",
        "plugins",
        "sandbox",
        "voice",
        "--api-key-stdin",
    ] {
        assert!(safe_args(&v(&["--trust-workspace", bad])).is_err(), "{bad}");
    }
    assert!(
        safe_args(&v(&[
            "--trust-workspace",
            "-c",
            "provider_retry.max_attempts=3",
            "please login later"
        ]))
        .is_ok()
    );
}

#[test]
fn options_parse_and_the_launcher_execs_muse_by_path_at_the_stub() {
    let l = parse_launch(&v(&[
        "-e",
        "K=v 1",
        "--no-wait",
        "--",
        "--trust-workspace",
        "-c",
        "x=1",
    ]))
    .unwrap();
    assert!(l.no_wait && l.envs == ["K=v 1"]);
    assert_eq!(l.args, ["--trust-workspace", "-c", "x=1"]);
    assert!(parse_launch(&v(&["--bogus"])).is_err());
    assert!(parse_launch(&v(&["-e"])).is_err());
    let run = Run {
        tag: "t".into(),
        run: "/s/runs/t".into(),
        w: "/s/runs/t/scratch".into(),
        port: "5".into(),
        proxy: "http://127.0.0.1:6".into(),
        muse_bin: "/a b/muse".into(),
        ..Run::default()
    };
    let s = launcher(&run, &l.envs, &l.args);
    assert!(
        s.starts_with("#!/bin/sh\nclear\nexec env -u OPENAI_API_KEY "),
        "{s}"
    );
    for want in [
        " -u CODEX_HOME -u CLAUDE_CONFIG_DIR ",
        "  PATH=\"/s/runs/t/shim:$PATH\" HOME=/s/runs/t/scratch/home TMPDIR=/s/runs/t/scratch/tmp XDG_CONFIG_HOME=/s/runs/t/scratch/home/.config ",
        "  HTTP_PROXY=http://127.0.0.1:6 HTTPS_PROXY=http://127.0.0.1:6 ",
        "  META_API_KEY=lab-not-a-key TBH_AUTH_BASE_URL=http://127.0.0.1:5 TBH_MINT_BASE_URL=http://127.0.0.1:5 \\\n",
        "TBH_DISABLE_TELEMETRY=1 TBH_DISABLE_FEATURE_CONFIG=1",
        "  'K=v 1' \\\n",
    ] {
        assert!(s.contains(want), "{want} in\n{s}");
    }
    assert!(
        s.ends_with("  '/a b/muse' --base-url http://127.0.0.1:5 --trust-workspace -c x=1\n"),
        "{s}"
    );
}

#[test]
fn an_extra_variable_never_replaces_the_lab_s_own() {
    for bad in [
        "META_API_KEY=real",
        "OPENAI_API_KEY=x",
        "TBH_AUTH_BASE_URL=https://example.com",
        "https_proxy=",
        "HOME=/Users//o",
        "TMPDIR=/tmp",
        "BROWSER=/usr/bin/open",
        "LAB_OPEN_LOG=/dev/null",
        "LAB_SECURITY_LOG=/dev/null",
        "XDG_CONFIG_HOME=/Users//o/.config",
        "-u",
        "/bin/echo",
        "=x",
        "1BAD=x",
    ] {
        assert!(parse_launch(&v(&["-e", bad])).is_err(), "{bad}");
    }
    assert!(parse_launch(&v(&["-e", "MUSE_DISABLE_APPROVAL_JUDGE=1"])).is_ok());
    assert!(safe_env("TBH_DISABLE_TELEMETRY=0").is_ok());
}
