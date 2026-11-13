// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn v(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn a_login_a_server_or_an_update_never_runs_in_the_lab() {
    for bad in [
        "login", "logout", "acp", "upgrade", "update", "web", "server", "rc", "vis", "provider",
        "doctor", "--login",
    ] {
        assert!(safe_args(&v(&["--yolo", bad])).is_err(), "{bad}");
    }
    assert!(safe_args(&v(&["-S", "abc", "-c", "--plan", "please login later"])).is_ok());
}

#[test]
fn options_parse_and_the_launcher_execs_kimi_by_path_with_scratch_homes() {
    let l = parse_launch(&v(&["-e", "K=v 1", "--no-wait", "--", "-S", "s1"])).unwrap();
    assert!(l.no_wait && l.envs == ["K=v 1"]);
    assert_eq!(l.args, ["-S", "s1"]);
    assert!(parse_launch(&v(&["--bogus"])).is_err());
    assert!(parse_launch(&v(&["-e"])).is_err());
    let run = Run {
        tag: "t".into(),
        run: "/s/runs/t".into(),
        w: "/s/runs/t/scratch".into(),
        port: "5".into(),
        proxy: "http://127.0.0.1:6".into(),
        kimi_bin: "/a b/kimi".into(),
        ..Run::default()
    };
    let s = launcher(&run, &l.envs, &l.args);
    assert!(
        s.starts_with("#!/bin/sh\nclear\nexec env -u KIMI_API_KEY "),
        "{s}"
    );
    for want in [
        " -u KIMI_MODEL_API_KEY -u KIMI_CODE_CUSTOM_HEADERS -u KIMI_CODE_BASE_URL -u KIMI_CODE_OAUTH_HOST ",
        " -u MOONSHOT_API_KEY ",
        "  PATH=\"/s/runs/t/shim:$PATH\" HOME=/s/runs/t/scratch/home TMPDIR=/s/runs/t/scratch/tmp KIMI_CODE_HOME=/s/runs/t/scratch/home/.kimi-code \\\n",
        "XDG_CONFIG_HOME=/s/runs/t/scratch/home/.config ",
        "  HTTP_PROXY=http://127.0.0.1:6 HTTPS_PROXY=http://127.0.0.1:6 ",
        "BROWSER=/s/runs/t/shim/open \\\n",
        "  KIMI_CODE_NO_AUTO_UPDATE=1 KIMI_DISABLE_TELEMETRY=1 \\\n",
        "  'K=v 1' \\\n",
    ] {
        assert!(s.contains(want), "{want} in\n{s}");
    }
    assert!(s.ends_with("  '/a b/kimi' -S s1\n"), "{s}");
    let c = config("5");
    for want in [
        "default_model = \"lab\"",
        "[providers.lab]\ntype = \"openai\"\nbase_url = \"http://127.0.0.1:5/v1\"\napi_key = \"lab-not-a-key\"",
        "[models.lab]\nprovider = \"lab\"\nmodel = \"lab-model\"",
    ] {
        assert!(c.contains(want), "{want} in\n{c}");
    }
}

#[test]
fn an_extra_variable_never_replaces_the_lab_s_own() {
    for bad in [
        "KIMI_API_KEY=real",
        "KIMI_CODE_HOME=/Users//o/.kimi-code",
        "MOONSHOT_BASE_URL=https://example.com",
        "OPENAI_API_KEY=x",
        "https_proxy=",
        "HOME=/Users//o",
        "BROWSER=open",
        "XDG_CONFIG_HOME=/Users//o/.config",
    ] {
        assert!(parse_launch(&v(&["-e", bad])).is_err(), "{bad}");
    }
    assert!(parse_launch(&v(&["-e", "COLUMNS=80"])).is_ok());
}
