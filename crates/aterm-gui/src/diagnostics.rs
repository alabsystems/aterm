// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `--diagnose`: a headless diagnostics ("doctor") report — version/build,
//! platform, renderer, advertised terminal capabilities, config location, and the
//! active `ATERM_*` environment. The report body is a PURE function of a captured
//! [`DiagInfo`], so it is unit-tested without launching a window; printing it and
//! exiting is the CLI's job (`cli.rs`). This is the discoverable surface a human or
//! an AI reads to understand what this build supports and how it is configured.

use std::fmt::Write as _;

/// A captured snapshot of what the diagnostics report prints. Built from the live
/// build + environment by [`collect`]; constructed directly by tests.
pub(crate) struct DiagInfo {
    pub version: String,
    pub git_commit: &'static str,
    pub build_time: &'static str,
    pub target_os: &'static str,
    pub target_arch: &'static str,
    pub profile: &'static str,
    /// Compiler provenance line (which rustc produced this binary — flavor r/t,
    /// commit slug, profile, trust_verify), from `build_info::compiler_summary`.
    pub compiler: String,
    /// SHA-256(raw 32-byte compiled updater key), or the explicit `empty` /
    /// `invalid` state. Read from shipping updater code, not binary text.
    pub update_pin_sha256: String,
    pub renderer_default: &'static str,
    pub features: Vec<(&'static str, bool)>,
    pub capabilities: Vec<(&'static str, bool)>,
    pub config_path: String,
    pub config_exists: bool,
    pub env: Vec<(String, String)>,
}

fn checkbox(on: bool) -> char {
    if on { 'x' } else { ' ' }
}

impl DiagInfo {
    /// Render the stable, sectioned report.
    pub(crate) fn render(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "aterm diagnostics");
        let _ = writeln!(s, "=================");
        let _ = writeln!(
            s,
            "version:   {} ({}, built {})",
            self.version, self.git_commit, self.build_time
        );
        let _ = writeln!(
            s,
            "build:     {} / {}-{}",
            self.profile, self.target_os, self.target_arch
        );
        let _ = writeln!(s, "compiler:  {}", self.compiler);
        let _ = writeln!(s, "update-pin-sha256: {}", self.update_pin_sha256);
        let _ = writeln!(s, "renderer:  {}", self.renderer_default);
        let _ = writeln!(
            s,
            "config:    {} [{}]",
            self.config_path,
            if self.config_exists {
                "present"
            } else {
                "absent — defaults"
            }
        );
        let _ = writeln!(s);
        let _ = writeln!(s, "features:");
        for (name, on) in &self.features {
            let _ = writeln!(s, "  [{}] {name}", checkbox(*on));
        }
        let _ = writeln!(s);
        let _ = writeln!(s, "terminal capabilities (advertised):");
        for (name, on) in &self.capabilities {
            let _ = writeln!(s, "  [{}] {name}", checkbox(*on));
        }
        let _ = writeln!(s);
        if self.env.is_empty() {
            let _ = writeln!(s, "environment: (no ATERM_* variables set)");
        } else {
            let _ = writeln!(s, "environment:");
            for (k, v) in &self.env {
                let _ = writeln!(s, "  {k}={v}");
            }
        }
        s
    }
}

/// The advertised terminal capabilities, enumerated as `(name, advertised)` from
/// the single source of truth (`aterm_capabilities()`).
fn capability_list() -> Vec<(&'static str, bool)> {
    let c = aterm_types::TerminalCapabilities::aterm_capabilities();
    vec![
        ("true_color", c.true_color),
        ("color_256", c.color_256),
        ("hyperlinks", c.hyperlinks),
        ("sixel_graphics", c.sixel_graphics),
        ("iterm_images", c.iterm_images),
        ("kitty_graphics", c.kitty_graphics),
        ("clipboard", c.clipboard),
        ("shell_integration", c.shell_integration),
        ("synchronized_output", c.synchronized_output),
        ("kitty_keyboard", c.kitty_keyboard),
        ("soft_fonts", c.soft_fonts),
        ("unicode", c.unicode),
        ("bracketed_paste", c.bracketed_paste),
        ("focus_reporting", c.focus_reporting),
        ("mouse_tracking", c.mouse_tracking),
        ("alternate_screen", c.alternate_screen),
    ]
}

/// The GPU renderer's backend name for the diagnostics label, per platform: wgpu
/// negotiates Metal on macOS and Vulkan elsewhere (Vulkan on Linux/NVIDIA). The live
/// `metrics` verb still reports the actually-negotiated backend at runtime.
#[cfg(target_os = "macos")]
const GPU_BACKEND_LABEL: &str = "gpu (metal)";
/// See the macOS variant above — non-macOS uses Vulkan via wgpu.
#[cfg(not(target_os = "macos"))]
const GPU_BACKEND_LABEL: &str = "gpu (vulkan)";

/// The renderer label for the reports: the platform GPU backend name when GPU is
/// the resolved default, else the CPU renderer. `gpu` must come from
/// [`crate::app_config::resolve_want_gpu`] so the label tracks the actual backend
/// selection (env + config `gpu` + GPU-on default), not env alone.
fn renderer_label(gpu: bool) -> &'static str {
    if gpu { GPU_BACKEND_LABEL } else { "cpu" }
}

/// Collect diagnostics from the live build + environment.
pub(crate) fn collect() -> DiagInfo {
    // Renderer default resolved through the shared funnel (env > config `gpu` >
    // GPU-on default) so the report matches what `main` would actually launch. wgpu
    // negotiates Metal on macOS, Vulkan elsewhere; the live `metrics` verb reports
    // the actually-negotiated backend.
    let gpu = crate::app_config::resolve_want_gpu(&crate::app_config::load_config());
    let renderer_default = renderer_label(gpu);

    let (config_path, config_exists) = match crate::app_config::config_path() {
        Some(p) => {
            let exists = p.exists();
            (p.display().to_string(), exists)
        }
        None => ("(no HOME / XDG_CONFIG_HOME)".to_string(), false),
    };

    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("ATERM_"))
        .collect();
    env.sort();

    DiagInfo {
        version: crate::build_info::version_display().to_string(),
        git_commit: crate::build_info::GIT_COMMIT,
        build_time: crate::build_info::BUILD_TIME,
        target_os: std::env::consts::OS,
        target_arch: std::env::consts::ARCH,
        profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        compiler: crate::build_info::compiler_summary(),
        update_pin_sha256: aterm_update::compiled_update_pin_sha256(),
        renderer_default,
        features: vec![
            ("sixel", cfg!(feature = "sixel")),
            ("a11y-appkit", cfg!(feature = "a11y-appkit")),
            ("a11y-accesskit", cfg!(feature = "a11y-accesskit")),
        ],
        capabilities: capability_list(),
        config_path,
        config_exists,
        env,
    }
}

/// Parse `text` as the `aterm.toml` config, returning the (possibly empty) list of
/// soft WARNINGS for a structurally-valid file, or a hard `Err` (with the toml error's
/// line/column) for a TOML syntax error. Pure — the file I/O lives in [`validate_config`].
///
/// A clean TOML parse is not the whole story: the loader warn-skips bad chords /
/// escapes / actions in `[key_sequences]` and `[keybindings]` at runtime, so an entry
/// that parses as a string can still silently never work. Those are returned as
/// warnings so the diagnostic doesn't print a false green.
pub(crate) fn validate_config_text(text: &str) -> Result<Vec<String>, String> {
    let config = toml::from_str::<crate::app_config::Config>(text).map_err(|e| e.to_string())?;
    let trail_packs = config.resolve_trail_pack_catalog();
    let mut warnings = Vec::new();
    if let Some(table) = config.key_sequences.as_ref() {
        for (chord, bytes) in table {
            if let Err(e) = crate::keybinding::Chord::parse(chord) {
                warnings.push(format!("key_sequences: chord {chord:?} invalid ({e})"));
            } else if let Err(e) = crate::keybinding::parse_byte_sequence(bytes) {
                warnings.push(format!("key_sequences[{chord:?}]: value invalid ({e})"));
            } else if bytes.is_empty() {
                // An empty value parses Ok([]) but the loader warn-skips it (it would
                // silently dead-key the chord), so flag it rather than false-greening.
                warnings.push(format!(
                    "key_sequences[{chord:?}]: empty value would silently disable the key"
                ));
            } else {
                if let Some(label) = crate::keybinding::builtin_shadow_label(chord) {
                    warnings.push(format!(
                        "key_sequences: chord {chord:?} conflicts with built-in {label}"
                    ));
                }
                if let Some(kb) = config.keybindings.as_ref()
                    && crate::keybinding::chord_in_keybindings(chord, kb)
                {
                    warnings.push(format!(
                        "key_sequences: chord {chord:?} is also bound in [keybindings] \
                         (the keybinding wins; this sequence never fires)"
                    ));
                }
            }
        }
    }
    if let Some(table) = config.keybindings.as_ref() {
        for (chord, action) in table {
            if let Err(e) = crate::keybinding::Chord::parse(chord) {
                warnings.push(format!("keybindings: chord {chord:?} invalid ({e})"));
            } else if crate::keybinding::Action::parse(action).is_none() {
                warnings.push(format!("keybindings[{chord:?}]: unknown action {action:?}"));
            } else if let Some(label) = crate::keybinding::builtin_shadow_label(chord) {
                warnings.push(format!(
                    "keybindings: chord {chord:?} conflicts with built-in {label}"
                ));
            }
        }
    }
    // W5h: keys the loaders warn-skip (falling back to a default) also flagged
    // here, so `--validate-config` can't false-green a config whose font/theme/
    // cursor/colours silently do nothing at load.
    //
    // Font resolvability: the loader falls back to $ATERM_FONT then the built-in
    // candidates with zero output — the classic "my font_family did nothing".
    if let Some(fam) = config.font_family.as_deref()
        && let Some(w) = crate::app_config::Config::font_family_warning(Some(fam))
    {
        warnings.push(w);
    }
    // Theme names: both sides of a `dark:…,light:…` split resolve (or warn).
    for appearance in [
        aterm_types::Appearance::Dark,
        aterm_types::Appearance::Light,
    ] {
        if let Some(name) = config.resolve_theme_name(appearance)
            && !name.eq_ignore_ascii_case("default")
            && let Err(e) = aterm_types::scheme::load(&name)
        {
            let w = format!("theme: {name:?} does not resolve ({e}); Default used at load");
            if !warnings.contains(&w) {
                warnings.push(w);
            }
        }
    }
    // Cursor-style spellings (the loader falls back to a blinking block).
    if let Some(style) = config.cursor_style.as_deref()
        && !matches!(
            style.trim().to_ascii_lowercase().as_str(),
            "block" | "underline" | "bar" | "beam"
        )
    {
        warnings.push(format!(
            "cursor_style: expected block|underline|bar, got {style:?} (block used at load)"
        ));
    }
    // Cursor-trail-style spellings: an unknown value fails the `glow_config`
    // enablement gate, so the whole cursor effect silently vanishes at load —
    // the classic "my typo'd style did nothing". Validated against the SAME
    // canonical + alias set the Settings picker resolves through.
    warnings.extend(trail_packs.diagnostics.iter().cloned());
    if let Some(w) = config.cursor_trail_style_warning(&trail_packs) {
        warnings.push(w);
    }
    // Hex colours: every colour-typed key the loaders parse-or-skip.
    for (key, value) in [
        ("foreground", config.foreground.as_deref()),
        ("background", config.background.as_deref()),
        ("cursor_color", config.cursor_color.as_deref()),
        ("selection_color", config.selection_color.as_deref()),
        (
            "selection_foreground",
            config.selection_foreground.as_deref(),
        ),
        ("cursor_trail_color", config.cursor_trail_color.as_deref()),
        ("cursor_trail_accent", config.cursor_trail_accent.as_deref()),
    ] {
        if let Some(s) = value
            && crate::app_config::parse_hex_color(s).is_none()
        {
            warnings.push(format!(
                "{key}: expected #RRGGBB, got {s:?} (ignored at load)"
            ));
        }
    }
    if let Some(palette) = config.palette.as_ref() {
        for (i, hex) in palette.iter().enumerate() {
            if crate::app_config::parse_hex_color(hex).is_none() {
                warnings.push(format!(
                    "palette[{i}]: expected #RRGGBB, got {hex:?} (ignored at load)"
                ));
            }
        }
    }
    warnings.extend(config.sparkle_toy_pack_warnings());
    Ok(warnings)
}

/// `--validate-config`: parse the config at its canonical path. Returns a message
/// and whether it is valid (the CLI maps the bool to the exit code).
pub(crate) fn validate_config() -> (String, bool) {
    match crate::app_config::config_path() {
        None => (
            "no config path (HOME / XDG_CONFIG_HOME unset); built-in defaults in use".to_string(),
            true,
        ),
        Some(p) if !p.exists() => (
            format!(
                "no config file at {} — built-in defaults in use (OK)",
                p.display()
            ),
            true,
        ),
        Some(p) => match std::fs::read_to_string(&p) {
            Err(e) => (format!("config {} is unreadable: {e}", p.display()), false),
            Ok(text) => match validate_config_text(&text) {
                Ok(w) if w.is_empty() => (format!("config {} is valid", p.display()), true),
                Ok(w) => (
                    format!(
                        "config {} is valid, but {} entr{} will be skipped at load:\n  {}",
                        p.display(),
                        w.len(),
                        if w.len() == 1 { "y" } else { "ies" },
                        w.join("\n  ")
                    ),
                    true,
                ),
                Err(e) => (format!("config {} is INVALID:\n{e}", p.display()), false),
            },
        },
    }
}

/// `--list-fonts`: the font search directories the resolver scans, then every
/// discoverable family STEM (from `aterm_render::list_fonts`). The directory
/// header makes the result self-explanatory (where these came from); the family
/// list is sorted + de-duplicated by the renderer. A host with no enumerable
/// fonts still prints the dirs header followed by a clear placeholder.
pub(crate) fn list_fonts() -> String {
    let mut s = String::new();
    let _ = writeln!(s, "font search directories:");
    for dir in aterm_render::font_search_dirs() {
        let _ = writeln!(s, "  {}", dir.display());
    }
    let _ = writeln!(s);
    let families = aterm_render::list_fonts();
    if families.is_empty() {
        let _ = writeln!(s, "fonts: (none discoverable)");
    } else {
        let _ = writeln!(s, "fonts ({}):", families.len());
        for f in families {
            let _ = writeln!(s, "  {f}");
        }
    }
    s
}

/// `--list-themes`: every built-in colour scheme as `name — description`, the
/// `"Default"` first, from the single registry (`scheme::builtin_themes`). These
/// are the names accepted by `theme = "<name>"` in the config.
pub(crate) fn list_themes() -> String {
    let mut s = String::new();
    let _ = writeln!(s, "built-in themes (set via `theme = \"<name>\"`):");
    for (name, desc) in aterm_types::scheme::builtin_themes() {
        let _ = writeln!(s, "  {name} — {desc}");
    }
    s
}

/// `--list-keybinds`: the keybinding surface. First the BUILT-IN default chords
/// (the fixed Cmd-* bindings handled in `on_key`), then any user `[keybindings]`
/// overrides from the effective config (parsed, malformed entries skipped). The
/// bindable action NAMES come from [`crate::keybinding::ACTION_NAMES`].
pub(crate) fn list_keybinds() -> String {
    let mut s = String::new();
    let _ = writeln!(s, "built-in keybindings (in the window):");
    for (chord, label) in crate::keybinding::BUILTIN_CMD_CHORDS {
        let _ = writeln!(s, "  {chord:<16} {label}");
    }
    let _ = writeln!(s);
    let config = crate::app_config::load_config();
    match config.keybindings.as_ref().filter(|t| !t.is_empty()) {
        None => {
            let _ = writeln!(s, "user [keybindings]: (none configured)");
        }
        Some(table) => {
            let _ = writeln!(s, "user [keybindings] (from config):");
            for (chord, action) in table {
                let note = if crate::keybinding::Action::parse(action).is_none() {
                    "  (UNKNOWN action)".to_string()
                } else if let Some(lbl) = crate::keybinding::builtin_shadow_label(chord) {
                    format!("  (conflicts with {lbl})")
                } else {
                    String::new()
                };
                let _ = writeln!(s, "  {chord:<16} {action}{note}");
            }
        }
    }
    let _ = writeln!(s);
    match config.key_sequences.as_ref().filter(|t| !t.is_empty()) {
        None => {
            let _ = writeln!(s, "user [key_sequences]: (none configured)");
        }
        Some(table) => {
            let _ = writeln!(
                s,
                "user [key_sequences] (chord -> raw bytes sent to the PTY):"
            );
            for (chord, bytes) in table {
                let note = if crate::keybinding::Chord::parse(chord).is_err() {
                    "  (INVALID chord)".to_string()
                } else if crate::keybinding::parse_byte_sequence(bytes).is_err() {
                    "  (INVALID bytes)".to_string()
                } else if config
                    .keybindings
                    .as_ref()
                    .is_some_and(|kb| crate::keybinding::chord_in_keybindings(chord, kb))
                {
                    "  (shadowed by [keybindings])".to_string()
                } else if let Some(lbl) = crate::keybinding::builtin_shadow_label(chord) {
                    format!("  (conflicts with {lbl})")
                } else {
                    String::new()
                };
                let _ = writeln!(s, "  {chord:<16} {bytes}{note}");
            }
        }
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "bindable action names (for [keybindings] values):");
    for name in crate::keybinding::ACTION_NAMES {
        let _ = writeln!(s, "  {name}");
    }
    s
}

/// `--show-config`: the EFFECTIVE resolved config — the values aterm would launch
/// with right now, after applying the env > config > default precedence. Reuses
/// the same resolvers the startup path uses (`resolve_font_px`,
/// `resolve_tab_strip_rows`, `Config::theme`/`applied_terminal_config`) so what is
/// printed is what would be applied. The config FILE path + presence is shown so
/// the reader knows whether any of this came from disk.
pub(crate) fn show_config() -> String {
    let config = crate::app_config::load_config();
    let (config_path, config_exists) = match crate::app_config::config_path() {
        Some(p) => (p.display().to_string(), p.exists()),
        None => ("(no HOME / XDG_CONFIG_HOME)".to_string(), false),
    };
    let gpu = crate::app_config::resolve_want_gpu(&config);
    let font_px = crate::app_config::resolve_font_px(&config);
    let tab_strip_rows = crate::app_config::resolve_tab_strip_rows(&config);
    let theme_name = config
        .theme
        .clone()
        .unwrap_or_else(|| "Default".to_string());
    let tc = config.applied_terminal_config();
    // The same effective resolution the renderer uses (env > config > platform
    // default), so `--show-config` reports the face that actually loads — on a
    // pristine config that is "(built-in candidates)", the library's
    // FONT_CANDIDATES lead (SF Mono on macOS); `--show-face` names the file.
    let font_family = crate::effective_font_family(config.font_family.as_deref())
        .unwrap_or_else(|| "(built-in candidates)".to_string());
    let columns = crate::app_config::env_u16("ATERM_COLUMNS")
        .or(config.columns)
        .unwrap_or(80);
    let lines = crate::app_config::env_u16("ATERM_LINES")
        .or(config.lines)
        .unwrap_or(24);

    let mut s = String::new();
    let _ = writeln!(s, "effective config (env > config > default)");
    let _ = writeln!(s, "=========================================");
    let _ = writeln!(
        s,
        "config file: {} [{}]",
        config_path,
        if config_exists {
            "present"
        } else {
            "absent — defaults"
        }
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "font_px:        {font_px}");
    let _ = writeln!(s, "font_family:    {font_family}");
    let _ = writeln!(s, "renderer:       {}", renderer_label(gpu));
    let _ = writeln!(s, "columns:        {columns}");
    let _ = writeln!(s, "lines:          {lines}");
    let _ = writeln!(s, "tab_strip_rows: {tab_strip_rows}");
    let _ = writeln!(s, "theme:          {theme_name}");
    // W2 typography knobs (all resolved with the same env > config precedence).
    let _ = writeln!(
        s,
        "text_blending:  {}",
        match config.text_blending_or_default() {
            aterm_render::TextBlending::Linear => "linear",
            aterm_render::TextBlending::LinearCorrected => "linear-corrected",
        }
    );
    let _ = writeln!(s, "font_thicken:   {}", config.font_thicken_or_default());
    let _ = writeln!(s, "stem_gamma:     {}", config.stem_gamma_or_default());
    // W9 variable-font instantiation: the parsed requests + dark nudge.
    let (vf_reqs, _) = config.font_variation_requests();
    let vf: Vec<String> = vf_reqs
        .iter()
        .map(|&(tag, v)| {
            let t = tag.to_be_bytes();
            format!("{}={v}", String::from_utf8_lossy(&t).trim_end())
        })
        .collect();
    let _ = writeln!(s, "font_variation: [{}]", vf.join(", "));
    let _ = writeln!(
        s,
        "font_weight_dark_nudge: {}",
        config.font_weight_dark_nudge_or_default()
    );
    // M2 "ink that dries" — default-on fade-in of streamed output.
    let _ = writeln!(
        s,
        "stream_fade:    {} ({} ms)",
        config.stream_fade_or_default(),
        config.stream_fade_ms_or_default()
    );
    let _ = writeln!(
        s,
        "foreground:     #{:02X}{:02X}{:02X}",
        tc.default_foreground.r, tc.default_foreground.g, tc.default_foreground.b
    );
    let _ = writeln!(
        s,
        "background:     #{:02X}{:02X}{:02X}",
        tc.default_background.r, tc.default_background.g, tc.default_background.b
    );
    s
}

/// `--show-face`: the resolved font FACE for `family` — the file aterm would
/// actually load plus its cell metrics + glyph count (from `aterm_render::face_info`,
/// the same resolver the renderer uses). `family` empty falls back to the effective
/// `font_family` (env > config > platform default). An unresolvable family yields a
/// clear message and a non-zero result so scripts can detect it.
pub(crate) fn show_face(family: &str) -> (String, bool) {
    let family = family.trim();
    let resolved_family = if family.is_empty() {
        let config = crate::app_config::load_config();
        crate::effective_font_family(config.font_family.as_deref()).unwrap_or_default()
    } else {
        family.to_string()
    };
    if resolved_family.trim().is_empty() {
        return (
            "no font family configured; pass one: --show-face <family>".to_string(),
            false,
        );
    }
    match aterm_render::face_info(&resolved_family) {
        None => (
            format!("font family {resolved_family:?} does not resolve to a loadable face"),
            false,
        ),
        Some(info) => {
            let mut s = String::new();
            let _ = writeln!(s, "resolved face for {resolved_family:?}:");
            let _ = writeln!(s, "  path:        {}", info.path);
            let _ = writeln!(
                s,
                "  metrics at:  {} px (probe size)",
                aterm_render::FaceInfo::PROBE_PX
            );
            let _ = writeln!(s, "  cell_width:  {} px", info.cell_width);
            let _ = writeln!(s, "  cell_height: {} px", info.cell_height);
            let _ = writeln!(s, "  baseline:    {} px", info.baseline);
            let _ = writeln!(s, "  glyph_count: {}", info.glyph_count);
            (s, true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_good_config_and_rejects_bad() {
        // A well-formed config of known keys parses.
        assert!(validate_config_text("font_px = 14.0\ngpu = true\ntheme = \"Dracula\"").is_ok());
        // Empty config is valid (all fields optional).
        assert!(validate_config_text("").is_ok());
        // A type error (string where a number is expected) is reported.
        let err = validate_config_text("font_px = \"big\"").unwrap_err();
        assert!(
            !err.is_empty(),
            "a type mismatch must yield an error message"
        );
        // Malformed TOML syntax is reported.
        assert!(validate_config_text("font_px = = 1").is_err());
    }

    #[test]
    fn validate_surfaces_toy_packs_that_runtime_would_skip() {
        let mut paths = vec!["/aterm-test/definitely-missing-pack.toml".to_string()];
        paths.extend((1..=8).map(|i| format!("/aterm-test/missing-pack-{i}.toml")));
        let quoted = paths
            .iter()
            .map(|path| format!("{path:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        let warnings = validate_config_text(&format!("[sparkle_words]\ntoy_packs = [{quoted}]\n"))
            .expect("structurally valid config");
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("only the first 8 load"))
        );
        assert!(
            warnings.iter().any(|warning| {
                warning.contains("toy_packs[0]") && warning.contains("unreadable")
            })
        );
    }

    /// A structurally-valid config whose [key_sequences]/[keybindings] entries the
    /// loader will warn+skip (bad chord, unknown action) parses Ok but yields a
    /// warning per offender — so --validate-config can't print a false green.
    #[test]
    fn validate_flags_bad_keybind_and_key_sequence_entries() {
        let cfg = r#"
[key_sequences]
"shift+entr" = "hi"
[keybindings]
"cmd+k" = "no_such_action"
"#;
        let warnings = validate_config_text(cfg).expect("structurally valid TOML");
        let joined = warnings.join("\n");
        assert!(joined.contains("shift+entr"), "bad chord flagged: {joined}");
        assert!(
            joined.contains("no_such_action"),
            "unknown action flagged: {joined}"
        );
        assert_eq!(warnings.len(), 2, "exactly the two offenders: {joined}");
    }

    /// Detection-only shadow / collision warnings (use `contains`, not exact counts):
    /// a [keybindings] entry on a built-in Cmd chord warns "conflicts with built-in Copy"; a
    /// [key_sequences] chord that is BOTH a built-in AND bound in [keybindings] warns
    /// both ways. No code gate — the override capability is preserved, just surfaced.
    #[test]
    fn validate_flags_shadow_and_collision() {
        let cfg = r#"
[key_sequences]
"cmd+t" = "\n"
[keybindings]
"cmd+c" = "find"
"cmd+t" = "new_tab"
"#;
        let joined = validate_config_text(cfg)
            .expect("structurally valid TOML")
            .join("\n");
        assert!(joined.contains("conflicts with built-in Copy"), "{joined}");
        assert!(
            joined.contains("conflicts with built-in New Tab"),
            "{joined}"
        );
        assert!(joined.contains("also bound in [keybindings]"), "{joined}");
    }

    /// An empty `[key_sequences]` value is valid TOML but the loader warn-skips it (it
    /// would dead-key the chord); --validate-config must SURFACE that, not false-green.
    #[test]
    fn validate_flags_empty_key_sequence() {
        let joined = validate_config_text("[key_sequences]\n\"ctrl+g\" = \"\"\n")
            .expect("structurally valid TOML")
            .join("\n");
        assert!(
            joined.contains("empty value would silently disable"),
            "empty key_sequences value flagged: {joined}"
        );
    }

    /// W5h: keys the loaders silently fall back on — an unresolvable
    /// `font_family`, an unknown `theme` name, a typo'd `cursor_style`, and
    /// malformed hex colours (top-level + palette entries) — are all flagged
    /// so `--validate-config` can't false-green a config that does nothing.
    #[test]
    fn validate_flags_font_theme_cursor_and_color_typos() {
        let joined = validate_config_text(
            "font_family = \"definitely-not-a-real-font-xyzzy\"\n\
             theme = \"NoSuchTheme\"\n\
             cursor_style = \"blok\"\n\
             foreground = \"#zzz\"\n\
             selection_foreground = \"12345\"\n\
             palette = [\"#102030\", \"nope\"]\n",
        )
        .expect("structurally valid TOML")
        .join("\n");
        assert!(
            joined.contains("does not resolve to a font file"),
            "{joined}"
        );
        assert!(joined.contains("NoSuchTheme"), "{joined}");
        assert!(joined.contains("cursor_style"), "{joined}");
        assert!(joined.contains("foreground: expected #RRGGBB"), "{joined}");
        assert!(
            joined.contains("selection_foreground: expected #RRGGBB"),
            "{joined}"
        );
        assert!(joined.contains("palette[1]"), "{joined}");
        assert!(
            !joined.contains("palette[0]"),
            "valid entry not flagged: {joined}"
        );

        // And a config using all these keys CORRECTLY stays warning-free (no
        // false positives): resolvable values only, so it validates green.
        let clean = validate_config_text(
            "theme = \"Default\"\ncursor_style = \"bar\"\nforeground = \"#aabbcc\"\n\
             minimum_contrast = 4.5\nline_height = 1.2\n",
        )
        .expect("valid");
        assert!(clean.is_empty(), "clean config must not warn: {clean:?}");
    }

    /// An unknown `cursor_trail_style` silently disables the WHOLE cursor
    /// effect at load (the `glow_config` gate), so `--validate-config` must
    /// flag it — while every canonical spelling AND every documented alias
    /// (rainbow/ember/ocean/…) validates green, since those genuinely render.
    #[test]
    fn validate_flags_unknown_cursor_trail_style() {
        let joined = validate_config_text("cursor_trail_style = \"phasr\"\n")
            .expect("structurally valid TOML")
            .join("\n");
        assert!(
            joined.contains("cursor_trail_style") && joined.contains("phasr"),
            "typo'd style must be flagged: {joined}"
        );
        for ok in ["phaser", "off", "Rainbow", "embers", "ocean", "light-beam"] {
            let clean =
                validate_config_text(&format!("cursor_trail_style = \"{ok}\"\n")).expect("valid");
            assert!(clean.is_empty(), "{ok:?} must validate green: {clean:?}");
        }
    }

    fn sample() -> DiagInfo {
        DiagInfo {
            version: "0.3.0".into(),
            git_commit: "abc1234",
            build_time: "2026-06-23T00:00:00Z",
            target_os: "macos",
            target_arch: "aarch64",
            profile: "release",
            compiler:
                "rustc 1.96.0 (ac68faa2) \u{00b7} rust \u{00b7} release \u{00b7} trust_verify off"
                    .into(),
            update_pin_sha256: "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925"
                .into(),
            renderer_default: "cpu",
            features: vec![("sixel", true), ("accessibility", false)],
            capabilities: vec![("kitty_graphics", true), ("soft_fonts", false)],
            config_path: "/home/u/.config/aterm/aterm.toml".into(),
            config_exists: false,
            env: vec![("ATERM_GPU".into(), "1".into())],
        }
    }

    #[test]
    fn report_includes_key_sections() {
        let r = sample().render();
        assert!(r.contains("aterm diagnostics"), "header");
        assert!(r.contains("version:   0.3.0 (abc1234"), "version line");
        assert!(r.contains("[x] kitty_graphics"), "advertised cap checked");
        assert!(r.contains("[ ] soft_fonts"), "unadvertised cap unchecked");
        assert!(r.contains("ATERM_GPU=1"), "env listed");
        assert!(
            r.contains(
                "update-pin-sha256: \
                 66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925"
            ),
            "compiled updater trust pin"
        );
        assert!(r.contains("absent — defaults"), "config absence noted");
    }

    #[test]
    fn empty_env_renders_placeholder() {
        let mut d = sample();
        d.env.clear();
        assert!(d.render().contains("(no ATERM_* variables set)"));
    }

    #[test]
    fn collect_enumerates_every_capability() {
        let d = collect();
        // All 16 advertised capabilities are surfaced (no silent omission).
        assert_eq!(d.capabilities.len(), 16);
        assert!(d.render().contains("terminal capabilities"));
        // Version is the real build's DISPLAY version (with the +r./+t. compiler
        // suffix), and the compiler provenance line rides along.
        assert_eq!(d.version, crate::build_info::version_display());
        assert_eq!(d.compiler, crate::build_info::compiler_summary());
    }

    #[test]
    fn list_themes_lists_every_builtin_default_first() {
        let out = list_themes();
        assert!(out.contains("built-in themes"), "header present");
        // Default is first and every registry name appears with a description.
        assert!(out.contains("Default — "), "Default themed first");
        for name in aterm_types::scheme::builtin_names() {
            assert!(out.contains(name), "{name} must be listed");
        }
    }

    #[test]
    fn list_fonts_has_dirs_header_and_lists_search_dirs() {
        let out = list_fonts();
        assert!(out.contains("font search directories:"), "dirs header");
        // Every scanned directory is named so the family list is self-explanatory.
        for dir in aterm_render::font_search_dirs() {
            assert!(
                out.contains(&dir.display().to_string()),
                "search dir {dir:?} listed"
            );
        }
        // Either a fonts section or the explicit empty placeholder — never silent.
        assert!(out.contains("fonts"), "a fonts line is always present");
    }

    #[test]
    fn list_keybinds_covers_builtins_and_action_names() {
        let out = list_keybinds();
        assert!(out.contains("built-in keybindings"), "builtin header");
        // A couple of the fixed Cmd-* chords are documented.
        assert!(out.contains("cmd+c"), "copy chord listed");
        assert!(out.contains("cmd+t"), "new-tab chord listed");
        // Every bindable action NAME is offered for [keybindings] values.
        assert!(out.contains("bindable action names"), "actions header");
        assert!(
            out.contains("[key_sequences]"),
            "key_sequences section present"
        );
        for name in crate::keybinding::ACTION_NAMES {
            assert!(out.contains(name), "{name} must be listed");
        }
    }

    #[test]
    fn show_config_reports_effective_resolved_values() {
        let out = show_config();
        assert!(out.contains("effective config"), "header present");
        // The key resolved knobs are each surfaced with a label.
        for label in [
            "config file:",
            "font_px:",
            "renderer:",
            "columns:",
            "lines:",
            "theme:",
            "foreground:",
            "background:",
            "text_blending:",
            "font_thicken:",
            "stem_gamma:",
            "font_variation:",
            "font_weight_dark_nudge:",
            "stream_fade:",
        ] {
            assert!(out.contains(label), "{label} must appear in show-config");
        }
    }

    /// The renderer label the reports print is derived from the SHARED backend
    /// resolver (`resolve_want_gpu`), not from `$ATERM_GPU` alone — so a default
    /// launch (GPU-on default) and a config `gpu = true` both report a GPU backend,
    /// and `$ATERM_CPU` / `gpu = false` report cpu. This pins the precedence the
    /// diagnostics reader relies on to match what `main` actually selects.
    #[test]
    fn renderer_label_tracks_resolved_gpu_precedence() {
        use crate::app_config::resolve_want_gpu_with;
        // $ATERM_CPU forces CPU regardless of $ATERM_GPU or config.
        assert_eq!(
            renderer_label(resolve_want_gpu_with(true, true, Some(true))),
            "cpu"
        );
        // $ATERM_GPU forces GPU even when config sets gpu = false.
        assert_eq!(
            renderer_label(resolve_want_gpu_with(false, true, Some(false))),
            GPU_BACKEND_LABEL
        );
        // With neither env set, config decides.
        assert_eq!(
            renderer_label(resolve_want_gpu_with(false, false, Some(false))),
            "cpu"
        );
        assert_eq!(
            renderer_label(resolve_want_gpu_with(false, false, Some(true))),
            GPU_BACKEND_LABEL
        );
        // The regression: a DEFAULT launch (no env, no config) renders on GPU, so
        // the report must say GPU — not "cpu" as the old env-only check did.
        assert_eq!(
            renderer_label(resolve_want_gpu_with(false, false, None)),
            GPU_BACKEND_LABEL
        );
    }

    #[test]
    fn show_face_rejects_an_unresolvable_family() {
        // A deliberately nonsensical family never resolves; the result is non-zero
        // with a clear message (scripts can detect the failure).
        let (msg, ok) = show_face("definitely-not-a-real-font-xyzzy");
        assert!(!ok, "unresolvable family must report failure");
        assert!(!msg.is_empty(), "a message is always produced");
    }
}
