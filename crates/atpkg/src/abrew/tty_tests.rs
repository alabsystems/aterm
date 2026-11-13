// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn no_color_env_wins_over_force_color() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    };
    assert_eq!(env_color_with(env(&[("NO_COLOR", "1")])), Some(false));
    assert_eq!(
        env_color_with(env(&[("HOMEBREW_NO_COLOR", "0")])),
        Some(false),
        "boolean :set: any present value opts out"
    );
    assert_eq!(
        env_color_with(env(&[("NO_COLOR", "0")])),
        None,
        "bin/brew skips copying NO_COLOR=0"
    );
    assert_eq!(env_color_with(env(&[("NO_COLOR", "")])), None);
    assert_eq!(env_color_with(env(&[("HOMEBREW_NO_COLOR", "  ")])), None);
    assert_eq!(
        env_color_with(env(&[("HOMEBREW_NO_COLOR", ""), ("NO_COLOR", "1")])),
        Some(false),
        "empty HOMEBREW_NO_COLOR lets NO_COLOR through"
    );
    assert_eq!(env_color_with(env(&[("HOMEBREW_COLOR", "1")])), Some(true));
    assert_eq!(env_color_with(env(&[("HOMEBREW_COLOR", "")])), None);
    assert_eq!(
        env_color_with(env(&[("NO_COLOR", "1"), ("HOMEBREW_COLOR", "1")])),
        Some(false)
    );
    assert_eq!(env_color_with(env(&[])), None);
}

#[test]
fn github_actions_annotations_and_colour() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    };
    let gha = env(&[("CI", "true"), ("GITHUB_ACTIONS", "true")]);
    assert_eq!(
        annotation_line_with("error", "Error:", "Error: boom\n100% bad\r\n", &gha),
        Some("::error::boom%0A100%25 bad%0D%0A".to_string())
    );
    assert_eq!(
        annotation_line_with("warning", "Warning:", "Warning: \x1b[33mhey\x1b[0m", &gha),
        Some("::warning::hey".to_string()),
        "labels and ANSI are stripped"
    );
    assert_eq!(env_color_with(gha), Some(true), "brew.sh forces colour");
    // The env filter hides GITHUB_* without CI.
    assert_eq!(
        annotation_line_with(
            "error",
            "Error:",
            "Error: boom",
            &env(&[("GITHUB_ACTIONS", "true")])
        ),
        None
    );
    assert_eq!(env_color_with(env(&[("GITHUB_ACTIONS", "true")])), None);
    // HOMEBREW_TESTS suppresses annotations, not the colour.
    let tests = env(&[
        ("CI", "true"),
        ("GITHUB_ACTIONS", "true"),
        ("HOMEBREW_TESTS", "1"),
    ]);
    assert_eq!(
        annotation_line_with("error", "Error:", "Error: boom", &tests),
        None
    );
    // No-colour opt-outs still beat the Actions colour force.
    assert_eq!(
        env_color_with(env(&[
            ("CI", "true"),
            ("GITHUB_ACTIONS", "true"),
            ("NO_COLOR", "1"),
        ])),
        Some(false)
    );
}

#[test]
fn labels_pass_through_without_the_label() {
    assert_eq!(label_line(true, RED, "Error:", "plain"), "plain");
    assert_eq!(
        label_line(true, RED, "Error:", "Error: boom"),
        "\x1b[31mError:\x1b[0m boom"
    );
    assert_eq!(
        label_line(false, RED, "Error:", "Error: boom"),
        "Error: boom"
    );
}

#[test]
fn headline_shapes_match_brew() {
    assert_eq!(
        headline(true, BLUE, "Caveats"),
        "\x1b[34m==>\x1b[0m \x1b[1mCaveats\x1b[0m"
    );
    assert_eq!(headline(false, BLUE, "Caveats"), "==> Caveats");
}
