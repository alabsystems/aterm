// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `trail-gallery` argument parsing, per-instance configuration, and checkout output path guards.

use super::*;

use std::sync::atomic::{AtomicU32, Ordering};

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "lab-trail-gallery-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    std::fs::canonicalize(&dir).expect("its physical path")
}

#[test]
fn the_defaults_are_typing_on_both_themes_with_clean_twins() {
    let a = parse_args(&args(&["/x/g"])).unwrap();
    assert_eq!(a.out, "/x/g");
    assert!(a.styles.is_empty());
    assert_eq!(a.themes, vec![Theme::Dark, Theme::Light]);
    assert_eq!(a.scenarios, vec![Scenario::Typing]);
    assert!(a.clean);
    assert_eq!(
        (a.columns, a.lines, a.font_px, a.aterm),
        (None, None, None, None)
    );
}

#[test]
fn options_go_anywhere_and_scenarios_repeat_in_order_once() {
    let a = parse_args(&args(&[
        "--scenario",
        "steam,typing",
        "/x/g",
        "phaser",
        "--theme",
        "light",
        "rainbow kitty pet",
        "--scenario",
        "typing,cjk",
        "--extra",
        "motion = \"reduced\"",
        "--columns",
        "80",
        "--no-clean",
        "--aterm",
        "/b/aterm",
    ]))
    .unwrap();
    assert_eq!(a.styles, vec!["phaser", "rainbow kitty pet"]);
    assert_eq!(a.themes, vec![Theme::Light]);
    assert_eq!(
        a.scenarios,
        vec![Scenario::Steam, Scenario::Typing, Scenario::Cjk]
    );
    assert_eq!(a.extras, vec!["motion = \"reduced\""]);
    assert_eq!(a.columns, Some(80));
    assert!(!a.clean);
    assert_eq!(a.aterm.as_deref(), Some("/b/aterm"));
    let all = parse_args(&args(&["--scenario", "all", "/x/g"])).unwrap();
    assert_eq!(all.scenarios, Scenario::ALL.to_vec());
}

#[test]
fn a_malformed_request_is_a_usage_error() {
    for bad in [
        &[][..],
        &["--theme", "sepia", "/x/g"],
        &["--scenario", "typing,nope", "/x/g"],
        &["--extra", "cursor_blink", "/x/g"],
        &["--extra", "[update] = 1", "/x/g"],
        &["--extra", "agents_auto_prime = true", "/x/g"],
        &["--columns", "0", "/x/g"],
        &["--lines", "many", "/x/g"],
        &["--bogus", "/x/g"],
        &["/x/g", "pha\"ser"],
        &["/x/g", "--theme"],
    ] {
        let e = parse_args(&args(bad)).expect_err(&format!("{bad:?} is refused"));
        assert_eq!(e.code, 2, "{bad:?}: {:?}", e.msg);
    }
}

#[test]
fn an_extra_line_replaces_the_base_line_of_its_key() {
    let base = config_lines("phaser", Theme::Dark, &[]);
    assert_eq!(
        base,
        vec![
            "cursor_trail_style = \"phaser\"",
            "motion = \"full\"",
            "trail_sounds = false",
        ]
    );
    let light = config_lines("fire", Theme::Light, &["motion=\"reduced\"".to_string()]);
    assert!(light.contains(&"theme = \"GitHub Light\"".to_string()));
    let motions: Vec<&String> = light.iter().filter(|l| toml_key(l) == "motion").collect();
    assert_eq!(motions, vec!["motion=\"reduced\""]);
    assert_eq!(light.last().map(String::as_str), Some("motion=\"reduced\""));
}

#[test]
fn names_are_the_audit_gallerys() {
    assert_eq!(slug("Rainbow Kitty: pet"), "rainbow-kitty--pet");
    assert_eq!(slug("classic mono"), "classic-mono");
    assert_eq!(
        instance_name("rainbow kitty pet", Theme::Dark, Scenario::Typing),
        "rainbow-kitty-pet-dark"
    );
    assert_eq!(
        instance_name("off", Theme::Light, Scenario::EnterBottom),
        "off-light-enter-bottom"
    );
    for s in Scenario::ALL {
        assert_eq!(Scenario::parse(s.name()), Some(s));
    }
}

#[test]
fn every_paint_ends_in_an_interactive_shell_and_context_paints_its_theme() {
    for s in Scenario::ALL {
        for t in [Theme::Dark, Theme::Light] {
            let p = paint(s, t);
            assert!(
                p.starts_with("LC_ALL=en_US.UTF-8; export LC_ALL\n"),
                "{s:?}"
            );
            assert!(p.ends_with("\nexec /bin/sh -i\n"), "{s:?}");
        }
    }
    assert!(paint(Scenario::Context, Theme::Dark).contains("\\033[48;2;51;65;94m"));
    assert!(paint(Scenario::Context, Theme::Light).contains("\\033[48;2;182;227;255m"));
    assert!(!paint(Scenario::Context, Theme::Light).contains("@SEL@"));
}

#[test]
fn the_roster_is_the_pickers_options() {
    let prefs = "field key=cursor_trail_enabled label=\"Trail\" value=\"true\"\n\
                 field key=cursor_trail_style label=\"Cursor style\" value=\"beam\" effective=\"Beam\" kind=enum \
                 options=[off,rainbow kitty pet,phaser,classic] visible=true\n";
    assert_eq!(
        roster_of(prefs),
        Some(args(&["off", "rainbow kitty pet", "phaser", "classic"]))
    );
    assert_eq!(roster_of("field key=cursor_trail_style options=[]"), None);
    assert_eq!(roster_of("nothing here"), None);
}

#[test]
fn an_output_dir_inside_a_checkout_is_refused_however_it_is_spelled() {
    let root = scratch("out");
    let checkout = root.join("checkout");
    std::fs::create_dir_all(checkout.join("src")).unwrap();
    std::fs::create_dir_all(root.join("outside")).unwrap();
    std::os::unix::fs::symlink(&checkout, root.join("link")).unwrap();
    let co = vec![checkout.clone()];
    let s = |p: PathBuf| p.to_string_lossy().into_owned();

    for inside in [
        checkout.join("g"),
        checkout.join("src/new/deeper"),
        root.join("link/g"),
        root.join("outside/../checkout/g"),
        checkout.clone(),
    ] {
        let e = resolve_out(&s(inside.clone()), &co).expect_err(&format!("{inside:?}"));
        assert!(e.contains("inside the repository"), "{e}");
    }
    assert_eq!(
        resolve_out(&s(root.join("outside/new/deeper")), &co).unwrap(),
        root.join("outside/new/deeper")
    );
    assert_eq!(
        resolve_out(&s(root.join("link/../outside/g")), &co).unwrap(),
        root.join("outside/g"),
        "a link resolves before its `..`, as the kernel reads it"
    );
    let e = resolve_out(&s(root.join("outside/missing/../g")), &co).unwrap_err();
    assert!(e.contains("climbs"), "{e}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_checkouts_are_read_from_gits_own_files() {
    let root = scratch("git");
    let main = root.join("main");
    let wt = root.join("wt");
    let meta = main.join(".git/worktrees/wt");
    std::fs::create_dir_all(&meta).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(
        meta.join("gitdir"),
        format!("{}\n", wt.join(".git").display()),
    )
    .unwrap();
    std::fs::write(meta.join("commondir"), "../..\n").unwrap();
    std::fs::write(wt.join(".git"), format!("gitdir: {}\n", meta.display())).unwrap();

    let from_wt = checkouts(&wt);
    assert!(from_wt.contains(&main), "{from_wt:?}");
    assert!(from_wt.contains(&wt), "{from_wt:?}");
    let from_main = checkouts(&main);
    assert!(from_main.contains(&wt), "{from_main:?}");
    assert_eq!(checkouts(&root), vec![root.clone()]);
    let _ = std::fs::remove_dir_all(&root);
}
