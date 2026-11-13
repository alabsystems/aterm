// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::atomic::{AtomicBool, Ordering};

use super::Rect;
use super::{
    AppRtWindows, CHROME_BG_UNKNOWN, ChromeApplyGuard, ClientBackdropDecline,
    DEFAULT_WHEEL_SCROLL_LINES, DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMSBT_TABBEDWINDOW,
    DWMSBT_TRANSIENTWINDOW, DWMWA_COLOR_DEFAULT, WHEEL_PAGESCROLL, WM_DWMCOLORIZATIONCOLORCHANGED,
    WM_ENDSESSION, WM_NCDESTROY, WM_QUERYENDSESSION, WM_SETTINGCHANGE, WM_SYSCOLORCHANGE,
    WM_THEMECHANGED, WheelNotch, backdrop_for, caption_color_label, caption_tint,
    chrome_policy_code, chrome_policy_from_code, chrome_reassert_needed, claim_os_settings_wake,
    colorref_swap, effective_backdrop, high_contrast_active, is_os_settings_message, os_text_scale,
    resolve_chrome_theme, resolve_forced_chrome_with, transparency_effects_enabled,
    validate_window_capture_transfer, watch_os_settings_message, wheel_notch_from_raw,
    work_area_pts_from_rect,
};
use crate::app_config::{BackgroundMaterial, WindowTheme};
use crate::native_appearance::AppearancePreferences;
use crate::platform::AppRt;

#[test]
fn the_work_area_drops_the_taskbar_and_lands_in_logical_points() {
    let taskbar_at_the_bottom = Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1032,
    };
    let area = work_area_pts_from_rect(&taskbar_at_the_bottom, 1.5).expect("a usable area");
    assert_eq!((area.x, area.y), (0.0, 0.0));
    assert_eq!(area.w, 1280.0);
    assert_eq!(area.h, 688.0);
    let docked_left_on_a_second_display = Rect {
        left: 1920 + 72,
        top: 0,
        right: 1920 + 1280,
        bottom: 1024,
    };
    let area = work_area_pts_from_rect(&docked_left_on_a_second_display, 2.0).expect("usable");
    assert_eq!((area.x, area.y), (996.0, 0.0));
    assert_eq!((area.w, area.h), (604.0, 512.0));
    for (rect, scale) in [
        (Rect::default(), 1.0),
        (
            Rect {
                left: 100,
                top: 100,
                right: 0,
                bottom: 0,
            },
            1.0,
        ),
        (
            Rect {
                left: 0,
                top: 0,
                right: 800,
                bottom: 600,
            },
            0.0,
        ),
        (
            Rect {
                left: 0,
                top: 0,
                right: 800,
                bottom: 600,
            },
            f64::NAN,
        ),
    ] {
        assert!(
            work_area_pts_from_rect(&rect, scale).is_none(),
            "degenerate work area {rect:?}@{scale} must not be cascaded against"
        );
    }
}

#[test]
fn the_settings_hook_watches_exactly_the_appearance_messages() {
    for msg in [
        WM_SETTINGCHANGE,
        WM_SYSCOLORCHANGE,
        WM_THEMECHANGED,
        WM_DWMCOLORIZATIONCOLORCHANGED,
    ] {
        assert!(
            is_os_settings_message(msg),
            "0x{msg:04X} must reach the settings lane"
        );
    }
    for msg in [
        WM_QUERYENDSESSION,
        WM_ENDSESSION,
        WM_NCDESTROY,
        0x0200,
        0x000F,
        0x0100,
        0x0006,
    ] {
        assert!(
            !is_os_settings_message(msg),
            "0x{msg:04X} must NOT post a settings wake"
        );
    }
    assert!(!is_os_settings_message(WM_QUERYENDSESSION));
}

#[test]
fn the_windows_installer_asks_for_a_session_save_before_it_force_kills() {
    let package = aterm_tempfile::manifest_dir();
    let scripts = package
        .parent()
        .and_then(|p| p.parent())
        .expect("the aterm-gui manifest dir has a workspace-root grandparent")
        .join("apps")
        .join("aterm-win");
    let declaration = format!("const uint WM_QUERYENDSESSION = 0x{WM_QUERYENDSESSION:04X};");
    let read = |name: &str| -> String {
        let path = scripts.join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
    };
    for name in ["install.ps1", "uninstall.ps1"] {
        let script = read(name);
        let code = script
            .lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !trimmed.starts_with('#') && !trimmed.starts_with("//")
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            code.contains(&declaration),
            "{name} must declare `{declaration}` — the constant is the only join \
             between the script's P/Invoke and this module's wndproc"
        );
        assert!(
            code.contains("SendMessageTimeoutW(hwnd, WM_QUERYENDSESSION"),
            "{name} must SEND this message at the call site — the P/Invoke merely \
             being declared proves nothing, and a POSTED message would still be \
             sitting in the queue when the script force-kills"
        );
        assert!(
            code.contains("SMTO_NORMAL | SMTO_ABORTIFHUNG,"),
            "{name} must PASS SMTO_ABORTIFHUNG, so a wedged message pump costs the \
             send its timeout instead of blocking the whole script"
        );

        let ask = code
            .find("Request-AtermSessionSave $running")
            .unwrap_or_else(|| {
                panic!("{name} must ask the running instances to save before stopping them")
            });
        let kill = code
            .find("Stop-Process -Force")
            .unwrap_or_else(|| panic!("{name} must still force-kill what it stops"));
        assert!(
            ask < kill,
            "{name} asks for the save at byte {ask}, after the force-kill at \
             {kill} — a terminated process saves nothing"
        );
        assert!(
            !code.contains("CloseMainWindow"),
            "{name} must not use CloseMainWindow(): WM_CLOSE raises aterm's \
             multi-tab close-confirm dialog and would hang the installer"
        );
    }

    let embedded = |name: &str| -> String {
        let script = read(name);
        let start = script
            .find("using System;")
            .unwrap_or_else(|| panic!("{name} must embed the C# session-save helper"));
        let end = script[start..]
            .find("'@")
            .unwrap_or_else(|| panic!("{name}'s embedded helper has no here-string terminator"))
            + start;
        script[start..end].to_string()
    };
    assert_eq!(
        embedded("install.ps1"),
        embedded("uninstall.ps1"),
        "install.ps1 and uninstall.ps1 must embed the SAME helper — uninstall.ps1 \
         is copied standalone into the install dir, so it can share a module with \
         nothing and the copies can only be kept honest here"
    );
}

#[test]
fn aterms_own_chrome_write_does_not_feed_the_settings_hook() {
    assert!(
        watch_os_settings_message(WM_THEMECHANGED),
        "watched normally"
    );
    {
        let _applying = ChromeApplyGuard::new();
        assert!(
            !watch_os_settings_message(WM_THEMECHANGED),
            "our own SetWindowTheme echo must be ignored"
        );
        for msg in [
            WM_SETTINGCHANGE,
            WM_SYSCOLORCHANGE,
            WM_DWMCOLORIZATIONCOLORCHANGED,
        ] {
            assert!(
                watch_os_settings_message(msg),
                "0x{msg:04X} is the OS talking, not us — it must still count"
            );
        }
        {
            let _inner = ChromeApplyGuard::new();
            assert!(!watch_os_settings_message(WM_THEMECHANGED));
        }
        assert!(
            !watch_os_settings_message(WM_THEMECHANGED),
            "the outer guard still holds after an inner one drops"
        );
    }
    assert!(
        watch_os_settings_message(WM_THEMECHANGED),
        "and the watch reopens the moment the write is done"
    );
}

#[test]
fn one_settings_burst_costs_exactly_one_resample() {
    let pending = AtomicBool::new(false);
    let mut wakes = 0_u32;
    let deliver = |msg: u32, wakes: &mut u32| {
        if watch_os_settings_message(msg) && claim_os_settings_wake(&pending) {
            *wakes += 1;
        }
    };

    for msg in [
        WM_SETTINGCHANGE,
        WM_SETTINGCHANGE,
        WM_THEMECHANGED,
        WM_SYSCOLORCHANGE,
        WM_DWMCOLORIZATIONCOLORCHANGED,
        WM_SETTINGCHANGE,
    ] {
        deliver(msg, &mut wakes);
    }
    assert_eq!(wakes, 1, "a burst of six messages is one Wake");

    pending.store(false, Ordering::Release);
    {
        let _applying = ChromeApplyGuard::new();
        deliver(WM_THEMECHANGED, &mut wakes);
    }
    assert_eq!(
        wakes, 1,
        "aterm's own chrome write re-entered the hook and bought a second re-sample"
    );

    deliver(WM_SETTINGCHANGE, &mut wakes);
    assert_eq!(wakes, 2, "the next real burst still wakes the loop");
}

#[test]
fn reduced_transparency_suppresses_the_backdrop_but_never_adds_one() {
    let install = |reduced| {
        crate::native_appearance::install_preferences(AppearancePreferences {
            high_contrast: false,
            reduced_transparency: reduced,
            differentiate_without_color: false,
            text_scale: 1.0,
        });
    };
    let previous = crate::native_appearance::current_preferences();

    install(false);
    for material in [
        DWMSBT_NONE,
        DWMSBT_MAINWINDOW,
        DWMSBT_TRANSIENTWINDOW,
        DWMSBT_TABBEDWINDOW,
    ] {
        assert_eq!(
            effective_backdrop(material),
            material,
            "transparency on: the configured material passes through untouched"
        );
    }

    install(true);
    for material in [
        DWMSBT_MAINWINDOW,
        DWMSBT_TRANSIENTWINDOW,
        DWMSBT_TABBEDWINDOW,
    ] {
        assert_eq!(
            effective_backdrop(material),
            DWMSBT_NONE,
            "transparency off: every translucent material collapses to none"
        );
    }
    assert_eq!(effective_backdrop(DWMSBT_NONE), DWMSBT_NONE);

    crate::native_appearance::install_preferences(previous);
}

/// NEGATIVE CONTROL: with both `high_contrast` and `force_override` false (including `None`, `""`, and `"0"` seam values), no palette is forced.
#[test]
fn the_chrome_palette_is_forced_only_under_high_contrast() {
    use std::ffi::OsStr;

    assert!(!super::force_hc_chrome_value(None));
    assert!(!super::force_hc_chrome_value(Some(OsStr::new(""))));
    assert!(!super::force_hc_chrome_value(Some(OsStr::new("0"))));
    assert!(super::force_hc_chrome_value(Some(OsStr::new("1"))));
    assert!(super::force_hc_chrome_value(Some(OsStr::new("true"))));

    assert_eq!(resolve_forced_chrome_with(false, false), None);
    assert!(resolve_forced_chrome_with(true, false).is_some());
    assert!(resolve_forced_chrome_with(false, true).is_some());
    assert!(resolve_forced_chrome_with(true, true).is_some());
}

#[test]
fn the_registry_backed_preferences_stay_in_contract() {
    let scale = os_text_scale();
    assert!(
        scale.is_finite() && scale > 0.0,
        "text scale must be a usable ratio, got {scale}"
    );
    let normalized = AppearancePreferences {
        high_contrast: false,
        reduced_transparency: false,
        differentiate_without_color: false,
        text_scale: scale,
    }
    .normalized();
    assert!(
        (0.85..=2.0).contains(&normalized.text_scale),
        "the shared clamp bounds the OS value, got {}",
        normalized.text_scale
    );
    for (hc, effects, want) in [
        (false, true, false),
        (false, false, true),
        (true, true, true),
        (true, false, true),
    ] {
        assert_eq!(
            super::reduced_transparency_from(hc, effects),
            want,
            "high_contrast={hc} transparency_effects={effects}"
        );
    }

    let saved_palette = crate::chrome_band::forced_chrome();
    let published = AppRtWindows {
        probe_preferences: true,
    }
    .native_appearance_preferences();
    crate::chrome_band::install_forced_chrome(saved_palette);
    assert_eq!(
        published.reduced_transparency,
        super::reduced_transparency_from(high_contrast_active(), transparency_effects_enabled()),
        "the published preference is the rule applied to the two OS reads"
    );
    assert_eq!(published.high_contrast, high_contrast_active());
}

#[test]
fn the_client_decline_tokens_round_trip_and_zero_means_nothing_declined() {
    for reason in [
        ClientBackdropDecline::HdrGlow,
        ClientBackdropDecline::NoGpu,
        ClientBackdropDecline::DcompUnavailable,
        ClientBackdropDecline::GpuLost,
        ClientBackdropDecline::NotAtLaunch,
    ] {
        let code = reason as u32;
        assert_ne!(code, 0, "no reason may collide with the empty sentinel");
        assert_eq!(ClientBackdropDecline::from_code(code), Some(reason));
        assert!(
            !reason.token().is_empty()
                && reason
                    .token()
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_'),
            "tokens stay greppable: {:?}",
            reason.token()
        );
    }
    assert_eq!(ClientBackdropDecline::from_code(0), None);
    assert_eq!(ClientBackdropDecline::from_code(99), None);
    assert_eq!(ClientBackdropDecline::HdrGlow.token(), "hdr_glow");
}

#[test]
fn material_maps_to_the_dwm_backdrop() {
    assert_eq!(backdrop_for(BackgroundMaterial::None), DWMSBT_NONE);
    assert_eq!(
        backdrop_for(BackgroundMaterial::UnderWindow),
        DWMSBT_MAINWINDOW
    );
    assert_eq!(
        backdrop_for(BackgroundMaterial::Sidebar),
        DWMSBT_TABBEDWINDOW
    );
    assert_eq!(
        backdrop_for(BackgroundMaterial::Hud),
        DWMSBT_TRANSIENTWINDOW
    );
    assert_eq!(
        (
            DWMSBT_NONE,
            DWMSBT_MAINWINDOW,
            DWMSBT_TRANSIENTWINDOW,
            DWMSBT_TABBEDWINDOW
        ),
        (1, 2, 3, 4)
    );
}

#[test]
fn auto_resolves_the_caption_from_the_terminal_background() {
    for bg in [0x00_0000, 0x1e_1e2e, 0x28_2c34, 0x0d_1117] {
        assert_eq!(
            resolve_chrome_theme(WindowTheme::Auto, bg, false),
            WindowTheme::Dark,
            "0x{bg:06x} is a dark terminal background"
        );
    }
    for bg in [0xff_ffff, 0xfd_f6e3, 0xee_eeee] {
        assert_eq!(
            resolve_chrome_theme(WindowTheme::Auto, bg, false),
            WindowTheme::Light,
            "0x{bg:06x} is a light terminal background"
        );
    }
}

#[test]
fn high_contrast_returns_auto_to_the_os() {
    for bg in [0x00_0000, 0xff_ffff, 0x1e_1e2e] {
        assert_eq!(
            resolve_chrome_theme(WindowTheme::Auto, bg, true),
            WindowTheme::Auto,
            "high contrast must defer 0x{bg:06x} to the OS"
        );
        assert_eq!(
            resolve_chrome_theme(WindowTheme::Light, bg, true),
            WindowTheme::Light
        );
        assert_eq!(
            resolve_chrome_theme(WindowTheme::Dark, bg, true),
            WindowTheme::Dark
        );
    }
}

#[test]
fn the_frame_guard_writes_only_on_a_disagreeing_readback() {
    assert!(
        chrome_reassert_needed(true, Some(0)),
        "dark wanted, light applied"
    );
    assert!(
        chrome_reassert_needed(false, Some(1)),
        "light wanted, dark applied"
    );
    assert!(!chrome_reassert_needed(true, Some(1)));
    assert!(!chrome_reassert_needed(false, Some(0)));
    assert!(
        !chrome_reassert_needed(true, None),
        "no readback ⇒ no evidence ⇒ no write"
    );
    assert!(!chrome_reassert_needed(false, None));
}

#[test]
fn explicit_light_and_dark_ignore_the_terminal_background() {
    for bg in [0x00_0000, 0xff_ffff, CHROME_BG_UNKNOWN] {
        assert_eq!(
            resolve_chrome_theme(WindowTheme::Light, bg, false),
            WindowTheme::Light
        );
        assert_eq!(
            resolve_chrome_theme(WindowTheme::Dark, bg, false),
            WindowTheme::Dark
        );
    }
}

#[test]
fn auto_without_a_published_background_stays_auto() {
    assert_eq!(
        resolve_chrome_theme(WindowTheme::Auto, CHROME_BG_UNKNOWN, false),
        WindowTheme::Auto
    );
}

#[test]
fn chrome_policy_latch_round_trips() {
    for theme in [WindowTheme::Auto, WindowTheme::Light, WindowTheme::Dark] {
        assert_eq!(chrome_policy_from_code(chrome_policy_code(theme)), theme);
    }
    assert_eq!(chrome_policy_from_code(0), WindowTheme::Auto);
    assert_eq!(chrome_policy_from_code(200), WindowTheme::Auto);
}

#[test]
fn window_capture_transfer_requires_print_and_every_scanline() {
    assert_eq!(validate_window_capture_transfer(true, 720, 720), Ok(()));
    assert_eq!(
        validate_window_capture_transfer(false, 720, 720).unwrap_err(),
        "PrintWindow failed",
        "GetDIBits-looking bytes cannot redeem a failed PrintWindow"
    );
    assert_eq!(
        validate_window_capture_transfer(true, 0, 720).unwrap_err(),
        "GetDIBits failed"
    );
    assert_eq!(
        validate_window_capture_transfer(true, 719, 720).unwrap_err(),
        "GetDIBits copied 719 of 720 window-capture scanlines",
        "a nonzero partial copy is still an incomplete image"
    );
}

#[test]
fn wheel_notch_decodes_lines_and_the_page_sentinel() {
    assert_eq!(
        wheel_notch_from_raw(DEFAULT_WHEEL_SCROLL_LINES),
        WheelNotch::Lines(3),
        "the Windows default is three lines per detent"
    );
    assert_eq!(wheel_notch_from_raw(1), WheelNotch::Lines(1));
    assert_eq!(wheel_notch_from_raw(0), WheelNotch::Lines(0));
    assert_eq!(wheel_notch_from_raw(WHEEL_PAGESCROLL), WheelNotch::Page);
}

#[test]
fn colorref_swap_reverses_r_and_b_and_is_an_involution() {
    assert_eq!(colorref_swap(0x11_1318), 0x18_1311);
    assert_eq!(colorref_swap(colorref_swap(0xAB_CDEF)), 0xAB_CDEF);
    assert_eq!(
        colorref_swap(0xFF00_0000),
        0,
        "top byte must not leak into a channel"
    );
}

#[test]
fn caption_tint_paints_the_terminal_background_under_auto() {
    let (caption, text) = caption_tint(WindowTheme::Auto, 0x11_1318, DWMSBT_NONE, false, true);
    assert_eq!(
        caption, 0x18_1311,
        "caption = theme bg in COLORREF byte order"
    );
    assert_eq!(text, 0x00FF_FFFF, "dark grid gets white title text");
    let (caption, text) = caption_tint(WindowTheme::Auto, 0xFD_F6E3, DWMSBT_NONE, false, true);
    assert_eq!(caption, 0xE3_F6FD);
    assert_eq!(text, 0x0000_0000, "light grid gets black title text");
}

#[test]
fn caption_text_dims_unfocused_while_the_fill_holds() {
    let bg = 0x11_1318;
    let (cap_focused, text_focused) = caption_tint(WindowTheme::Auto, bg, DWMSBT_NONE, false, true);
    let (cap_unfocused, text_unfocused) =
        caption_tint(WindowTheme::Auto, bg, DWMSBT_NONE, false, false);
    assert_eq!(cap_focused, cap_unfocused, "the fill never dims");
    assert_ne!(text_focused, text_unfocused, "the text must dim");
    assert_ne!(text_unfocused, cap_unfocused, "dimmed is not erased");
    let expected = crate::chrome_band::mix3(
        [0xFF, 0xFF, 0xFF],
        [0x11, 0x13, 0x18],
        super::CAPTION_TEXT_UNFOCUSED_DIM,
    );
    let expected_rgb =
        (u32::from(expected[0]) << 16) | (u32::from(expected[1]) << 8) | u32::from(expected[2]);
    assert_eq!(text_unfocused, colorref_swap(expected_rgb));
}

#[test]
fn caption_tint_defers_on_every_gate() {
    let default = (DWMWA_COLOR_DEFAULT, DWMWA_COLOR_DEFAULT);
    let bg = 0x11_1318;
    for focused in [true, false] {
        assert_eq!(
            caption_tint(WindowTheme::Auto, bg, DWMSBT_NONE, true, focused),
            default,
            "High Contrast owns the caption"
        );
        assert_eq!(
            caption_tint(WindowTheme::Auto, bg, DWMSBT_MAINWINDOW, false, focused),
            default,
            "a solid caption colour would override Mica"
        );
        assert_eq!(
            caption_tint(WindowTheme::Light, bg, DWMSBT_NONE, false, focused),
            default,
            "explicit light keeps the stock OS caption"
        );
        assert_eq!(
            caption_tint(WindowTheme::Dark, bg, DWMSBT_NONE, false, focused),
            default,
            "explicit dark keeps the stock OS caption"
        );
        assert_eq!(
            caption_tint(
                WindowTheme::Auto,
                CHROME_BG_UNKNOWN,
                DWMSBT_NONE,
                false,
                focused
            ),
            default,
            "no published background yet — nothing to tint with"
        );
    }
}

#[test]
fn the_accent_palette_gives_the_winui_shades_and_dwm_backs_it_up() {
    let shades: [[u8; 3]; 8] = [
        [0x99, 0xeb, 0xff],
        [0x4c, 0xc2, 0xff],
        [0x00, 0x91, 0xf8],
        [0x00, 0x78, 0xd4],
        [0x00, 0x67, 0xc0],
        [0x00, 0x3e, 0x92],
        [0x00, 0x1a, 0x68],
        [0xf7, 0x63, 0x0c],
    ];
    let mut palette = [0u8; 32];
    for (i, rgb) in shades.iter().enumerate() {
        palette[i * 4..i * 4 + 3].copy_from_slice(rgb);
    }
    let accent = super::accent_from_palette(Some(palette), Some(0xffd4_7800))
        .expect("a palette is an accent");
    assert_eq!(accent.on_dark, [0x4c, 0xc2, 0xff]);
    assert_eq!(accent.on_light, [0x00, 0x67, 0xc0]);
    let dwm = super::accent_from_palette(None, Some(0xffd4_7800)).expect("DWM's accent");
    assert_eq!(
        (dwm.on_dark, dwm.on_light),
        ([0x00, 0x78, 0xd4], [0x00, 0x78, 0xd4])
    );
    assert_eq!(super::accent_from_palette(None, None), None);
}

#[test]
fn the_caption_color_field_reports_what_was_written_when_dwm_will_not_read_back() {
    let tint = colorref_swap(0x30_3135);
    assert_eq!(caption_color_label(Some(tint), Some(0)), "#303135");
    assert_eq!(caption_color_label(None, Some(tint)), "#303135(set)");
    assert_eq!(
        caption_color_label(None, Some(DWMWA_COLOR_DEFAULT)),
        "default(set)"
    );
    assert_eq!(
        caption_color_label(Some(DWMWA_COLOR_DEFAULT), None),
        "default"
    );
    assert_eq!(caption_color_label(None, None), "n/a");
}

#[test]
fn the_caption_matches_the_band_under_it_and_the_live_background_without_one() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 1;
    let band = crate::tab_bar::strip_bleed_tones(
        app.chrome_palette(crate::app_render::ChromeSurface::Header),
    )
    .expect("Windows paints a band")
    .0;
    assert_eq!(app.caption_surface_tone(wid), band);
    assert_ne!(band, app.theme.bg & 0x00FF_FFFF, "the band is its own tone");
    app.tab_strip_rows = 0;
    app.windows.get_mut(&wid).unwrap().input_scratch.default_bg = 0x002a_00ff;
    assert_eq!(app.caption_surface_tone(wid), 0x002a_00ff);
    app.windows.get_mut(&wid).unwrap().input_scratch.default_bg = aterm_core::render::COLOR_UNSET;
    assert_eq!(app.caption_surface_tone(wid), app.theme.bg & 0x00FF_FFFF);
}
