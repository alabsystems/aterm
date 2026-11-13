// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn requirement_displays_render_like_brew() {
    assert_eq!(
        requirement_display(&Requirement::MacosMin(Some("15".into()))),
        "macOS >= 15"
    );
    assert_eq!(requirement_display(&Requirement::MacosMin(None)), "macOS");
    assert_eq!(
        requirement_display(&Requirement::XcodeMin(Some("16.0".into()))),
        "Xcode >= 16.0 (on macOS)"
    );
    let item = aterm_json::from_str(
        r#"{"depends_on":{"arch":[{"type":"arm","bits":64}],"cask":["gstreamer-runtime"],"macos":{">=":["15"]},"maximum_macos":{"<=":["26"]}}}"#,
    )
    .unwrap();
    assert_eq!(
        cask_requirement_displays(&item),
        [
            "arm64 architecture",
            "Gstreamer-runtime",
            "macOS >= 15",
            "macOS <= 26"
        ]
    );
    let exact = aterm_json::from_str(r#"{"depends_on":{"macos":{"==":["11","12"]}}}"#).unwrap();
    assert_eq!(cask_requirement_displays(&exact), ["macOS == 11 / 12"]);
    let legacy = aterm_json::from_str(r#"{"depends_on":{"macos":[">= :sonoma"]}}"#).unwrap();
    assert_eq!(cask_requirement_displays(&legacy), ["macOS >= 14"]);
    let bare = aterm_json::from_str(r#"{"depends_on":{"macos":{}}}"#).unwrap();
    assert_eq!(cask_requirement_displays(&bare), ["macOS"]);
    // The info variant drops the cask-token requirements.
    assert_eq!(
        cask_info_requirement_displays(&item),
        ["arm64 architecture", "macOS >= 15", "macOS <= 26"]
    );
}

#[test]
fn bottles_never_require_a_newer_os_or_another_architecture() {
    let bottle = |tag: &str, url: &str| Bottle {
        tag: tag.into(),
        url: url.into(),
        ..Bottle::default()
    };
    let mut formula = Formula {
        bottle_files: vec![
            bottle("arm64_tahoe", "newer"),
            bottle("arm64_sonoma", "older"),
            bottle("all", "universal"),
        ],
        ..Formula::default()
    };
    let tags = |macos: &str| Machine::mac("arm64", macos, None, false).bottle_tags();
    assert_eq!(formula.bottle_for(&tags("14.6")).unwrap().url, "older");
    assert_eq!(formula.bottle_for(&tags("26.0")).unwrap().url, "newer");
    assert_eq!(
        formula.bottle_for(&tags("unknown")).unwrap().url,
        "universal"
    );
    formula.bottle_files.pop();
    assert!(formula.bottle_for(&tags("unknown")).is_none());
}

#[test]
fn version_ordering_common_shapes() {
    assert_eq!(version_cmp("1.2.3", "1.2.3"), Ordering::Equal);
    assert_eq!(version_cmp("1.10.0", "1.9.9"), Ordering::Greater);
    assert_eq!(version_cmp("1.0", "1.0.0"), Ordering::Equal);
    assert_eq!(version_cmp("2.0rc1", "2.0"), Ordering::Less);
    assert_eq!(version_cmp("2.0beta2", "2.0rc1"), Ordering::Less);
    assert_eq!(version_cmp("1.2.3_1", "1.2.3"), Ordering::Greater);
    assert_eq!(version_cmp("2024.01", "2023.12"), Ordering::Greater);
    assert_eq!(version_cmp("2024-01-15", "2024-02"), Ordering::Less);
    assert_eq!(
        version_cmp("1.0.0-alpha.1", "1.0.0-alpha-2"),
        Ordering::Less
    );
    assert_eq!(version_cmp("1-2", "1.1"), Ordering::Greater);
    // Brew's NumericToken outranks any string token.
    assert_eq!(version_cmp("3.54.1", "latest"), Ordering::Greater);
    assert_eq!(version_cmp("1.0foo", "1.0.1"), Ordering::Less);
    assert_eq!(version_cmp("1.0pre1", "1.0rc1"), Ordering::Less);
}

#[test]
fn native_bottles_require_complete_supported_api_metadata() {
    let mut value: Value = aterm_json::from_str(r#"{"name":"demo","bottle":{"stable":{"files":{"all":{"url":"https://example.invalid/demo.tar.gz","sha256":"hash","cellar":":any_skip_relocation"}}}}}"#).unwrap();
    let native = |f: &Formula| f.native_bottle(&[]).map(|bottle| bottle.tag.clone());
    assert_eq!(
        native(&Formula::from_api(&value).unwrap()).as_deref(),
        Ok("all")
    );
    for (unsupported, needs) in [
        (
            r#"{"name":"demo","post_install_defined":true,"bottle":{"stable":{"files":{"all":{"cellar":":any_skip_relocation"}}}}}"#,
            "a post_install recipe",
        ),
        (
            r#"{"name":"demo","requirements":[{"name":"linux"}],"bottle":{"stable":{"files":{"all":{"cellar":":any_skip_relocation"}}}}}"#,
            "requirements abrew cannot evaluate",
        ),
        (
            r#"{"name":"demo","pour_bottle_only_if":"condition","bottle":{"stable":{"files":{"all":{"cellar":":any_skip_relocation"}}}}}"#,
            "pour_bottle_only_if :condition",
        ),
        (
            r#"{"name":"demo","post_install_steps":[{"step":"x"}],"bottle":{"stable":{"files":{"all":{"cellar":":any_skip_relocation"}}}}}"#,
            "post-install steps",
        ),
        (
            r#"{"name":"demo","bottle":{"stable":{"files":{"arm64_tahoe":{"cellar":":any_skip_relocation"}}}}}"#,
            "no bottle for this machine",
        ),
    ] {
        value = aterm_json::from_str(unsupported).unwrap();
        assert_eq!(
            native(&Formula::from_api(&value).unwrap()),
            Err(needs.to_string())
        );
    }
    value = aterm_json::from_str(
        r#"{"name":"demo","conflicts_with":["other"],"conflicts_with_reasons":["both install `x`"],"requirements":[{"name":"macos","version":"13","contexts":[],"specs":["stable","head"]},{"name":"xcode","contexts":["build","test"]},{"name":"xcode","version":"14.0","contexts":[],"specs":["stable"]}],"pour_bottle_only_if":"clt_installed","bottle":{"stable":{"files":{"all":{"cellar":":any_skip_relocation"}}}}}"#,
    )
    .unwrap();
    let f = Formula::from_api(&value).unwrap();
    assert_eq!(native(&f).as_deref(), Ok("all"));
    assert_eq!(f.conflicts, ["other"]);
    assert_eq!(f.conflict_reasons, ["both install `x`"]);
    assert_eq!(
        f.requirements,
        [
            Requirement::MacosMin(Some("13".into())),
            Requirement::XcodeMin(Some("14.0".into()))
        ]
    );
    assert!(f.pour_requires_clt);
    value = aterm_json::from_str(
        r#"{"name":"demo","dependencies":["base"],"variations":{"arm64_linux":{"dependencies":["linuxonly"]},"x86_64_linux":{"dependencies":["linuxonly"]}},"bottle":{"stable":{"files":{"all":{"cellar":":any_skip_relocation"}}}}}"#,
    )
    .unwrap();
    let mut f = Formula::from_api(&value).unwrap();
    assert_eq!(native(&f).as_deref(), Ok("all"));
    let mut mac = f.clone();
    assert!(mac.resolve_variations_for_keys(&["arm64_tahoe".to_string()]));
    assert_eq!(mac.dependencies, ["base"]);
    assert_eq!(native(&mac).as_deref(), Ok("all"));
    assert!(f.resolve_variations_for_keys(&["arm64_linux".to_string()]));
    assert_eq!(f.dependencies, ["linuxonly"]);
    for field in [
        r#""pour_bottle_only_if":{"unknown":true}"#,
        r#""conflicts_with":{"formula":["other"]}"#,
        r#""requirements":[{"name":"macos","contexts":[42]}]"#,
    ] {
        let malformed: Value = aterm_json::from_str(&format!(
            r#"{{"name":"demo",{field},"bottle":{{"stable":{{"files":{{"all":{{"cellar":":any_skip_relocation"}}}}}}}}}}"#
        )).unwrap();
        assert!(native(&Formula::from_api(&malformed).unwrap()).is_err());
    }
    for variation in [r#"[42]"#, r#"{"arm64_tahoe":{"dependencies":[42]}}"#] {
        let malformed: Value = aterm_json::from_str(&format!(
            r#"{{"name":"demo","versions":{{"stable":"1"}},"revision":0,"dependencies":[],"variations":{variation},"bottle":{{"stable":{{"files":{{"all":{{"cellar":":any_skip_relocation"}}}}}}}}}}"#
        )).unwrap();
        let mut parsed = Formula::from_api(&malformed).unwrap();
        assert!(!parsed.resolve_variations_for_keys(&["arm64_tahoe".to_string()]));
        assert!(native(&parsed).is_err());
    }
    let malformed: Value = aterm_json::from_str(r#"{"name":"demo","versions":{"stable":"1"},"revision":0,"dependencies":[],"variations":[42]}"#).unwrap();
    assert!(super::super::api::dependency_formula(&malformed, "demo", Machine::this()).is_err());
    let complete: Value = aterm_json::from_str(r#"{"name":"demo","versions":{"stable":"1"},"revision":0,"dependencies":[],"variations":{}}"#).unwrap();
    assert!(super::super::api::dependency_formula(&complete, "demo", Machine::this()).is_ok());
    for cellar in ["/opt/homebrew/Cellar", ""] {
        value = aterm_json::from_str(&format!(r#"{{"name":"demo","bottle":{{"stable":{{"files":{{"all":{{"cellar":"{cellar}"}}}}}}}}}}"#)).unwrap();
        assert_eq!(
            native(&Formula::from_api(&value).unwrap()),
            Err(format!("its bottle relocates: cellar {cellar}"))
        );
    }
    value = aterm_json::from_str(
        r#"{"name":"demo","bottle":{"stable":{"files":{"all":{"cellar":":any"}}}}}"#,
    )
    .unwrap();
    if cfg!(target_os = "macos") {
        assert_eq!(
            native(&Formula::from_api(&value).unwrap()),
            Ok("all".to_string())
        );
    } else {
        assert_eq!(
            native(&Formula::from_api(&value).unwrap()),
            Err("its bottle relocates: cellar :any".to_string())
        );
    }
}

#[test]
fn cask_options_and_constraints_survive_as_unsupported_work() {
    let simple: Value =
        aterm_json::from_str(r#"{"token":"demo","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","artifacts":[{"app":["Demo.app"]}]}"#).unwrap();
    assert_eq!(
        Cask::from_api(&simple).unwrap().artifacts,
        [Artifact::App("Demo.app".into(), None)]
    );
    let dmg: Value = aterm_json::from_str(r#"{"token":"demo","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","container":{"type":"dmg"},"artifacts":[{"app":["Demo.app"]}]}"#).unwrap();
    let dmg = Cask::from_api(&dmg).unwrap();
    assert!(dmg.unsupported_constraints.is_none());
    assert_eq!(dmg.artifacts, [Artifact::App("Demo.app".into(), None)]);
    for json in [
        r#"{"token":"demo","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","depends_on":{"macos":[">= :sonoma"]}}"#,
        r#"{"token":"demo","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","container":{"type":"dmg","nested":"x"}}"#,
    ] {
        let value: Value = aterm_json::from_str(json).unwrap();
        assert!(
            Cask::from_api(&value)
                .unwrap()
                .artifacts
                .iter()
                .any(|artifact| matches!(artifact, Artifact::Other(_)))
        );
    }
    for json in [
        r#"{"token":"demo","artifacts":[{"app":["Demo.app"]}]}"#,
        r#"{"token":"demo","sha256":"short","artifacts":[{"app":["Demo.app"]}]}"#,
    ] {
        let value: Value = aterm_json::from_str(json).unwrap();
        assert!(Cask::from_api(&value).is_err());
    }
    let constrained: Value = aterm_json::from_str(&format!(
        r#"{{"token":"demo","sha256":"{}","depends_on":{{"macos":{{">=":["12"]}}}},"conflicts_with":{{"cask":["other"],"formula":["wget"]}},"artifacts":[]}}"#,
        "a".repeat(64)
    ))
    .unwrap();
    let cask = Cask::from_api(&constrained).unwrap();
    assert_eq!(cask.macos_min.as_deref(), Some("12"));
    assert_eq!(cask.conflicts, ["other", "wget"]);
    assert!(cask.artifacts.is_empty());
    let unbounded: Value = aterm_json::from_str(&format!(
        r#"{{"token":"demo","sha256":"{}","depends_on":{{"macos":{{}}}},"artifacts":[]}}"#,
        "a".repeat(64)
    ))
    .unwrap();
    let cask = Cask::from_api(&unbounded).unwrap();
    assert_eq!(cask.macos_min, None);
    assert!(cask.artifacts.is_empty());
    let arched: Value = aterm_json::from_str(&format!(
        r#"{{"token":"demo","sha256":"{}","depends_on":{{"arch":[{{"type":"arm","bits":64}}],"macos":{{">=":["14"]}}}},"artifacts":[{{"binary":["$APPDIR/Demo.app/Contents/MacOS/demo",{{"target":"demo"}}]}}]}}"#,
        "a".repeat(64)
    ))
    .unwrap();
    let cask = Cask::from_api(&arched).unwrap();
    assert_eq!(cask.arch.as_deref(), Some("arm64"));
    assert_eq!(cask.macos_min.as_deref(), Some("14"));
    assert_eq!(
        cask.artifacts,
        [Artifact::Binary(
            "/Applications/Demo.app/Contents/MacOS/demo".into(),
            Some("demo".into())
        )]
    );
    let varied: Value = aterm_json::from_str(&format!(
        r#"{{"token":"demo","sha256":"{}","url":"https://x/base.zip","artifacts":[],"variations":{{"arm64_sonoma":{{"url":"https://x/sonoma.zip"}},"tahoe":{{"url":"https://x/tahoe.zip","sha256":"{}"}}}}}}"#,
        "a".repeat(64),
        "b".repeat(64)
    ))
    .unwrap();
    let cask = Cask::from_api(&varied).unwrap();
    assert_eq!(cask.variations.len(), 2);
    assert!(
        cask.variation_for(&Machine::mac("arm64", "26.0", None, false))
            .is_none()
    );
    assert_eq!(
        cask.variation_for(&Machine::mac("x86_64", "26.0", None, false))
            .and_then(|v| v.url.as_deref()),
        Some("https://x/tahoe.zip")
    );
    assert!(
        cask.variation_for(&Machine::mac("arm64", "27.0", None, false))
            .is_none()
    );
    assert_eq!(
        cask.variation_for(&Machine::mac("arm64", "14.0", None, false))
            .and_then(|v| v.url.as_deref()),
        Some("https://x/sonoma.zip")
    );
    assert!(
        cask.variation_for(&Machine::mac("x86_64", "13.0", None, false))
            .is_none()
    );
    let real: Value = aterm_json::from_str(&format!(
        r#"{{"token":"demo","sha256":"{}","artifacts":[{{"uninstall":[{{"quit":"com.x"}}]}},{{"font":["ttf/F.ttf"],"target":"/$HOME/Library/Fonts/F.ttf"}},{{"zap":[{{"trash":["~/Library/Caches/x"]}}]}}]}}"#,
        "a".repeat(64)
    ))
    .unwrap();
    assert_eq!(
        Cask::from_api(&real).unwrap().artifacts,
        [Artifact::Font("ttf/F.ttf".into(), Some("F.ttf".into()))]
    );
    let kinds: Value = aterm_json::from_str(&format!(
        r#"{{"token":"demo","sha256":"{}","artifacts":[{{"suite":["SuiteDir"]}},{{"keyboard_layout":["K.bundle"]}},{{"screen_saver":["S.saver"]}},{{"prefpane":["P.prefPane"]}},{{"service":["W.workflow"]}},{{"artifact":["tree",{{"target":"$HOMEBREW_PREFIX/share/demo/tree"}}]}},{{"binary":["$HOMEBREW_PREFIX/share/demo/tree/bin/tool"]}}]}}"#,
        "a".repeat(64)
    ))
    .unwrap();
    let wrapper: Value = aterm_json::from_str(&format!(
        r#"{{"token":"demo","sha256":"{}","artifacts":[{{"command_wrapper":["demo",{{"executable":"$APPDIR/Demo.app/Contents/MacOS/demo","args":["--fixed"]}}],"target":"$HOMEBREW_PREFIX/bin/demo"}}]}}"#,
        "a".repeat(64)
    ))
    .unwrap();
    assert_eq!(
        Cask::from_api(&wrapper).unwrap().artifacts,
        [Artifact::Wrapper(
            "demo".into(),
            "/Applications/Demo.app/Contents/MacOS/demo".into(),
            vec!["--fixed".to_string()],
            Some("demo".into())
        )]
    );
    assert_eq!(
        Cask::from_api(&kinds).unwrap().artifacts,
        [
            Artifact::Suite("SuiteDir".into(), None),
            Artifact::KeyboardLayout("K.bundle".into(), None),
            Artifact::ScreenSaver("S.saver".into(), None),
            Artifact::PrefPane("P.prefPane".into(), None),
            Artifact::Service("W.workflow".into(), None),
            Artifact::Generic(
                "tree".into(),
                Some("$HOMEBREW_PREFIX/share/demo/tree".into())
            ),
            Artifact::Binary("$HOMEBREW_PREFIX/share/demo/tree/bin/tool".into(), None),
        ]
    );
    for artifact in [
        r#"42"#,
        r#"{"artifact":["src",{"target":"$HOMEBREW_PREFIX/lib/x"}],"target":"$HOMEBREW_PREFIX/bin/x"}"#,
        r#"{"binary":["tool",{"chmod":"0755"}]}"#,
        r#"{"binary":["tool",{"target":42}]}"#,
        r#"{"binary":["tool",{"chmod":"0755","extra":true}],"target":"renamed"}"#,
        r#"{"binary":["tool"],"target":42}"#,
        r#"{"binary":["tool",{"target":"first"}],"target":"second"}"#,
    ] {
        let unsupported: Value = aterm_json::from_str(&format!(
            r#"{{"token":"demo","sha256":"no_check","artifacts":[{artifact}]}}"#
        ))
        .unwrap();
        assert!(
            Cask::from_api(&unsupported)
                .unwrap()
                .artifacts
                .iter()
                .any(|artifact| matches!(artifact, Artifact::Other(_)))
        );
    }
    let equivalent: Value = aterm_json::from_str(r#"{"token":"demo","sha256":"no_check","artifacts":[{"artifact":["src",{"target":"$HOMEBREW_PREFIX/Library/Services/x"}],"target":"/$HOME/Library/Services/x"},{"binary":["tool",{"target":"x"}],"target":"$HOMEBREW_PREFIX/bin/x"}]}"#).unwrap();
    assert_eq!(
        Cask::from_api(&equivalent).unwrap().artifacts,
        [
            Artifact::Generic(
                "src".to_string(),
                Some("/$HOME/Library/Services/x".to_string())
            ),
            Artifact::Binary("tool".to_string(), Some("x".to_string())),
        ]
    );
    let per_system: Value = aterm_json::from_str(r#"{"token":"demo","sha256":"no_check","artifacts":[{"app":["Demo.app"]}],"variations":{"arm64_tahoe":{"unknown_recipe":true}}}"#).unwrap();
    let per_system = Cask::from_api(&per_system).unwrap();
    assert_eq!(per_system.unsupported_constraints, None);
    assert!(
        per_system
            .variation_for(&Machine::mac("arm64", "27.0", None, false))
            .is_none()
    );
    assert!(
        per_system
            .variation_for(&Machine::mac("arm64", "26.0", None, false))
            .unwrap()
            .unsupported
    );
    let naked: Value = aterm_json::from_str(r#"{"token":"demo","sha256":"no_check","container":{"type":"naked"},"artifacts":[{"binary":["tool"]}]}"#).unwrap();
    let naked = Cask::from_api(&naked).unwrap();
    assert!(naked.naked && naked.unsupported_constraints.is_none());
    assert_eq!(
        naked.artifacts,
        [Artifact::Binary("tool".to_string(), None)]
    );
    for container in [
        r#"{"type":"naked","nested":"archive"}"#,
        r#"{"type":"naked","custom":true}"#,
    ] {
        let rich: Value = aterm_json::from_str(&format!(r#"{{"token":"demo","sha256":"no_check","container":{container},"artifacts":[{{"binary":["tool"]}}]}}"#)).unwrap();
        let rich = Cask::from_api(&rich).unwrap();
        assert!(!rich.naked && rich.unsupported_constraints == Some("container"));
        assert!(
            rich.artifacts
                .iter()
                .any(|artifact| matches!(artifact, Artifact::Other(_)))
        );
    }
    let waived: Value =
        aterm_json::from_str(r#"{"token":"demo","sha256":"no_check","artifacts":[]}"#).unwrap();
    assert_eq!(Cask::from_api(&waived).unwrap().sha256, None);
}

#[test]
fn formula_parse_from_api_shape() {
    let v: Value = aterm_json::from_str(
        r#"{"name":"wget","full_name":"wget","desc":"Get","homepage":"https://x","license":"GPL-3.0",
                "versions":{"stable":"1.25.0"},"urls":{"stable":{"url":"https://y/wget.tar.gz","checksum":"abc"}},
                "bottle":{"stable":{"files":{"arm64_tahoe":{"url":"https://ghcr/b","sha256":"def"}}}},
                "dependencies":["openssl@3"],"build_dependencies":["pkgconf"],"keg_only":false}"#,
    )
    .unwrap();
    let f = Formula::from_api(&v).unwrap();
    assert_eq!(f.version, "1.25.0");
    assert_eq!(f.dependencies, ["openssl@3"]);
    assert_eq!(
        f.bottle_for(&["arm64_tahoe".to_string()]).unwrap().sha256,
        "def"
    );
    assert_ne!(token_cmp("+10", "10"), Ordering::Equal);
}
