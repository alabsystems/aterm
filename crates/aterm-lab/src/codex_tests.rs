// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_login_or_a_server_never_runs_in_the_lab() {
    let v = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert!(safe_args(false, &v(&["login", "status"])).is_ok());
    assert!(safe_args(false, &v(&["login", "--help"])).is_ok());
    assert!(safe_args(true, &v(&["login", "status"])).is_err());
    assert!(safe_args(false, &v(&["login", "-c", "status"])).is_err());
    assert!(safe_args(false, &v(&["mcp", "login", "x"])).is_err());
    for bad in [
        "logout",
        "app",
        "app-server",
        "remote-control",
        "agents",
        "exec-server",
        "cloud",
        "update",
    ] {
        assert!(safe_args(false, &v(&["exec", bad])).is_err(), "{bad}");
    }
    assert!(safe_args(false, &v(&["exec", "--skip-git-repo-check", "hi"])).is_ok());
}

#[test]
fn the_config_names_its_login_and_trusts_the_project_both_ways() {
    let c = config_text(
        "chatgpt",
        "5",
        "/private/tmp/at.X/proj",
        "/tmp/at.X/proj",
        "",
    );
    assert!(c.starts_with("# aterm-lab codex (crates/aterm-lab), mode chatgpt:"));
    assert!(c.contains(
        "chatgpt_base_url = \"http://127.0.0.1:5/backend-api/\"\n\n[features]\napps = false\n"
    ));
    assert!(c.contains("base_url = \"http://127.0.0.1:5/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = true\n"));
    assert!(
        c.contains("[projects.\"/private/tmp/at.X/proj\"]")
            && c.contains("[projects.\"/tmp/at.X/proj\"]")
    );
    let lab = config_text("lab", "5", "/p", "/p", "gpt-x");
    assert!(
        lab.contains("model_provider = \"lab\"\nmodel = \"gpt-x\"\n")
            && lab.contains("env_key = \"LAB_CODEX_KEY\"")
    );
    assert_eq!(lab.matches("[projects.").count(), 1);
    let out = config_text("signed-out", "5", "/p", "/p", "");
    assert!(!out.contains("model_provider") && !out.contains("[model_providers.lab]"));
    let ctrl = config_text("lab", "5", "/p\n\"q\"\r\t\x00\x7f", "/p", "gpt-\n\"x\"");
    let parsed: aterm_toml::Value = aterm_toml::from_str(&ctrl).unwrap();
    assert_eq!(
        parsed.get("model").and_then(|v| v.as_str()),
        Some("gpt-\n\"x\"")
    );
}

#[test]
fn the_composed_login_is_a_jwt_the_stub_reads_by_its_label_only() {
    let j = J::parse(&auth_json("B", "acct-lab-1", "plus", 1_792_091_823)).unwrap();
    let tokens = j.get("tokens").unwrap();
    let access = tokens.get("access_token").and_then(J::as_str).unwrap();
    assert!(access.ends_with(".bGFiLW5vdC1zaWduZWQ"));
    assert_eq!(
        crate::stub::auth_label(Some(&format!("Bearer {access}"))),
        "jwt:B"
    );
    let claims = crate::stub::lab_claims(&format!("Bearer {access}")).unwrap();
    let auth = claims.get("https://api.openai.com/auth").unwrap();
    assert_eq!(
        auth.get("chatgpt_plan_type").and_then(J::as_str),
        Some("plus")
    );
    assert_eq!(
        auth.get("chatgpt_account_id").and_then(J::as_str),
        Some("acct-lab-1")
    );
    assert_eq!(
        j.get("last_refresh").and_then(J::as_str),
        Some("2026-10-15T19:17:03.000000Z")
    );
    assert_eq!(
        tokens.get("refresh_token").and_then(J::as_str),
        Some("lab-refresh-B")
    );
    assert_eq!(crate::stub::auth_label(Some("Bearer lab-not-a-key")), "key");
    assert_eq!(crate::stub::auth_label(None), "none");
    assert_eq!(
        crate::stub::auth_label(Some("Bearer sk-x")),
        "other(4 chars)"
    );
}

#[test]
fn ratelimits_name_each_window_and_the_credits() {
    let a: Vec<String> = ["76", "-", "credits=unlimited", "balance=12.5"]
        .map(String::from)
        .to_vec();
    let h = ratelimit_headers(&a).unwrap();
    assert_eq!(
        h.get("x-codex-primary-used-percent").and_then(J::as_str),
        Some("76.0")
    );
    assert_eq!(
        h.get("x-codex-primary-reset-at").and_then(J::as_str),
        Some("@NOW+3600@")
    );
    assert!(h.get("x-codex-secondary-used-percent").is_none());
    assert_eq!(
        h.get("x-codex-credits-unlimited").and_then(J::as_str),
        Some("true")
    );
    assert_eq!(
        ratelimit_headers(&["off".to_string()]).unwrap(),
        J::Obj(Vec::new())
    );
    assert!(ratelimit_headers(&["5".to_string()]).is_err());
    assert!(ratelimit_headers(&["1".into(), "2".into(), "bogus=1".into()]).is_err());
}

const NAMES_IT: &[u8] = b"\x00\xcf\xfa\xed\xfeno-daemon cannot be used with --remote\x00";

const NAMES_NONE: &[u8] = b"\x00\xcf\xfa\xed\xfeno-daemo\x00n-daemon\x00no-daemoN\x00";

fn package_at(root: &Path, version: &str, alab: bool, exe: &[u8]) -> String {
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::write(root.join("bin/codex"), exe).unwrap();
    fs::write(
        root.join(aterm_agent::harness::codex_alab::PACKAGE_JSON),
        format!(r#"{{"layoutVersion":1,"version":"{version}","entrypoint":"bin/codex"}}"#),
    )
    .unwrap();
    let mark = root.join(aterm_agent::harness::codex_alab::ALAB_BUILD_JSON);
    if alab {
        fs::write(&mark, "{}").unwrap();
    } else {
        let _ = fs::remove_file(&mark);
    }
    root.join("bin/codex").to_string_lossy().into_owned()
}

#[test]
fn the_launcher_execs_codex_by_path_with_no_daemon_where_it_is_taken() {
    let scratch = aterm_tempfile::tempdir().expect("exclusive package fixture");
    let alab = package_at(
        &scratch.path().join("alab b"),
        "0.164.0-dev.1+gabc",
        true,
        b"",
    );
    let vendor = package_at(&scratch.path().join("vendor"), "0.151.0", false, NAMES_NONE);
    let vendor_163 = package_at(
        &scratch.path().join("vendor 163"),
        "0.163.0",
        false,
        NAMES_IT,
    );
    let dev = scratch.path().join("codex-rs/target/release/codex");
    fs::create_dir_all(dev.parent().unwrap()).unwrap();
    fs::write(&dev, NAMES_IT).unwrap();
    let dev = dev.to_string_lossy().into_owned();
    let other = scratch.path().join("other/codex");
    fs::create_dir_all(other.parent().unwrap()).unwrap();
    fs::write(&other, NAMES_NONE).unwrap();
    let other = other.to_string_lossy().into_owned();
    let neither = package_at(
        &scratch.path().join("neither"),
        "0.152.0-alpha.1",
        false,
        NAMES_IT,
    );
    let run = |codex_bin: &str| Run {
        tag: "t".into(),
        run: "/s/runs/t".into(),
        w: "/s/runs/t/scratch".into(),
        port: "5".into(),
        proxy: "http://127.0.0.1:6".into(),
        codex_bin: codex_bin.into(),
        ..Run::default()
    };
    let args = ["-C".to_string(), "/x y".to_string()];
    let s = launcher(&run(&alab), true, "lab", &["K=v 1".into()], &args);
    assert!(s.starts_with("#!/bin/sh\nclear\nexec env -u OPENAI_API_KEY -u CODEX_API_KEY -u OPENAI_BASE_URL PATH=\"/s/runs/t/shim:$PATH\""), "{s}");
    assert!(
        s.contains("  LAB_CODEX_KEY=lab-not-a-key \\\n  'K=v 1' \\\n"),
        "{s}"
    );
    let special = launcher(
        &Run {
            run: r#"/s/"a"$b`c`/t"#.into(),
            w: "/s/w".into(),
            port: "5;evil".into(),
            proxy: "http://127.0.0.1:6 x".into(),
            codex_bin: alab.clone(),
            ..Run::default()
        },
        false,
        "lab",
        &[],
        &[],
    );
    assert!(
        special.contains(r#"PATH="/s/\"a\"\$b\`c\`/t/shim:$PATH""#)
            && special.contains("HTTP_PROXY='http://127.0.0.1:6 x'")
            && special
                .contains("CODEX_REFRESH_TOKEN_URL_OVERRIDE='http://127.0.0.1:5;evil/oauth/token'"),
        "{special}"
    );
    let takes = |bin: &str| {
        [(true, "lab"), (false, "chatgpt")].map(|(tab, mode)| {
            let v = launcher(&run(bin), tab, mode, &[], &args);
            let quoted = shell_quote(bin);
            if v.ends_with(&format!("  {quoted} --no-daemon -C '/x y'\n")) {
                Some(true)
            } else if v.ends_with(&format!("  {quoted} -C '/x y'\n")) && !v.contains("--no-daemon")
            {
                Some(false)
            } else {
                None
            }
        })
    };
    assert_eq!(
        takes(&alab),
        [Some(true); 2],
        "codex-alab, tab and codex-run"
    );
    let r = launcher(&run(&alab), false, "chatgpt", &[], &[]);
    assert!(!r.contains("clear") && !r.contains("LAB_CODEX_KEY="), "{r}");
    assert!(r.ends_with(" --no-daemon\n"), "codex-run's too: {r}");
    assert_eq!(
        takes(&vendor),
        [Some(false); 2],
        "vendor 0.151.0: its bytes name no flag"
    );
    assert_eq!(
        takes(&vendor_163),
        [Some(true); 2],
        "vendor 0.163.0: a release whose bytes name the flag"
    );
    assert_eq!(takes("/a b/codex"), [Some(false); 2], "no file");
    assert_eq!(takes(&dev), [Some(true); 2], "a codex-alab dev build");
    assert_eq!(takes(&other), [Some(false); 2], "a binary naming no flag");
    assert_eq!(
        takes(&neither),
        [Some(true); 2],
        "a package that says neither"
    );
    let marked = package_at(&scratch.path().join("vendor"), "0.151.0", true, NAMES_NONE);
    assert_eq!(marked, vendor);
    assert_eq!(takes(&marked), [Some(true); 2], "CONTROL: the mark");
    fs::write(&vendor_163, NAMES_NONE).unwrap();
    assert_eq!(
        takes(&vendor_163),
        [Some(false); 2],
        "CONTROL: 0.163.0's literal"
    );
    fs::write(&dev, NAMES_NONE).unwrap();
    assert_eq!(takes(&dev), [Some(false); 2], "CONTROL: the literal");
}

#[test]
fn a_binary_names_the_flag_across_a_block_seam() {
    let scratch = aterm_tempfile::tempdir().expect("exclusive binary fixture");
    let bin = scratch.path().join("codex");
    let mut bytes = vec![0u8; (4 << 20) - 4];
    bytes.extend_from_slice(b"no-daemon");
    bytes.extend_from_slice(&[0u8; 64]);
    fs::write(&bin, &bytes).unwrap();
    assert!(names_no_daemon(&bin), "across the seam");
    let at = (4 << 20) - 4 + 4;
    assert_eq!(bytes[at], b'a');
    bytes[at] = b'A';
    fs::write(&bin, &bytes).unwrap();
    assert!(!names_no_daemon(&bin), "CONTROL: one byte changed");
    assert!(!names_no_daemon(scratch.path()), "a directory");
}
