// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-open-url`: the profile's directory read from Chrome's own state,
//! the command it runs, and every refusal said without the URL.

use super::*;

/// Chrome's `Local State`, as much of it as the opener reads, for three
/// profiles: `Default` ("Person 1"), `Profile 2` ("Claude m7-a"), `Profile 3`
/// ("Twin") and `Profile 4` ("Twin").
const LOCAL_STATE: &str = r#"{
  "browser": {"enabled_labs_experiments": []},
  "profile": {
    "info_cache": {
      "Default": {"name": "Person 1", "user_name": "x@example.com"},
      "Profile 2": {"name": "Claude m7-a"},
      "Profile 3": {"name": "Twin"},
      "Profile 4": {"name": "Twin"}
    },
    "last_used": "Default"
  }
}"#;

/// A URL of the shape Claude Code hands its opener (the hint an example.com
/// address).
const URL: &str = "https://claude.ai/oauth/authorize?login_hint=a%40example.com&state=s";

#[test]
fn a_profile_is_found_by_its_display_name_and_only_when_it_is_one() {
    assert_eq!(
        profile_dir_of(LOCAL_STATE, "Claude m7-a").as_deref(),
        Some("Profile 2")
    );
    assert_eq!(
        profile_dir_of(LOCAL_STATE, "Person 1").as_deref(),
        Some("Default")
    );
    // The directory's own name is no display name.
    assert_eq!(profile_dir_of(LOCAL_STATE, "Profile 2"), None);
    // Two of one name: which the owner meant is not known.
    assert_eq!(profile_dir_of(LOCAL_STATE, "Twin"), None);
    assert_eq!(profile_dir_of(LOCAL_STATE, "Nobody"), None);
    assert_eq!(profile_dir_of("not json", "Claude m7-a"), None);
    // A directory name that would be a path or an option is none.
    let odd = r#"{"profile":{"info_cache":{"../x":{"name":"A"},"-y":{"name":"B"}}}}"#;
    assert_eq!(profile_dir_of(odd, "A"), None);
    assert_eq!(profile_dir_of(odd, "B"), None);
}

#[test]
fn the_url_opens_in_that_profile_and_is_never_said() {
    let mut ran = Vec::new();
    let (code, said) = open_with(
        &[URL.to_string()],
        Some("Claude m7-a"),
        &|| Some(LOCAL_STATE.to_string()),
        &mut |cmd| {
            ran.push((
                cmd.get_program().to_string_lossy().into_owned(),
                cmd.get_args()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
            ));
            true
        },
    );
    if !cfg!(target_os = "macos") {
        assert_eq!(code, 1);
        assert!(ran.is_empty());
        return;
    }
    assert_eq!((code, said), (0, None));
    assert_eq!(
        ran,
        vec![(
            "open".to_string(),
            vec![
                "-na".to_string(),
                "Google Chrome".to_string(),
                "--args".to_string(),
                "--profile-directory=Profile 2".to_string(),
                URL.to_string(),
            ]
        )]
    );
}

/// One refusal case: the opener's arguments, the profile named, Chrome's
/// state as read, and the exit wanted.
type Case<'a> = (
    &'a [String],
    Option<&'a str>,
    &'a dyn Fn() -> Option<String>,
    u8,
);

/// Every failure is exit 1 (a usage error 2) with one line that names no
/// URL, and nothing run: Claude Code then prints the URL itself.
#[test]
fn every_refusal_is_exit_one_and_names_no_url() {
    let mut never = |_cmd: aterm_uds::spawnfd::Command| -> bool {
        panic!("nothing is run");
    };
    let state = || Some(LOCAL_STATE.to_string());
    let none = || None;
    let cases: [Case<'_>; 6] = [
        (&[], Some("Claude m7-a"), &state, 2),
        (
            &["http://claude.ai/x".to_string()],
            Some("Claude m7-a"),
            &state,
            1,
        ),
        (
            &["https://claude.ai/x y".to_string()],
            Some("Claude m7-a"),
            &state,
            1,
        ),
        (&[URL.to_string()], None, &state, 1),
        (&[URL.to_string()], Some("Claude m7-a"), &none, 1),
        (&[URL.to_string()], Some("Twin"), &state, 1),
    ];
    for (args, profile, local_state, want) in cases {
        let (code, said) = open_with(args, profile, local_state, &mut never);
        assert_eq!(code, want, "{args:?} {profile:?}");
        let said = said.expect("a line");
        assert!(said.starts_with(OPENER), "{said}");
        assert!(!said.contains("claude.ai"), "the URL is never said: {said}");
    }
    // `open` refusing is the same.
    if cfg!(target_os = "macos") {
        let (code, said) = open_with(&[URL.to_string()], Some("Claude m7-a"), &state, &mut |_| {
            false
        });
        assert_eq!(code, 1);
        assert!(!said.expect("a line").contains("claude.ai"));
    }
}
