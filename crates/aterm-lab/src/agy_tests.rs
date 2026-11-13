// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn v(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn a_sign_in_never_runs_in_the_lab() {
    for bad in ["login", "logout", "auth"] {
        assert!(safe_args(&v(&["-p", "x", bad])).is_err(), "{bad}");
    }
    assert!(safe_args(&v(&["-p", "hi"])).is_ok());
    assert!(safe_args(&v(&["-s"])).is_ok());
}

/// `--setting KEY=VALUE` sets one of agy's own keys beside the seed's, the
/// value read as JSON where it is JSON.
#[test]
fn an_agy_setting_is_seeded_beside_the_provider() {
    let l = parse_launch(&v(&["--setting", "hideActiveItems=true"])).unwrap();
    assert_eq!(
        l.settings,
        vec![("hideActiveItems".to_string(), J::Bool(true))]
    );
    let seeded = J::parse(&agy_settings_with(&v(&["/p"]), &l.settings)).unwrap();
    assert_eq!(seeded.get("hideActiveItems"), Some(&J::Bool(true)));
    assert_eq!(
        seeded.get("modelProvider").and_then(J::as_str),
        Some("gemini")
    );
    assert_eq!(
        setting_of("theme=dark"),
        Some(("theme".into(), J::s("dark")))
    );
    for bad in ["modelProvider=google", "noequals", "=true"] {
        assert_eq!(setting_of(bad), None, "{bad}");
        assert!(parse_launch(&v(&["--setting", bad])).is_err(), "{bad}");
    }
    let kept = J::parse(&agy_settings_with(
        &[],
        &[("modelProvider".to_string(), J::s("google"))],
    ))
    .unwrap();
    assert_eq!(
        kept.get("modelProvider").and_then(J::as_str),
        Some("gemini")
    );
}

#[test]
fn options_parse() {
    let l = parse_launch(&v(&["-e", "K=v 1", "--onboarding", "--", "-p", "hi"])).unwrap();
    assert!(l.onboarding && l.untrusted && !l.no_wait);
    assert_eq!((l.envs, l.args), (v(&["K=v 1"]), v(&["-p", "hi"])));
    let g = parse_launch(&v(&["--untrusted", "--no-wait"])).unwrap();
    assert!(g.untrusted && g.no_wait && !g.onboarding);
    assert!(parse_launch(&v(&["-e"])).is_err());
    assert!(
        parse_launch(&v(&["--sandbox"])).is_err(),
        "no option of agy's"
    );
}

/// The seeds keep every sign-in away: agy's provider is the Gemini API
/// whether trusted or not; only the trust seed moves.
#[test]
fn the_seeds_keep_the_auth_and_move_only_the_trust() {
    let trusted = J::parse(&agy_settings(&v(&["/private/tmp/at.X/proj"]))).unwrap();
    assert_eq!(
        trusted.get("modelProvider").and_then(J::as_str),
        Some("gemini")
    );
    assert_eq!(
        trusted
            .get("trustedWorkspaces")
            .and_then(J::as_arr)
            .map(<[J]>::len),
        Some(1)
    );
    let open = J::parse(&agy_settings(&[])).unwrap();
    assert_eq!(
        open.get("modelProvider").and_then(J::as_str),
        Some("gemini")
    );
    assert!(open.get("trustedWorkspaces").is_none());
    assert_eq!(
        J::parse(&agy_onboarded())
            .unwrap()
            .get("onboardingComplete"),
        Some(&J::Bool(true))
    );
}

#[test]
fn the_launcher_execs_agy_by_path_with_the_fake_key_and_the_stub() {
    let run = Run {
        tag: "t".into(),
        run: "/s/runs/t".into(),
        w: "/s/runs/t/scratch".into(),
        port: "5".into(),
        proxy: "http://127.0.0.1:6".into(),
        agy_bin: "/a b/agy".into(),
        ..Run::default()
    };
    let a = launcher(&run, &v(&["K=v 1"]), &v(&["-m", "x y"]));
    assert!(
        a.starts_with("#!/bin/sh\nclear\nexec env -u GOOGLE_API_KEY"),
        "{a}"
    );
    assert!(
        a.contains(
            "GEMINI_API_KEY=lab-gemini-not-a-key GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:5 \\\n"
        ),
        "{a}"
    );
    assert!(a.contains("HOME=/s/runs/t/scratch/agy-home"), "{a}");
    assert!(
        a.contains("HTTPS_PROXY=http://127.0.0.1:6") && a.contains("NO_PROXY=127.0.0.1,localhost"),
        "{a}"
    );
    assert!(a.contains("AGY_CLI_DISABLE_AUTO_UPDATE=true"), "{a}");
    assert!(a.ends_with("  'K=v 1' \\\n  '/a b/agy' -m 'x y'\n"), "{a}");
}
