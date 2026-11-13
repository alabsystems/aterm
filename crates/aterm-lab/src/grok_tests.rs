// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn v(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn a_sign_in_an_update_or_a_server_never_runs_in_the_lab() {
    for bad in [
        "login",
        "logout",
        "update",
        "mcp",
        "leader",
        "worker",
        "host-server",
        "--device-auth",
        "--leader-socket",
    ] {
        assert!(safe_args(&v(&["--model", "m", bad])).is_err(), "{bad}");
    }
    assert!(safe_args(&v(&["-r", "--model", "m", "please login later"])).is_ok());
}

#[test]
fn options_parse_and_the_launcher_proves_the_jail_then_execs_grok_at_the_stub() {
    let l = parse_launch(&v(&["-e", "K=v 1", "--no-wait", "--", "-r", "x"])).unwrap();
    assert!(l.no_wait && l.envs == ["K=v 1"]);
    assert_eq!(l.args, ["-r", "x"]);
    assert!(parse_launch(&v(&["--bogus"])).is_err());
    assert!(parse_launch(&v(&["-e"])).is_err());
    let run = Run {
        tag: "t".into(),
        run: "/s/runs/t".into(),
        w: "/s/runs/t/scratch".into(),
        port: "5".into(),
        proxy: "http://127.0.0.1:6".into(),
        grok_bin: "/a b/grok".into(),
        ..Run::default()
    };
    let s = launcher(&run, &l.envs, &l.args);
    let probe = s.find("probe=$(/usr/bin/open ").expect("the probe");
    let exec = s.find("exec env -u OPENAI_API_KEY ").expect("the exec");
    assert!(probe < exec, "the probe runs before Grok Build\n{s}");
    assert!(
        s.contains(&format!("case $probe in *'{JAIL_DENIED}'*) ;; *) printf ")),
        "{s}"
    );
    assert!(s.contains(&format!("exit {JAIL_UNPROVEN};; esac\n")), "{s}");
    for want in [
        " -u GROK_CODE_XAI_API_KEY -u GROK_DEPLOYMENT_KEY -u GROK_AUTH_PATH ",
        "  PATH=\"/s/runs/t/shim:$PATH\" HOME=/s/runs/t/scratch/home TMPDIR=/s/runs/t/scratch/tmp XDG_CONFIG_HOME=/s/runs/t/scratch/home/.config ",
        "  HTTP_PROXY=http://127.0.0.1:6 HTTPS_PROXY=http://127.0.0.1:6 ",
        " BROWSER=/usr/bin/false \\\n",
        "  GROK_HOME=/s/runs/t/scratch/home/.grok XAI_API_KEY=xai-lab-not-a-key \
         GROK_XAI_API_BASE_URL=http://127.0.0.1:5/v1 GROK_CLI_CHAT_PROXY_BASE_URL=http://127.0.0.1:5/v1 \\\n",
        "  GROK_DISABLE_AUTOUPDATER=1 ",
        "  'K=v 1' \\\n",
    ] {
        assert!(s.contains(want), "{want} in\n{s}");
    }
    assert!(s.ends_with("  '/a b/grok' -r x\n"), "{s}");
}

#[test]
fn an_extra_variable_never_replaces_the_lab_s_own() {
    for bad in [
        "XAI_API_KEY=real",
        "GROK_CODE_XAI_API_KEY=x",
        "GROK_XAI_API_BASE_URL=https://api.x.ai/v1",
        "GROK_CLI_CHAT_PROXY_BASE_URL=https://cli-chat-proxy.grok.com/v1",
        "GROK_MODELS_LIST_URL=https://api.x.ai/v1/models",
        "GROK_HOME=/Users//o/.grok",
        "GROK_AUTH_PATH=/Users//o/.grok/auth.json",
        "GROK_DEPLOYMENT_KEY=k",
        "GROK_DISABLE_AUTOUPDATER=0",
        "BROWSER=open",
        "https_proxy=",
        "HOME=/Users//o",
        "XDG_CONFIG_HOME=/Users//o/.config",
    ] {
        assert!(parse_launch(&v(&["-e", bad])).is_err(), "{bad}");
    }
    assert!(parse_launch(&v(&["-e", "GROK_FOLDER_TRUST=0"])).is_ok());
}
