// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Config, PRIVACY_WARMUP_HOLD_MS_MAX, PrivacyWarmup};
use crate::matrix_rain::{RAIN_ALPHA_CAP, RAIN_ALPHA_FLOOR, RainConfig, RainHue};

const BG: u32 = 0x0011_1318;
const FG: u32 = 0x00D0_D0D0;
const CURSOR: u32 = 0x0050_FA7B;

fn rain(toml: &str) -> Option<RainConfig> {
    let cfg = aterm_toml::from_str::<Config>(toml).expect("valid toml");
    cfg.matrix_rain_enabled()
        .then(|| cfg.matrix_rain_params(BG, FG, CURSOR))
}

#[test]
fn rain_is_off_by_default_and_params_ignore_the_enabled_bit() {
    assert!(!Config::default().matrix_rain_enabled());
    assert!(rain("[matrix_rain]\nfps = 42").is_none());
    assert!(rain("[matrix_rain]\nenabled = false\nfps = 42").is_none());
    let params = |toml: &str| {
        aterm_toml::from_str::<Config>(toml)
            .unwrap()
            .matrix_rain_params(BG, FG, CURSOR)
    };
    let disabled = params("[matrix_rain]\nenabled = false\nfps = 24");
    assert_eq!(disabled, params("[matrix_rain]\nenabled = true\nfps = 24"));
    assert_eq!(disabled.fps, 24, "knobs still resolve while disabled");
}

#[test]
fn packages_retired_switch_note_and_retired_keys_load() {
    let note = |text: &str| {
        aterm_toml::from_str::<Config>(text)
            .unwrap()
            .packages_retired_switch_note()
    };
    assert_eq!(
        note("[packages]\nauto_update = false"),
        Some(atpkg::config::auto_update_note(false, None))
    );
    assert_eq!(
        note("[packages]\nenabled = true\nauto_update = false"),
        Some(atpkg::config::auto_update_note(false, Some(true)))
    );
    for text in [
        "[packages]\nenabled = false\nauto_update = false",
        "[packages]\nauto_update = true",
        "[packages]\nenabled = false",
        "",
    ] {
        assert_eq!(note(text), None, "{text:?}");
    }
    let retired = "[packages]\nchannel = 7\ninclude = \"x\"\n[packages.links]\nay = \"~/ay\"\n";
    assert!(aterm_toml::from_str::<Config>(retired).is_ok());
}

#[test]
fn packages_defaults_agree_across_the_two_config_readers() {
    for text in [
        "",
        "[packages]\n",
        "[packages]\nenabled = false\n",
        "[packages]\nauto_update = false\n",
        "[packages]\nenabled = true\nauto_update = false\n",
        "[packages]\nauto_update = true\n",
        "[packages]\nauto_install = false\n",
        "[packages]\nseed_install = false\n",
        "[packages]\nseed_install = false\nauto_install = true\n",
        "[packages]\nseed_install = true\nauto_install = false\n",
    ] {
        let gui = aterm_toml::from_str::<Config>(text).unwrap();
        let pkg = atpkg::config::parse_packages(text);
        assert_eq!(gui.packages_enabled(), pkg.enabled(), "enabled: {text:?}");
        assert_eq!(
            gui.packages_auto_install(),
            pkg.auto_install(),
            "auto_install: {text:?}"
        );
    }
    for text in [
        "",
        "[reroute]\nannounce = true\n",
        "[reroute]\nannounce = false\n",
        "[reroute]\nstrict = true\n",
        "[reroute]\nannounce = false\nstrict = false\n",
    ] {
        let gui = aterm_toml::from_str::<Config>(text).unwrap();
        assert_eq!(
            gui.reroute
                .as_ref()
                .and_then(|r| r.announce)
                .unwrap_or(true),
            atpkg::config::parse_reroute(text).announce(),
            "reroute.announce: {text:?}"
        );
        assert_eq!(
            gui.reroute.as_ref().and_then(|r| r.strict).unwrap_or(false),
            atpkg::config::parse_reroute(text).strict(),
            "reroute.strict: {text:?}"
        );
    }
}

#[test]
fn update_enabled_agrees_with_the_updaters_own_reader() {
    for text in [
        "",
        "[update]\n",
        "[update]\nenabled = true\n",
        "[update]\nenabled = false\n",
    ] {
        let gui = aterm_toml::from_str::<Config>(text).unwrap();
        assert_eq!(
            gui.update.as_ref().and_then(|u| u.enabled).unwrap_or(true),
            aterm_update_core::settings::parse_update_enabled(text),
            "{text:?}"
        );
    }
}

#[test]
fn machine_defaults_agree_across_the_two_config_readers() {
    let gui = Config::default();
    let pkg = atpkg::config::MachineConfig::default();
    assert_eq!(gui.machine_universal_control(), pkg.universal_control());
    assert_eq!(gui.machine_rosetta(), pkg.rosetta());
    for text in [
        "[machine]\nuniversal_control = \"leave\"\n",
        "[machine]\nuniversal_control = \"off\"\n",
        "[machine]\nuniversal_control = \"\"\n",
        "[machine]\nrosetta = false\n",
        "[machine]\nrosetta = true\n",
        "[machine]\nuniversal_control = \"leave\"\nrosetta = false\n",
        "[machine]\n",
        "",
    ] {
        let gui = aterm_toml::from_str::<Config>(text).unwrap();
        let pkg = atpkg::config::parse_machine(text);
        assert_eq!(
            gui.machine_universal_control(),
            pkg.universal_control(),
            "{text:?}"
        );
        assert_eq!(gui.machine_rosetta(), pkg.rosetta(), "{text:?}");
    }
    let bogus =
        aterm_toml::from_str::<Config>("[machine]\nuniversal_control = \"bogus\"\n").unwrap();
    assert_eq!(
        bogus.machine_universal_control(),
        atpkg::config::UniversalControlPolicy::Leave,
        "an unknown spelling is the inert reading, never a write on the machine"
    );
}

#[test]
fn privacy_absent_section_offers_warmup_and_observes_as_sentinel() {
    use crate::consent_observer::ObserverMode;
    let c = Config::default();
    assert!(c.privacy_probe_gate().permits());
    assert_eq!(c.privacy_warmup(), PrivacyWarmup::Automatic);
    let mode = |text: &str| {
        aterm_toml::from_str::<Config>(text)
            .expect("parses")
            .privacy_observer_mode()
    };
    assert_eq!(mode(""), ObserverMode::Sentinel, "no [privacy] table");
    assert_eq!(mode("[privacy]\n"), ObserverMode::Sentinel, "key absent");
    assert_eq!(mode("[privacy]\nobserver = false\n"), ObserverMode::Off);
}

#[test]
fn the_default_warmup_omits_app_data_but_an_explicit_list_may_include_it() {
    let absent = Config::default();
    let defaults = absent.privacy_warmup_folders();
    assert_eq!(defaults, vec!["documents", "desktop", "downloads"]);
    let explicit = aterm_toml::from_str::<Config>(
        "[privacy]\nwarmup_folders = [\"documents\", \"app-data\"]\n",
    )
    .expect("explicit list parses");
    assert_eq!(
        explicit.privacy_warmup_folders(),
        vec!["documents".to_string(), "app-data".to_string()]
    );
}

const ABS_TMP: &str = if cfg!(windows) { "C:/tmp" } else { "/tmp" };

#[test]
fn privacy_explicit_values_win_over_the_defaults() {
    let c = aterm_toml::from_str::<Config>(&format!(
        concat!(
            "[privacy]\n",
            "enabled = false\n",
            "check = false\n",
            "notice = false\n",
            "report_attribution = false\n",
            "warmup = \"never\"\n",
            "observer = true\n",
            "protected_roots = [\"{}/aterm-privacy-explicit\"]\n",
            "auto_accept = true\n",
        ),
        ABS_TMP
    ))
    .expect("explicit table parses");
    assert!(
        !c.privacy_probe_gate().permits(),
        "either bit off ⇒ the gate refuses before any syscall"
    );
    assert_eq!(c.privacy_warmup(), PrivacyWarmup::Never);
    assert_eq!(
        c.privacy_observer_mode(),
        crate::consent_observer::ObserverMode::Full,
        "the full stream is opt-in from config, and config alone"
    );
    assert_eq!(
        c.privacy_protected_roots(),
        vec![std::path::PathBuf::from(format!(
            "{ABS_TMP}/aterm-privacy-explicit"
        ))],
        "an absolute override replaces the containment set verbatim"
    );
    assert_eq!(
        c.privacy.as_ref().and_then(|p| p.auto_accept),
        Some(true),
        "the reserved key round-trips so --validate-config can refuse it"
    );

    let over =
        aterm_toml::from_str::<Config>("[privacy]\nwarmup_hold_ms = 3600000\n").expect("parses");
    assert_eq!(
        over.privacy_warmup_hold_ms(),
        PRIVACY_WARMUP_HOLD_MS_MAX,
        "an hour-long hold is clamped, not honored"
    );
    let typo = aterm_toml::from_str::<Config>("[privacy]\nwarmup = \"first-launch\"\n")
        .expect("an unknown spelling must NOT fail the whole file");
    assert_eq!(
        typo.privacy_warmup(),
        PrivacyWarmup::default(),
        "unknown spellings fall back; the validator names them"
    );
    assert_eq!(
        PrivacyWarmup::parse("Automatic"),
        Some(PrivacyWarmup::Automatic)
    );
    assert!(PrivacyWarmup::OnRequest.offered() && PrivacyWarmup::Automatic.offered());
    assert!(!PrivacyWarmup::Never.offered());
}

#[cfg(unix)]
#[test]
fn privacy_empty_protected_roots_resolve_to_the_containment_private_set() {
    let expected: Vec<std::path::PathBuf> = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .map(|home| {
            aterm_containment::sbpl::PRIVATE_SUBDIRS
                .iter()
                .map(|sub| home.join(sub))
                .collect()
        })
        .unwrap_or_default();
    let explicit_empty =
        aterm_toml::from_str::<Config>("[privacy]\nprotected_roots = []\n").expect("parses");
    assert_eq!(explicit_empty.privacy_protected_roots(), expected);
    assert_eq!(
        Config::default().privacy_protected_roots(),
        expected,
        "an absent key resolves the same way an empty one does"
    );
    assert!(
        !expected.is_empty(),
        "this test is only meaningful with a $HOME; the resolver returns \
         empty for 'cannot answer', never for 'nothing is protected'"
    );
}

#[test]
fn privacy_protected_root_overrides_expand_tilde_and_drop_relatives() {
    let c = aterm_toml::from_str::<Config>(&format!(
        "[privacy]\nprotected_roots = [\"~/src\", \"relative/path\", \"{ABS_TMP}/absolute\"]\n"
    ))
    .expect("parses");
    let roots = c.privacy_protected_roots();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let mut expected = Vec::new();
    if let Some(home) = home {
        expected.push(home.join("src"));
    }
    expected.push(std::path::PathBuf::from(format!("{ABS_TMP}/absolute")));
    assert_eq!(
        roots, expected,
        "the relative entry is dropped, not guessed"
    );
}

#[test]
fn clamps_hold_at_both_edges() {
    let lo = rain(concat!(
        "[matrix_rain]\nenabled = true\n",
        "fps = 1\ndensity = 0\nspeed = 0\ntrail = 0\n",
        "alpha = 0\nhead_alpha = 0\nmutation_ms = 1\nidle_secs = 0\n"
    ))
    .unwrap();
    assert!(
        lo.fps > 1
            && lo.density > 0
            && lo.speed > 0
            && lo.trail > 0
            && lo.mutation_ms > 1
            && lo.idle_secs > 0
    );
    assert_eq!(lo.alpha_override, Some(RAIN_ALPHA_FLOOR));
    let hi = rain(concat!(
        "[matrix_rain]\nenabled = true\n",
        "fps = 240\nalpha = 255\nhead_alpha = 255\n"
    ))
    .unwrap();
    assert!(hi.fps <= 60);
    assert_eq!(hi.alpha_override, Some(RAIN_ALPHA_CAP), "READABLE ceiling");
    assert_eq!(hi.head_alpha_override, Some(RAIN_ALPHA_CAP));
}

#[test]
fn head_alpha_floors_at_body_alpha() {
    let c = rain("[matrix_rain]\nenabled = true\nalpha = 120\nhead_alpha = 20").unwrap();
    assert_eq!(c.alpha_override, Some(120));
    assert_eq!(c.head_alpha_override, Some(120));
    let c = rain("[matrix_rain]\nenabled = true\nhead_alpha = 5").unwrap();
    assert_eq!(c.alpha_override, None);
    assert_eq!(c.head_alpha_override, Some(RAIN_ALPHA_FLOOR));
}

#[test]
fn hue_parses_and_bad_hex_fails_closed() {
    let hue = |h: &str| {
        rain(&format!("[matrix_rain]\nenabled = true\nhue = \"{h}\""))
            .expect("hue never aborts the resolve")
            .hue
    };
    assert_eq!(hue("matrix"), RainHue::Matrix);
    assert_eq!(hue("theme"), RainHue::Theme);
    assert_eq!(hue("THEME"), RainHue::Theme, "case-insensitive");
    assert_eq!(hue("#28D75F"), RainHue::Custom(0x0028_D75F));
    assert_eq!(
        hue("#a0b1c2"),
        RainHue::Custom(0x00A0_B1C2),
        "lowercase hex"
    );
    for bad in ["#28D75", "#28D75F0", "#GGGGGG", "chartreuse", "", "#"] {
        assert_eq!(hue(bad), RainHue::Matrix, "{bad:?} must fail closed");
    }
}
