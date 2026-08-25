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
    /// What shell-integration preparation ACTUALLY did for the shell this
    /// configuration spawns — distinct from the `shell_integration` row in the
    /// capabilities list, which is the compile-time ADVERTISED capability. An
    /// unknown shell or an unwritable loader cache reads "NOT ACTIVE" here
    /// while the advertised row stays checked, and that split is the fact a
    /// "command not found on the ALab tools" report needs surfaced.
    pub shell_integration_runtime: String,
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
        let _ = writeln!(s, "shell-int: {}", self.shell_integration_runtime);
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

/// The RUNTIME shell-integration outcome for the shell this configuration
/// would actually spawn (env > config `shell`, the same funnel as the spawn
/// path) — probed by running the REAL preparation. That write is idempotent,
/// memoized, and exactly what every launch performs; it is also the only
/// honest probe, because "is the loader cache writable" is proven by writing.
fn shell_integration_runtime(config: &crate::app_config::Config) -> String {
    use aterm_core::shell_integration as si;
    let hint = crate::app_config::resolve_shell_override(config);
    let shell = match hint.as_deref().filter(|s| !s.is_empty()) {
        Some(h) => si::ShellType::detect(h),
        None => si::ShellType::detect_current(),
    };
    match si::prepare(shell) {
        Ok(Some(_)) => format!("active ({shell:?} loader prepared)"),
        Ok(None) => {
            let name = hint
                .filter(|s| !s.is_empty())
                .or_else(|| std::env::var("SHELL").ok())
                .unwrap_or_else(|| "the default shell".to_string());
            format!(
                "NOT ACTIVE — no integration for {name}; ALab tools reach new shells \
                 only via shell.d, so add <prefix>/bin to PATH manually"
            )
        }
        Err(e) => format!("NOT ACTIVE — loader cache unwritable: {e}"),
    }
}

/// Collect diagnostics from the live build + environment.
pub(crate) fn collect() -> DiagInfo {
    let config = crate::app_config::load_config();
    // Renderer default resolved through the shared funnel (env > config `gpu` >
    // GPU-on default) so the report matches what `main` would actually launch. wgpu
    // negotiates Metal on macOS, Vulkan elsewhere; the live `metrics` verb reports
    // the actually-negotiated backend.
    let gpu = crate::app_config::resolve_want_gpu(&config);
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
        shell_integration_runtime: shell_integration_runtime(&config),
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

/// One deterministic, source-addressable warning shared by `--validate-config`
/// and the native Manual language service. `key` names the owning TOML item so
/// the editor can underline the same value whose runtime consumer would skip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigSemanticWarning {
    pub(crate) key: &'static str,
    pub(crate) message: String,
}

/// The at-most-one semantic warning a single `[keybindings]` row earns, with
/// the built-in shadow ALREADY RESOLVED by the caller
/// ([`crate::keybinding::builtin_shadow_label`]).
///
/// The label is a PARAMETER rather than a lookup because the hardcoded
/// Cmd/Super suite is compiled off on Linux, so a host-conditional test can
/// only ever assert the Linux half — and the review's escape (`"cmd+c" =
/// "none"` validating fully green) lived exactly in the half Linux cannot see.
/// With the gate explicit, the macOS-shaped path is reachable from any host.
///
/// Order is load-bearing, and the unbind arm is deliberately NOT terminal:
/// `"none"`/`"unbind"` is the documented spelling for masking a platform SEED,
/// so it must never be flagged as an unknown action (the old behaviour told a
/// user their working config was broken) — but an unbind cannot mask a
/// HARDCODED built-in, so where one claims the chord the conflict caveat is
/// exactly as true as for any other value and still rides.
fn keybinding_row_warning(
    chord: &str,
    action: &str,
    shadow: Option<&'static str>,
) -> Option<ConfigSemanticWarning> {
    if let Err(e) = crate::keybinding::Chord::parse(chord) {
        return Some(ConfigSemanticWarning {
            key: "keybindings",
            message: format!("keybindings: chord {chord:?} invalid ({e})"),
        });
    }
    if crate::keybinding::is_unbind_action(action) {
        return shadow.map(|label| ConfigSemanticWarning {
            key: "keybindings",
            message: format!(
                "keybindings: chord {chord:?} unbinds a default but still conflicts with built-in {label}"
            ),
        });
    }
    if crate::keybinding::Action::parse(action).is_none() {
        return Some(ConfigSemanticWarning {
            key: "keybindings",
            message: format!("keybindings[{chord:?}]: unknown action {action:?}"),
        });
    }
    shadow.map(|label| ConfigSemanticWarning {
        key: "keybindings",
        message: format!("keybindings: chord {chord:?} conflicts with built-in {label}"),
    })
}


/// Semantic checks that require no filesystem, platform service, or ambient
/// capability. Keeping this subset separate lets the config editor stay pure
/// and responsive while sharing the exact keybinding/sequence and palette
/// acceptance rules with `--validate-config`.
pub(crate) fn config_semantic_warnings(
    config: &crate::app_config::Config,
) -> Vec<ConfigSemanticWarning> {
    let mut warnings = Vec::new();

    if let Some(font_px) = config.font_px
        && (!font_px.is_finite() || !(crate::FONT_PX_MIN..=crate::FONT_PX_MAX).contains(&font_px))
    {
        warnings.push(ConfigSemanticWarning {
            key: "font_px",
            message: format!(
                "font_px is {font_px:?}, outside the supported {}–{} range; the resolver ignores it rather than clamping it and uses the next valid precedence fallback",
                crate::FONT_PX_MIN,
                crate::FONT_PX_MAX,
            ),
        });
    }

    if let Some(variations) = config.font_variation.as_ref() {
        for (index, spec) in variations.0.iter().enumerate() {
            if aterm_render::variation::parse_variation_spec(spec).is_none() {
                warnings.push(ConfigSemanticWarning {
                    key: "font_variation",
                    message: format!(
                        "font_variation[{index}]: malformed entry {spec:?} is ignored; use \"tag=value\", for example \"wght=450\""
                    ),
                });
            }
        }
    }

    if let Some(feature_entries) = config.font_features.as_ref() {
        use aterm_types::text_shaping::FontFeature;
        for (index, entry) in feature_entries.iter().enumerate() {
            let tokens = entry.split_whitespace().collect::<Vec<_>>();
            let rejected = tokens
                .iter()
                .copied()
                .filter(|token| FontFeature::parse_token(token).is_none())
                .collect::<Vec<_>>();
            if tokens.is_empty() {
                warnings.push(ConfigSemanticWarning {
                    key: "font_features",
                    message: format!(
                        "font_features[{index}]: blank entry contains no OpenType feature token and has no effect"
                    ),
                });
            } else if !rejected.is_empty() {
                warnings.push(ConfigSemanticWarning {
                    key: "font_features",
                    message: format!(
                        "font_features[{index}]: malformed token(s) {rejected:?} are ignored; use tag, +tag, -tag, or tag=unsigned-value with a 1–4 byte ASCII tag"
                    ),
                });
            }
            let accepted = tokens
                .iter()
                .filter(|token| FontFeature::parse_token(token).is_some())
                .count();
            if accepted > 256 {
                warnings.push(ConfigSemanticWarning {
                    key: "font_features",
                    message: format!(
                        "font_features[{index}]: only the first 256 valid tokens are applied; {} later token(s) are ignored",
                        accepted - 256
                    ),
                });
            }
        }
    }

    if let Some(update) = config.update.as_ref() {
        for (key, value) in [
            ("update.owner", update.owner.as_deref()),
            ("update.repo", update.repo.as_deref()),
        ] {
            if let Some(value) = value
                && !aterm_update_core::is_valid_slug(value.trim())
            {
                warnings.push(ConfigSemanticWarning {
                    key,
                    message: format!(
                        "{key} value {value:?} is not a safe GitHub slug and is ignored; a valid environment value or the compiled default is used"
                    ),
                });
            }
        }
    }

    if let Some(packages) = config.packages.as_ref() {
        if let Some(account) = packages.account.as_deref()
            && !aterm_update_core::is_valid_slug(account.trim())
        {
            warnings.push(ConfigSemanticWarning {
                key: "packages.account",
                message: format!(
                    "packages.account value {account:?} is not a safe GitHub owner slug and is ignored; atpkg uses its next valid precedence fallback"
                ),
            });
        }
        if packages
            .channel
            .as_deref()
            .is_some_and(|channel| channel.trim().is_empty())
        {
            warnings.push(ConfigSemanticWarning {
                key: "packages.channel",
                message: "packages.channel is blank and is treated as unset; atpkg uses the stable channel"
                    .to_string(),
            });
        }
    }
    if let Some(table) = config.key_sequences.as_ref() {
        for (chord, bytes) in table {
            if let Err(e) = crate::keybinding::Chord::parse(chord) {
                warnings.push(ConfigSemanticWarning {
                    key: "key_sequences",
                    message: format!("key_sequences: chord {chord:?} invalid ({e})"),
                });
            } else if let Err(e) = crate::keybinding::parse_byte_sequence(bytes) {
                warnings.push(ConfigSemanticWarning {
                    key: "key_sequences",
                    message: format!("key_sequences[{chord:?}]: value invalid ({e})"),
                });
            } else if bytes.is_empty() {
                // An empty value parses Ok([]) but the loader warn-skips it (it would
                // silently dead-key the chord), so flag it rather than false-greening.
                warnings.push(ConfigSemanticWarning {
                    key: "key_sequences",
                    message: format!(
                        "key_sequences[{chord:?}]: empty value would silently disable the key"
                    ),
                });
            } else {
                if let Some(label) = crate::keybinding::builtin_shadow_label(chord) {
                    warnings.push(ConfigSemanticWarning {
                        key: "key_sequences",
                        message: format!(
                            "key_sequences: chord {chord:?} conflicts with built-in {label}"
                        ),
                    });
                }
                if let Some(kb) = config.keybindings.as_ref()
                    && crate::keybinding::chord_in_keybindings(chord, kb)
                {
                    warnings.push(ConfigSemanticWarning {
                        key: "key_sequences",
                        message: format!(
                            "key_sequences: chord {chord:?} is also bound in [keybindings] \
                             (the keybinding wins; this sequence never fires)"
                        ),
                    });
                }
            }
        }
    }
    if let Some(table) = config.keybindings.as_ref() {
        for (chord, action) in table {
            warnings.extend(keybinding_row_warning(
                chord,
                action,
                crate::keybinding::builtin_shadow_label(chord),
            ));
        }
    }
    if let Some(palette) = config.palette.as_ref() {
        for (i, hex) in palette.iter().enumerate() {
            if crate::app_config::parse_hex_color(hex).is_none() {
                warnings.push(ConfigSemanticWarning {
                    key: "palette",
                    message: format!(
                        "palette[{i}]: expected #RRGGBB, got {hex:?} (ignored at load)"
                    ),
                });
            }
        }
    }
    if let Some(palette) = config
        .sparkle_words
        .as_ref()
        .and_then(|sparkle| sparkle.profanity.as_ref())
        .and_then(|profanity| profanity.palette.as_ref())
    {
        for (index, color) in palette.iter().enumerate() {
            if crate::app_config::parse_hex_color(color).is_none() {
                warnings.push(ConfigSemanticWarning {
                    key: "sparkle_words.profanity.palette",
                    message: format!(
                        "sparkle_words.profanity.palette[{index}]: expected RRGGBB or #RRGGBB, got {color:?}; this color is ignored"
                    ),
                });
            }
        }
    }
    if let Some(hue) = config
        .matrix_rain
        .as_ref()
        .and_then(|rain| rain.hue.as_deref())
    {
        let hue = hue.trim();
        let valid = hue.eq_ignore_ascii_case("matrix")
            || hue.eq_ignore_ascii_case("theme")
            || (hue.starts_with('#') && crate::app_config::parse_hex_color(hue).is_some());
        if !valid {
            warnings.push(ConfigSemanticWarning {
                key: "matrix_rain.hue",
                message: format!(
                    "matrix_rain.hue value {hue:?} is not matrix, theme, or #RRGGBB; stock matrix green is used"
                ),
            });
        }
    }

    if let Some(custom) = config
        .sparkle_words
        .as_ref()
        .and_then(|sparkle| sparkle.custom.as_ref())
    {
        for (index, record) in custom.iter().enumerate() {
            let record_number = index + 1;
            if !record.words.iter().any(|word| !word.trim().is_empty()) {
                warnings.push(ConfigSemanticWarning {
                    key: if record.words.is_empty() {
                        "sparkle_words.custom"
                    } else {
                        "sparkle_words.custom.words"
                    },
                    message: format!(
                        "sparkle_words.custom.words in [[sparkle_words.custom]] record {record_number} has no nonblank word; the record cannot match and is ignored"
                    ),
                });
            }
            let live_burst = record
                .burst
                .as_ref()
                .is_some_and(|burst| burst.chance.unwrap_or(100) != 0);
            if record.ink.is_none() && record.graphic.is_none() && !live_burst {
                let (key, detail) = if record.burst.is_some() {
                    (
                        "sparkle_words.custom.burst.chance",
                        "its only configured burst has chance = 0",
                    )
                } else {
                    (
                        "sparkle_words.custom",
                        "it configures no ink, graphic, or live burst axis",
                    )
                };
                warnings.push(ConfigSemanticWarning {
                    key,
                    message: format!(
                        "sparkle_words.custom in [[sparkle_words.custom]] record {record_number} is inert because {detail}; the record is ignored"
                    ),
                });
            }
        }
    }

    if let Some(net) = config.net.as_ref() {
        let mut names = std::collections::HashSet::new();
        for (index, connection) in net.connections.iter().enumerate() {
            let record = index + 1;
            if !crate::net_connections::valid_connection_name(&connection.name) {
                warnings.push(ConfigSemanticWarning {
                    key: "net.connections.name",
                    message: format!(
                        "net.connections.name in [[net.connections]] record {record} must be a nonempty [A-Za-z0-9_-] name; this connection cannot be dialed"
                    ),
                });
            } else if !names.insert(connection.name.as_str()) {
                warnings.push(ConfigSemanticWarning {
                    key: "net.connections.name",
                    message: format!(
                        "net.connections.name in [[net.connections]] record {record} duplicates {:?}; only the first matching record can be resolved",
                        connection.name
                    ),
                });
            }
            if connection.host.trim().is_empty() {
                warnings.push(ConfigSemanticWarning {
                    key: "net.connections.host",
                    message: format!(
                        "net.connections.host in [[net.connections]] record {record} is blank; dialing this connection always fails before TLS"
                    ),
                });
            } else if !crate::net_connections::valid_dial_endpoint(&connection.host) {
                warnings.push(ConfigSemanticWarning {
                    key: "net.connections.host",
                    message: format!(
                        "net.connections.host in [[net.connections]] record {record} must be host:port, a numeric IP:port, or bracketed IPv6:port with a nonzero port; dialing this value always fails before TLS"
                    ),
                });
            }
            if crate::net_connections::parse_fingerprint(&connection.fingerprint).is_none() {
                warnings.push(ConfigSemanticWarning {
                    key: "net.connections.fingerprint",
                    message: format!(
                        "net.connections.fingerprint in [[net.connections]] record {record} must be 64 hexadecimal SHA-256 digits, optionally prefixed by sha256:; dialing refuses this record"
                    ),
                });
            }
            if connection.sid.is_some() {
                warnings.push(ConfigSemanticWarning {
                    key: "net.connections.sid",
                    message: format!(
                        "net.connections.sid in [[net.connections]] record {record} is stored but currently inert because the shipping rebind check is nonce-only"
                    ),
                });
            }
            if connection.expect_nonce.is_some() {
                warnings.push(ConfigSemanticWarning {
                    key: "net.connections.expect_nonce",
                    message: format!(
                        "net.connections.expect_nonce in [[net.connections]] record {record} currently makes dialing fail closed because the shipping wire protocol cannot verify the remote launch nonce"
                    ),
                });
            }
        }
    }
    if let Some(ink) = config
        .sparkle_words
        .as_ref()
        .and_then(|sparkle| sparkle.ink.as_ref())
        && ink.loop_.unwrap_or(false)
        && let Some(sweep_ms) = ink.sweep_ms
        && sweep_ms < 600
    {
        warnings.push(ConfigSemanticWarning {
            key: "sparkle_words.ink.sweep_ms",
            message: format!(
                "sparkle_words.ink.sweep_ms is configured as {sweep_ms} ms but is effectively 600 ms because loop = true raises the flash-safety minimum"
            ),
        });
    }
    if let Some(rain) = config.matrix_rain.as_ref()
        && let Some(head_alpha) = rain.head_alpha
    {
        use crate::matrix_rain::{RAIN_ALPHA_CAP, RAIN_ALPHA_FLOOR};
        if let Some(alpha) = rain.alpha {
            let resolved_alpha =
                alpha.clamp(u32::from(RAIN_ALPHA_FLOOR), u32::from(RAIN_ALPHA_CAP));
            if head_alpha < resolved_alpha {
                warnings.push(ConfigSemanticWarning {
                    key: "matrix_rain.head_alpha",
                    message: format!(
                        "matrix_rain.head_alpha is configured as {head_alpha} but is effectively {resolved_alpha} because rain heads cannot be dimmer than the resolved matrix_rain.alpha"
                    ),
                });
            }
        } else if head_alpha < u32::from(RAIN_ALPHA_CAP) {
            let floor_note = if head_alpha < u32::from(RAIN_ALPHA_FLOOR) {
                format!(" is first clamped to {RAIN_ALPHA_FLOOR} and")
            } else {
                String::new()
            };
            warnings.push(ConfigSemanticWarning {
                key: "matrix_rain.head_alpha",
                message: format!(
                    "matrix_rain.head_alpha is configured without matrix_rain.alpha; it{floor_note} may be raised further to the theme-derived body alpha because rain heads cannot be dimmer than the body"
                ),
            });
        }
    }
    if let Some(configured_top) = config.window_padding_top
        && configured_top.is_finite()
    {
        let base = config.window_padding_or_default();
        if configured_top > base {
            warnings.push(ConfigSemanticWarning {
                key: "window_padding_top",
                message: format!(
                    "window_padding_top is configured as {configured_top}px but is effectively {base}px because it cannot exceed window_padding"
                ),
            });
        }
    }
    if config.allow_osc52_query == Some(true) {
        // The warning states the WIDEST thing the key grants on THIS platform —
        // informed consent needs the scarier truth, not the reassuring one. On
        // macOS/Windows the Query arm answers with the SYSTEM pasteboard
        // (`pbpaste` — the whole point of the knob is remote vim/tmux clipboard
        // sync, which must see what the user copied in OTHER apps), so the old
        // "own-slot only — never the desktop's clipboard" line was a false
        // promise exactly where a password manager's copy is at stake. Linux is
        // narrower for an implementation reason, not a policy one: a
        // foreign-owner X11 read is a blocking round-trip inside the terminal
        // lock, so only aterm's own selections answer today (spawn.rs, the
        // Query arm) — say that, and no more.
        let message = if cfg!(target_os = "linux") {
            "allow_osc52_query is enabled: a program in the terminal can READ the \
             clipboard selections aterm itself owns (a foreign app's copy is not \
             readable on X11 today). Leave it off unless a specific tool needs it"
        } else {
            "allow_osc52_query is enabled: a program in the terminal can READ the \
             system clipboard — including text copied in OTHER apps, such as a \
             password manager. Leave it off unless a specific tool needs it \
             (e.g. remote vim/tmux clipboard sync)"
        };
        warnings.push(ConfigSemanticWarning {
            key: "allow_osc52_query",
            message: message.to_string(),
        });
    }
    if config.allow_window_ops == Some(true) {
        // Linux wires the manipulation half (frame audit #4: `spawn::
        // configure_window_ops` → `App::on_window_op`); the surviving gap there
        // is the position/pixel-geometry reports. Elsewhere the pre-wiring
        // statement is still the honest one.
        let message = if cfg!(target_os = "linux") {
            "allow_window_ops is wired to the window on Linux: manipulations (iconify, maximize, fullscreen, resize) apply and move stays denied; position/pixel-geometry reports beyond the window-title and text-grid-size fallbacks remain unanswered"
        } else {
            "allow_window_ops enables only the GUI's XTWINOPS window-title and text-grid-size fallback reports; no window callback is installed, so host manipulation and most state/geometry requests are ignored"
        };
        warnings.push(ConfigSemanticWarning {
            key: "allow_window_ops",
            message: message.to_string(),
        });
    }
    warnings
}

/// Runtime validation that deliberately owns ambient filesystem access.
///
/// Manual runs this set off the event-loop and merges the result only when the
/// exact document revision is still current. `--validate-config` calls the same
/// function synchronously. Keeping the checks source-addressable prevents the
/// editor and the loader from disagreeing about a font, theme, Trail Pack, or
/// keyword-toy pack that would otherwise be silently replaced/skipped.
pub(crate) fn config_host_semantic_warnings(
    config: &crate::app_config::Config,
) -> Vec<ConfigSemanticWarning> {
    config_host_semantic_warnings_with_backend(config, crate::app_config::resolve_want_gpu(config))
}

/// Host validation with the renderer backend supplied by the caller. Manual
/// passes the backend that is actually presenting its window; command-line
/// validation uses [`crate::app_config::resolve_want_gpu`] through the wrapper
/// above because no renderer has been constructed there yet.
pub(crate) fn config_host_semantic_warnings_with_backend(
    config: &crate::app_config::Config,
    backend_gpu: bool,
) -> Vec<ConfigSemanticWarning> {
    let assets =
        config.resolve_asset_catalog_with_themes(crate::app_config::ThemeCatalog::discover());
    config_host_semantic_warnings_with_backend_and_assets(
        config,
        backend_gpu,
        &assets,
        crate::net_listen::launched_inside_aterm(),
    )
}

/// Host validation against the exact immutable non-text asset generation
/// admitted with `config`. Manual passes its [`ConfigSnapshot`](crate::native_config_service::ConfigSnapshot)
/// catalog here so theme, Trail Pack, and rainbow kitty diagnostics cannot race a live
/// directory/file change or disagree with what the renderer actually applies.
///
/// `nested` — whether this process was launched inside another aterm
/// ([`crate::net_listen::launched_inside_aterm`]) — is supplied by the CALLER.
/// It is process-global environment state, and the listener arms below branch on
/// it, so reading it here would make this projection depend on how the host
/// process happened to be started: the suite's own listener tests then see the
/// no-bind arm whenever they are run from inside aterm. Same law the pure
/// `listener_capability_warnings` already states, one frame up.
pub(crate) fn config_host_semantic_warnings_with_backend_and_assets(
    config: &crate::app_config::Config,
    backend_gpu: bool,
    assets: &crate::app_config::ConfigAssetCatalog,
    nested: bool,
) -> Vec<ConfigSemanticWarning> {
    let mut warnings = Vec::new();

    // Environment/CLI precedence is a host fact, so Manual resolves it in this
    // off-thread lane. Only authored keys receive diagnostics: an absent key
    // has no misleading TOML token to underline, while Settings still exposes
    // the active value in its ordinary row projection.
    for key in [
        "columns",
        "lines",
        "font_px",
        "font_family",
        "gpu",
        "tab_strip_rows",
        "stem_gamma",
        "window_theme",
        "shell",
        "net.listen",
        "net.cert",
        "net.key",
        "update.owner",
        "update.repo",
        "update.auto_apply",
        "packages.account",
    ] {
        if config_key_is_authored(config, key)
            && let Some(active) = crate::app_config::active_environment_override(key)
        {
            warnings.push(ConfigSemanticWarning {
                key,
                message: format!(
                    "{key}: ${} overrides the saved value; effective value is {}",
                    active.variable, active.effective
                ),
            });
        }
    }

    // W5i: `shell` names the execve TARGET, used VERBATIM by the spawn
    // (crates/aterm-pty `spawn_shell_with_pid`: "a bare name relies on it being
    // absolute" — there is NO PATH search). A typo here is the config error
    // with the worst failure mode in the product: every new session dies at
    // exec, and before the launch-alert work the app just bounced and
    // vanished. Validate the authored value so `--validate-config` (and
    // Manual) says so BEFORE a session has to fail. `$ATERM_SHELL` precedence
    // is already surfaced by the env-override warning above; this checks the
    // saved key, which is what survives a restart.
    if let Some(shell) = config.shell.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if !shell.contains('/') {
            warnings.push(ConfigSemanticWarning {
                key: "shell",
                message: format!(
                    "shell: {shell:?} is a bare name, and the spawn execs it verbatim \
                     (no PATH search) — new sessions will fail. Use an absolute path \
                     (e.g. /bin/zsh)"
                ),
            });
        } else {
            match std::fs::metadata(shell) {
                Err(_) => warnings.push(ConfigSemanticWarning {
                    key: "shell",
                    message: format!(
                        "shell: {shell:?} does not exist — new sessions will fail to spawn"
                    ),
                }),
                Ok(md) => {
                    #[cfg(unix)]
                    let executable = {
                        use std::os::unix::fs::PermissionsExt as _;
                        md.is_file() && md.permissions().mode() & 0o111 != 0
                    };
                    #[cfg(not(unix))]
                    let executable = md.is_file();
                    if !executable {
                        warnings.push(ConfigSemanticWarning {
                            key: "shell",
                            message: format!(
                                "shell: {shell:?} is not an executable file — new sessions \
                                 will fail to spawn"
                            ),
                        });
                    }
                }
            }
        }
    }

    if config.font_family.is_some()
        && let Some(message) = crate::app_config::Config::font_family_warning(
            crate::effective_font_family(config.font_family_request().as_deref()).as_deref(),
        )
    {
        warnings.push(ConfigSemanticWarning {
            key: "font_family",
            message,
        });
    }

    for appearance in [
        aterm_types::Appearance::Dark,
        aterm_types::Appearance::Light,
    ] {
        if let Some(name) = config.resolve_theme_name(appearance)
            && !name.eq_ignore_ascii_case("default")
            && let Err(error) = assets.themes.resolve(&name)
        {
            let message =
                format!("theme: {name:?} does not resolve ({error}); Default used at load");
            if !warnings
                .iter()
                .any(|warning: &ConfigSemanticWarning| warning.message == message)
            {
                warnings.push(ConfigSemanticWarning {
                    key: "theme",
                    message,
                });
            }
        }
    }

    warnings.extend(
        assets
            .trail_packs
            .diagnostics
            .iter()
            .cloned()
            .map(|message| ConfigSemanticWarning {
                key: "cursor_trail_packs",
                message,
            }),
    );
    if let Some(message) = config.cursor_trail_style_warning(&assets.trail_packs) {
        warnings.push(ConfigSemanticWarning {
            key: "cursor_trail_style",
            message,
        });
    }
    // The typing-sound picker's domain: an unknown spelling plays `auto`
    // silently at runtime, so the validator is where the author hears of it.
    if let Some(message) = config.trail_sound_style_warning() {
        warnings.push(ConfigSemanticWarning {
            key: crate::prefs::EDIT_TRAIL_SOUND_STYLE,
            message,
        });
    }
    if let Some(reason) = assets.kitty_sprite.diagnostic() {
        let source = assets
            .kitty_sprite
            .source_id()
            .unwrap_or("configured source");
        warnings.push(ConfigSemanticWarning {
            // The TOML key keeps its shipped spelling (renaming it would break every
            // config that sets it); the message names the Settings row the user sees,
            // which is now "Rainbow kitty sprite", so the two surfaces connect.
            key: "cursor_nyan_sprite",
            message: format!(
                "cursor_nyan_sprite (the Rainbow kitty sprite) {source:?} is invalid ({reason}); \
                 disabled"
            ),
        });
    }

    let (_, font_warnings) = crate::app_config::FontConfig::from_config(config);
    for message in font_warnings {
        let key = [
            "font_family_bold_italic",
            "font_family_bold",
            "font_family_italic",
            "fallback_fonts",
            "symbol_font",
            "emoji_font",
        ]
        .into_iter()
        .find(|key| message.starts_with(&format!("config {key}:")))
        .unwrap_or("font_family");
        warnings.push(ConfigSemanticWarning { key, message });
    }

    if let Some(path) = config
        .sparkle_words
        .as_ref()
        .and_then(|sparkle| sparkle.lexicon.as_deref())
    {
        let expanded = crate::app_config::sparkle_expand_tilde(path);
        match aterm_effects::file_feed::read_bounded_regular_utf8(
            &expanded,
            aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES,
        ) {
            Ok(contents) => {
                let languages = config.sparkle_languages();
                let languages = languages.iter().map(String::as_str).collect::<Vec<_>>();
                if let Err(error) =
                    aterm_lexicon::Lexicon::with_languages_and_override(&languages, Some(&contents))
                {
                    warnings.push(ConfigSemanticWarning {
                        key: "sparkle_words.lexicon",
                        message: format!(
                            "sparkle_words.lexicon {expanded:?} is rejected ({error}); that lexicon layer is skipped"
                        ),
                    });
                }
            }
            Err(error) => warnings.push(ConfigSemanticWarning {
                key: "sparkle_words.lexicon",
                message: format!(
                    "sparkle_words.lexicon {expanded:?} is unreadable ({error}); that lexicon layer is skipped"
                ),
            }),
        }
    }

    warnings.extend(
        config
            .sparkle_toy_pack_warnings()
            .into_iter()
            .map(|message| ConfigSemanticWarning {
                key: "sparkle_words.toy_packs",
                message,
            }),
    );
    if let Some((deco, override_toml)) = config.sparkle_runtime_parts() {
        let languages = config.sparkle_languages();
        let languages = languages.iter().map(String::as_str).collect::<Vec<_>>();
        match aterm_lexicon::Lexicon::with_languages_and_override(
            &languages,
            override_toml.as_deref(),
        ) {
            Ok(lexicon) => {
                warnings.extend(
                    lexicon
                        .conflicts()
                        .iter()
                        .filter(|warning| {
                            aterm_effects::pipeline::lexicon_warning_applies(
                                warning,
                                deco.cjk_single_char,
                            )
                        })
                        .map(|warning| sparkle_lexicon_conflict_warning(config, warning)),
                );
            }
            Err(error) => warnings.push(ConfigSemanticWarning {
                key: "sparkle_words",
                message: format!(
                    "the merged sparkle_words lexicon is rejected ({error}); runtime falls back to the built-in lexicon"
                ),
            }),
        }
    }
    let listener_inputs = crate::net_listen::listener_inputs(config);
    let effective_listener_fields = listener_inputs.presence();
    warnings.extend(listener_capability_warnings(
        config,
        effective_listener_fields,
        nested,
    ));
    if !nested && effective_listener_fields.into_iter().all(|present| present) {
        match crate::net_listen::preflight_listener(&listener_inputs) {
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => {
                warnings.extend(
                    error
                        .config_keys()
                        .iter()
                        .map(|&key| ConfigSemanticWarning {
                            key,
                            message: error.to_string(),
                        }),
                )
            }
        }
    }

    let package_disabled = std::env::var_os("ATPKG_DISABLE").is_some();
    // The compiled anchor is the ONLY anchor. `ATPKG_ROOTKEY_OVERRIDE` used to be
    // consulted here and could enable an unpinned build; it is gone, because an
    // environment variable must never decide what is trusted.
    let package_root_available = !atpkg::PINNED_PKG_ROOTKEY.is_empty();
    warnings.extend(package_capability_warnings(
        config,
        package_disabled,
        package_root_available,
    ));
    warnings.extend(config_backend_capability_warnings(
        config,
        backend_gpu,
        ConfigCapabilityPlatform::CURRENT,
    ));
    warnings
}

fn first_authored_listener_key(config: &crate::app_config::Config) -> Option<&'static str> {
    let net = config.net.as_ref()?;
    if net.listen.is_some() {
        Some("net.listen")
    } else if net.cert.is_some() {
        Some("net.cert")
    } else if net.key.is_some() {
        Some("net.key")
    } else {
        None
    }
}

/// Pure listener admission projection. The caller supplies effective env/config
/// presence and nesting so the no-bind arms can be exhaustively tested without
/// mutating process-global environment state.
fn listener_capability_warnings(
    config: &crate::app_config::Config,
    effective_fields: [bool; 3],
    nested: bool,
) -> Vec<ConfigSemanticWarning> {
    let Some(key) = first_authored_listener_key(config) else {
        return Vec::new();
    };
    if nested {
        return vec![ConfigSemanticWarning {
            key,
            message: format!(
                "{key}: the network listener never binds in an aterm child/nested session, even when net.listen, net.cert, and net.key are complete"
            ),
        }];
    }
    let count = effective_fields
        .into_iter()
        .filter(|present| *present)
        .count();
    if count == 1 || count == 2 {
        vec![ConfigSemanticWarning {
            key,
            message: format!(
                "{key}: the effective network-listener configuration is incomplete ({count}/3 of listen, cert, and key); no port is bound until all three resolve"
            ),
        }]
    } else {
        Vec::new()
    }
}

fn first_authored_package_key(config: &crate::app_config::Config) -> Option<&'static str> {
    let packages = config.packages.as_ref()?;
    if packages.enabled.is_some() {
        Some("packages.enabled")
    } else if packages.auto_update.is_some() {
        Some("packages.auto_update")
    } else if packages.auto_install.is_some() {
        Some("packages.auto_install")
    } else if packages.account.is_some() {
        Some("packages.account")
    } else if packages.channel.is_some() {
        Some("packages.channel")
    } else if packages.include.is_some() {
        Some("packages.include")
    } else if packages.exclude.is_some() {
        Some("packages.exclude")
    } else {
        Some("packages")
    }
}

/// Pure atpkg admission projection. The package preferences remain parseable
/// while disabled, but no package operation may act without both operator
/// opt-in and a verification root.
fn package_capability_warnings(
    config: &crate::app_config::Config,
    disabled: bool,
    root_available: bool,
) -> Vec<ConfigSemanticWarning> {
    let Some(key) = first_authored_package_key(config) else {
        return Vec::new();
    };
    let reason = if disabled {
        Some("$ATPKG_DISABLE is set")
    } else if !root_available {
        Some("no package verification root is pinned in this build")
    } else {
        None
    };
    reason.map_or_else(Vec::new, |reason| {
        vec![ConfigSemanticWarning {
            key,
            message: format!(
                "{key}: package install/update operations are currently inert because {reason}; atpkg fails closed and performs no install or update"
            ),
        }]
    })
}

/// Native compositor families relevant to config capability validation.
/// Keeping this explicit (instead of a `target_macos` boolean) matters because
/// Windows consumes `background_material` as Mica/Mica Alt/Acrylic without the
/// macOS translucency precondition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))] // non-host variants drive the cross-platform test matrix
pub(crate) enum ConfigCapabilityPlatform {
    MacOs,
    Windows,
    Unsupported,
}

impl ConfigCapabilityPlatform {
    #[cfg(target_os = "macos")]
    const CURRENT: Self = Self::MacOs;
    #[cfg(windows)]
    const CURRENT: Self = Self::Windows;
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    const CURRENT: Self = Self::Unsupported;
}

/// Deterministic capability half of host validation. `platform` is an explicit
/// input so every platform/backend arm can be exhaustively tested on one build
/// host rather than hidden behind conditional compilation.
pub(crate) fn config_backend_capability_warnings(
    config: &crate::app_config::Config,
    backend_gpu: bool,
    platform: ConfigCapabilityPlatform,
) -> Vec<ConfigSemanticWarning> {
    use crate::app_config::BackgroundMaterial;

    let mut warnings = Vec::new();
    let opacity = config.background_opacity_or_default();
    if config.background_opacity.is_some() && opacity < 1.0 {
        let issue = if !backend_gpu {
            Some(
                "the current CPU renderer has no translucent present path; the window stays solid (the saved value applies with the macOS GPU renderer)",
            )
        } else if platform != ConfigCapabilityPlatform::MacOs {
            Some(
                "this platform has no implemented per-pixel translucent window consumer; the GPU still presents a solid grid (Windows background_material can style DWM chrome independently)",
            )
        } else {
            None
        };
        if let Some(issue) = issue {
            warnings.push(ConfigSemanticWarning {
                key: "background_opacity",
                message: format!("background_opacity requests translucency, but {issue}"),
            });
        }
    }

    let material = config
        .background_material
        .as_deref()
        .and_then(BackgroundMaterial::parse)
        .unwrap_or(BackgroundMaterial::None);
    if material != BackgroundMaterial::None {
        let issue = match platform {
            ConfigCapabilityPlatform::Unsupported => Some((
                "this platform has no native window-material consumer",
                "it is supported only by the macOS and Windows GPU renderers",
            )),
            ConfigCapabilityPlatform::MacOs if !backend_gpu => Some((
                "the current CPU renderer cannot composite a window material",
                "it requires the macOS GPU renderer and background_opacity < 1",
            )),
            ConfigCapabilityPlatform::MacOs if opacity >= 1.0 => Some((
                "background_opacity resolves to 1 (solid)",
                "it requires the macOS GPU renderer and background_opacity < 1",
            )),
            ConfigCapabilityPlatform::Windows if !backend_gpu => Some((
                "the current CPU renderer does not install a DWM system backdrop",
                "Mica, Mica Alt, and Acrylic require the Windows GPU renderer",
            )),
            ConfigCapabilityPlatform::MacOs | ConfigCapabilityPlatform::Windows => None,
        };
        if let Some((reason, requirement)) = issue {
            warnings.push(ConfigSemanticWarning {
                key: "background_material",
                message: format!(
                    "background_material has no effect because {reason}; {requirement}"
                ),
            });
        }
    }

    if config.window_colorspace.is_some()
        && (platform != ConfigCapabilityPlatform::MacOs || !backend_gpu)
    {
        let reason = if platform != ConfigCapabilityPlatform::MacOs {
            "this platform has no macOS CAMetalLayer to tag"
        } else {
            "the current CPU renderer has no GPU CAMetalLayer"
        };
        warnings.push(ConfigSemanticWarning {
            key: "window_colorspace",
            message: format!(
                "window_colorspace has no effect because {reason}; it applies only to the macOS GPU window layer"
            ),
        });
    }
    if !backend_gpu {
        for (key, authored) in [
            (
                crate::prefs::EDIT_CURSOR_TRAIL_BLOOM,
                config.cursor_trail_bloom.is_some(),
            ),
            (
                crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_STRENGTH,
                config.cursor_trail_bloom_strength.is_some(),
            ),
            (
                crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_RADIUS,
                config.cursor_trail_bloom_radius.is_some(),
            ),
            (
                crate::prefs::EDIT_CURSOR_FIRE_SHIMMER,
                config.cursor_fire_shimmer.is_some(),
            ),
            (crate::prefs::EDIT_HDR_GLOW, config.hdr_glow.is_some()),
            (
                crate::prefs::EDIT_CURSOR_GLOW_SDR_BOOST,
                config.cursor_glow_sdr_boost.is_some(),
            ),
        ] {
            if authored {
                warnings.push(ConfigSemanticWarning {
                    key,
                    message: format!(
                        "{key} is parsed and preserved but has no effect while the CPU renderer is active; it requires the GPU renderer"
                    ),
                });
            }
        }
    }
    if config.font_thicken.is_some() && platform != ConfigCapabilityPlatform::MacOs {
        warnings.push(ConfigSemanticWarning {
            key: "font_thicken",
            message: "font_thicken is parsed and preserved but has no effect on this platform; it requires macOS CoreText smoothing"
                .to_string(),
        });
    }
    // The one security key whose silence would be worst: a user who authored a
    // PROTECTION must not believe it holds on a platform where it cannot.
    if config.secure_keyboard_entry.is_some() && platform != ConfigCapabilityPlatform::MacOs {
        warnings.push(ConfigSemanticWarning {
            key: crate::prefs::EDIT_SECURE_KEYBOARD_ENTRY,
            message: "secure_keyboard_entry is parsed and preserved but has no protective effect on this platform; Secure Keyboard Entry is a macOS mechanism                       (Carbon secure input)"
                .to_string(),
        });
    }
    if platform != ConfigCapabilityPlatform::MacOs {
        for (key, authored) in [
            (
                crate::prefs::EDIT_TRAIL_SOUNDS,
                config.trail_sounds.is_some(),
            ),
            (
                crate::prefs::EDIT_TRAIL_SOUND_VOLUME,
                config.trail_sound_volume.is_some(),
            ),
            // The rest of the Sound menu's SYNTH voices. They ride the same
            // macOS-only `trail_audio` output as the two keys above, and the
            // Sound menu made them reachable from Settings, so a portable
            // `aterm.toml` must get the same honest warning for all of them.
            (
                crate::prefs::EDIT_TRAIL_SOUND_STYLE,
                config.trail_sound_style.is_some(),
            ),
            (crate::prefs::EDIT_TONE_MELODY, config.tone_melody.is_some()),
            (crate::prefs::EDIT_ROBI, config.robi.is_some()),
            (
                crate::prefs::EDIT_TRAIL_SOUND_BED,
                config.trail_sound_bed.is_some(),
            ),
            (
                crate::prefs::EDIT_TRAIL_SOUND_RIFF,
                config.trail_sound_riff.is_some(),
            ),
        ] {
            if authored {
                warnings.push(ConfigSemanticWarning {
                    key,
                    message: format!(
                        "{key} is parsed and preserved but has no effect on this platform; cursor-trail audio is currently implemented only on macOS"
                    ),
                });
            }
        }
    }
    if platform == ConfigCapabilityPlatform::Unsupported {
        // `confirm_multiline_paste` has NO warning here any more: it is live on
        // every platform — the macOS sheet, the Windows MessageBoxW, and the
        // Linux in-window `paste_banner` (the clipboard sweep closed the
        // audited silent no-op this arm used to describe).
        if config.allow_notifications.is_some() {
            warnings.push(ConfigSemanticWarning {
                key: "allow_notifications",
                message: "allow_notifications is parsed and preserved but has no effect on this platform because desktop-notification delivery is currently implemented only on macOS and Windows"
                    .to_string(),
            });
        }
        // The audible BEL is the macOS/Windows pair, not the macOS-only synth:
        // `NSBeep` and `MessageBeep` both exist, and nothing else does — so this
        // key belongs in the Unsupported block with its platform twins above,
        // NOT in the macOS-only trail-audio block.
        if config.bell_sound.is_some() {
            warnings.push(ConfigSemanticWarning {
                key: crate::prefs::EDIT_BELL_SOUND,
                message: "bell_sound is parsed and preserved but has nothing to suppress on this platform because the audible terminal bell is implemented only with the macOS and Windows system alert sound"
                    .to_string(),
            });
        }
    }
    if platform != ConfigCapabilityPlatform::MacOs
        && let Some(update) = config.update.as_ref()
    {
        for (key, authored) in [
            ("update.owner", update.owner.is_some()),
            ("update.repo", update.repo.is_some()),
            ("update.auto_apply", update.auto_apply.is_some()),
        ] {
            if authored {
                warnings.push(ConfigSemanticWarning {
                    key,
                    message: format!(
                        "{key} is parsed and preserved but has no effect on this platform because the in-app updater is macOS-only"
                    ),
                });
            }
        }
    }
    warnings
}

fn sparkle_lexicon_conflict_warning(
    config: &crate::app_config::Config,
    conflict: &str,
) -> ConfigSemanticWarning {
    let Some(sparkle) = config.sparkle_words.as_ref() else {
        return ConfigSemanticWarning {
            key: "sparkle_words",
            message: format!("sparkle_words lexicon conflict: {conflict}"),
        };
    };

    if let Some(custom) = sparkle.custom.as_deref() {
        for (record_index, record) in custom.iter().enumerate() {
            for (word_index, word) in record.words.iter().enumerate() {
                if lexicon_conflict_mentions_word(conflict, word) {
                    return ConfigSemanticWarning {
                        key: "sparkle_words.custom.words",
                        message: format!(
                            "sparkle_words.custom.words in [[sparkle_words.custom]] record {} word {} has a merged lexicon conflict: {conflict}",
                            record_index + 1,
                            word_index + 1,
                        ),
                    };
                }
            }
        }
    }

    for (key, words) in [
        (
            "sparkle_words.profanity.extra_words",
            sparkle
                .profanity
                .as_ref()
                .and_then(|category| category.extra_words.as_deref()),
        ),
        (
            "sparkle_words.feline.extra_words",
            sparkle
                .feline
                .as_ref()
                .and_then(|category| category.extra_words.as_deref()),
        ),
        (
            "sparkle_words.orca.extra_words",
            sparkle
                .orca
                .as_ref()
                .and_then(|category| category.extra_words.as_deref()),
        ),
        (
            "sparkle_words.emphasis.extra_words",
            sparkle
                .emphasis
                .as_ref()
                .and_then(|category| category.extra_words.as_deref()),
        ),
    ] {
        if let Some((index, _)) = words.and_then(|words| {
            words
                .iter()
                .enumerate()
                .find(|(_, word)| lexicon_conflict_mentions_word(conflict, word))
        }) {
            return ConfigSemanticWarning {
                key,
                message: format!("{key}[{index}]: merged lexicon conflict: {conflict}"),
            };
        }
    }

    if sparkle.lexicon.is_some() {
        ConfigSemanticWarning {
            key: "sparkle_words.lexicon",
            message: format!("sparkle_words.lexicon merged-layer conflict: {conflict}"),
        }
    } else if sparkle
        .toy_packs
        .as_ref()
        .is_some_and(|paths| !paths.is_empty())
    {
        ConfigSemanticWarning {
            key: "sparkle_words.toy_packs",
            message: format!("sparkle_words.toy_packs merged-layer conflict: {conflict}"),
        }
    } else {
        ConfigSemanticWarning {
            key: "sparkle_words",
            message: format!("sparkle_words merged lexicon conflict: {conflict}"),
        }
    }
}

fn lexicon_conflict_mentions_word(conflict: &str, word: &str) -> bool {
    let word = word.trim();
    !word.is_empty() && conflict.contains(&format!("{word:?}"))
}

fn config_key_is_authored(config: &crate::app_config::Config, key: &str) -> bool {
    match key {
        "columns" => config.columns.is_some(),
        "lines" => config.lines.is_some(),
        "font_px" => config.font_px.is_some(),
        "font_family" => config.font_family.is_some(),
        "gpu" => config.gpu.is_some(),
        "tab_strip_rows" => config.tab_strip_rows.is_some(),
        "stem_gamma" => config.stem_gamma.is_some(),
        "window_theme" => config.window_theme.is_some(),
        "shell" => config.shell.is_some(),
        "net.listen" => config.net.as_ref().is_some_and(|net| net.listen.is_some()),
        "net.cert" => config.net.as_ref().is_some_and(|net| net.cert.is_some()),
        "net.key" => config.net.as_ref().is_some_and(|net| net.key.is_some()),
        "update.owner" => config
            .update
            .as_ref()
            .is_some_and(|update| update.owner.is_some()),
        "update.repo" => config
            .update
            .as_ref()
            .is_some_and(|update| update.repo.is_some()),
        "update.auto_apply" => config
            .update
            .as_ref()
            .is_some_and(|update| update.auto_apply.is_some()),
        "packages.account" => config
            .packages
            .as_ref()
            .is_some_and(|packages| packages.account.is_some()),
        _ => false,
    }
}

/// Parse `text` as the `aterm.toml` config, returning the (possibly empty) list of
/// soft WARNINGS for a structurally-valid file, or a hard `Err` (with the toml error's
/// line/column) for a TOML syntax error. The explicit CLI validation path may
/// resolve referenced assets; the shared semantic subset above remains pure.
///
/// A clean TOML parse is not the whole story: the loader warn-skips bad chords /
/// escapes / actions in `[key_sequences]` and `[keybindings]` at runtime, so an entry
/// that parses as a string can still silently never work. Those are returned as
/// warnings so the diagnostic doesn't print a false green.
pub(crate) fn validate_config_text(text: &str) -> Result<Vec<String>, String> {
    if text.len() > crate::native_config_service::MAX_CONFIG_FILE_BYTES {
        return Err(format!(
            "config exceeds the {}-byte admission limit",
            crate::native_config_service::MAX_CONFIG_FILE_BYTES
        ));
    }
    let config = toml::from_str::<crate::app_config::Config>(text).map_err(|e| e.to_string())?;
    let mut warnings = config_semantic_warnings(&config)
        .into_iter()
        .map(|warning| warning.message)
        .collect::<Vec<_>>();
    // W5h: keys the loaders warn-skip (falling back to a default) also flagged
    // here, so `--validate-config` can't false-green a config whose font/theme/
    // cursor/colours silently do nothing at load.
    //
    // Cursor-style spellings (unknown values fall back to a blinking block;
    // the retired underline spelling is preserved but resolves to a bar).
    if let Some(style) = config.cursor_style.as_deref() {
        let normalized = style.trim().to_ascii_lowercase();
        if normalized == "underline" {
            warnings.push(
                "cursor_style = \"underline\" is retired and renders as \"bar\"; use \"bar\" explicitly"
                    .to_string(),
            );
        } else if !matches!(normalized.as_str(), "block" | "bar" | "beam") {
            warnings.push(format!(
                "cursor_style: expected block|bar, got {style:?} (block used at load)"
            ));
        }
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
    warnings.extend(
        config_host_semantic_warnings(&config)
            .into_iter()
            .map(|warning| warning.message),
    );
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
        Some(p) => validate_config_path(&p),
    }
}

fn validate_config_path(path: &std::path::Path) -> (String, bool) {
    match crate::native_config_service::VersionedConfigService::observe_path(path, true) {
        Err(error) => (
            format!("config {} is unreadable: {error}", path.display()),
            false,
        ),
        Ok(observation) if !observation.baseline.observed.exists => (
            format!(
                "no config file at {} — built-in defaults in use (OK)",
                path.display()
            ),
            true,
        ),
        Ok(observation) => match validate_config_text(&observation.text) {
            Ok(w) if w.is_empty() => (format!("config {} is valid", path.display()), true),
            Ok(w) => (
                format!(
                    "config {} is structurally valid, with {} runtime warning{}:\n  {}",
                    path.display(),
                    w.len(),
                    if w.len() == 1 { "" } else { "s" },
                    w.join("\n  ")
                ),
                true,
            ),
            Err(e) => (format!("config {} is INVALID:\n{e}", path.display()), false),
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
/// — PER PLATFORM: on macOS the fixed Cmd-* bindings handled in `on_key`, off
/// macOS the seeded table `Keybindings::platform_defaults` installs. Printing
/// `BUILTIN_CMD_CHORDS` unconditionally (the old behaviour) documented thirty
/// `cmd+*` chords on Windows and not one that works: `Chord::from_event` maps
/// `cmd` from `super_key()`, so "cmd+t" literally means Win+T — a keystroke the
/// shell steals before aterm ever sees it. Then any user `[keybindings]`
/// overrides from the effective config (parsed, malformed entries skipped). The
/// bindable action NAMES come from [`crate::keybinding::ACTION_NAMES`].
pub(crate) fn list_keybinds() -> String {
    let mut s = String::new();
    #[cfg(target_os = "macos")]
    {
        let _ = writeln!(s, "built-in keybindings (in the window):");
        for (chord, label) in crate::keybinding::BUILTIN_CMD_CHORDS {
            let _ = writeln!(s, "  {chord:<16} {label}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = writeln!(
            s,
            "built-in default keybindings (each rebindable via [keybindings]):"
        );
        for (chord, action) in crate::keybinding::Keybindings::PLATFORM_DEFAULT_PAIRS {
            let _ = writeln!(s, "  {chord:<18} {action}");
        }
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
                let note = user_keybinding_note(chord, action);
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
    // The unbind spelling is part of the value surface too (it is not an action,
    // so it cannot live in ACTION_NAMES — a test there asserts every name
    // parses): document it where a user shopping for values is already looking.
    let _ = writeln!(
        s,
        "  none | unbind      (not an action: unbinds the chord's built-in default)"
    );
    s
}

/// The inline annotation for one user `[keybindings]` row in [`list_keybinds`],
/// extracted so the spellings are testable without a config file on disk. The
/// unbind spelling is checked FIRST: `"f11" = "none"` is the documented way to
/// free a seeded default, and printing `(UNKNOWN action)` for it (the old
/// behaviour) told the user their working config was broken.
fn user_keybinding_note(chord: &str, action: &str) -> String {
    user_keybinding_note_for(action, crate::keybinding::builtin_shadow_label(chord))
}

/// The note's spelling with the built-in shadow RESOLVED — the same gate-
/// explicit shape as [`keybinding_row_warning`], and for the same reason: the
/// Cmd/Super suite is compiled off on Linux, so only a resolved label lets a
/// test see the macOS-shaped path where the review's escape lived.
fn user_keybinding_note_for(action: &str, shadow: Option<&'static str>) -> String {
    if crate::keybinding::is_unbind_action(action) {
        // An unbind masks a SEED. Where a hardcoded built-in also claims the
        // chord, that half survives the unbind — say both, or the listing
        // implies the chord is now free (the review's macOS `cmd+c` case).
        match shadow {
            Some(lbl) => format!("  (unbinds a default; still conflicts with {lbl})"),
            None => "  (unbinds a default)".to_string(),
        }
    } else if crate::keybinding::Action::parse(action).is_none() {
        "  (UNKNOWN action)".to_string()
    } else if let Some(lbl) = shadow {
        format!("  (conflicts with {lbl})")
    } else {
        String::new()
    }
}

fn show_config_font_px(value: f32, explicit: bool) -> String {
    if explicit {
        format!("{value} (explicit physical px)")
    } else {
        format!("{value} (auto base; final physical px depends on display scale)")
    }
}

/// `--show-config`: the resolved launch config after applying the
/// env > config > default precedence. Most values are final before a window
/// exists. An unset font size is necessarily reported as its auto-scale base:
/// the final physical size is selected only when the real window/display scale
/// is known. The config FILE path + presence is shown so the reader knows
/// whether any of this came from disk.
pub(crate) fn show_config() -> String {
    let config = crate::app_config::load_config();
    let (config_path, config_exists) = match crate::app_config::config_path() {
        Some(p) => (p.display().to_string(), p.exists()),
        None => ("(no HOME / XDG_CONFIG_HOME)".to_string(), false),
    };
    let gpu = crate::app_config::resolve_want_gpu(&config);
    let font_px = show_config_font_px(
        crate::app_config::resolve_font_px(&config),
        crate::app_config::font_px_is_explicit(&config),
    );
    let tab_strip_rows = crate::app_config::resolve_tab_strip_rows(&config);
    let theme_name = config
        .theme
        .clone()
        .unwrap_or_else(|| "Default".to_string());
    let themes = crate::app_config::ThemeCatalog::discover();
    let tc = config.applied_terminal_config_for_with_assets(aterm_types::Appearance::Dark, &themes);
    // The same effective resolution the renderer uses (env > config > platform
    // default), so `--show-config` reports the face that actually loads — on a
    // pristine config that is "(built-in candidates)", the library's
    // FONT_CANDIDATES lead (SF Mono on macOS); `--show-face` names the file.
    let font_family = crate::effective_font_family(config.font_family_request().as_deref())
        .unwrap_or_else(|| "(built-in candidates)".to_string());
    let columns = crate::app_config::resolve_initial_columns(&config);
    let lines = crate::app_config::resolve_initial_lines(&config);

    let mut s = String::new();
    let _ = writeln!(s, "resolved launch config (env > config > default)");
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
        crate::effective_font_family(config.font_family_request().as_deref()).unwrap_or_default()
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

    fn parsed(source: &str) -> crate::app_config::Config {
        toml::from_str(source).expect("test config")
    }

    #[test]
    fn semantic_warnings_match_dependent_runtime_clamps() {
        let config = parsed(
            "[sparkle_words.ink]\nloop = true\nsweep_ms = 400\n\
             [matrix_rain]\nalpha = 120\nhead_alpha = 20\n",
        );
        let warnings = config_semantic_warnings(&config);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().any(|warning| {
            warning.key == "sparkle_words.ink.sweep_ms"
                && warning.message.contains("effectively 600 ms")
        }));
        assert!(warnings.iter().any(|warning| {
            warning.key == "matrix_rain.head_alpha" && warning.message.contains("effectively 120")
        }));

        let clean = parsed(
            "[sparkle_words.ink]\nloop = true\nsweep_ms = 600\n\
             [matrix_rain]\nalpha = 120\nhead_alpha = 120\n",
        );
        assert!(config_semantic_warnings(&clean).is_empty());
    }

    #[test]
    fn capability_warning_matrix_distinguishes_macos_windows_and_unsupported() {
        let translucent = parsed(
            "background_opacity = 0.7\nbackground_material = \"hud\"\n\
             window_colorspace = \"display-p3\"\n",
        );
        let keys = |gpu, platform| {
            config_backend_capability_warnings(&translucent, gpu, platform)
                .into_iter()
                .map(|warning| warning.key)
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert!(keys(true, ConfigCapabilityPlatform::MacOs).is_empty());
        assert_eq!(
            keys(false, ConfigCapabilityPlatform::MacOs),
            std::collections::BTreeSet::from([
                "background_material",
                "background_opacity",
                "window_colorspace",
            ])
        );
        assert_eq!(
            keys(true, ConfigCapabilityPlatform::Windows),
            std::collections::BTreeSet::from(["background_opacity", "window_colorspace"]),
            "Windows GPU consumes DWM material, but not per-pixel grid opacity"
        );
        assert_eq!(
            keys(false, ConfigCapabilityPlatform::Windows),
            std::collections::BTreeSet::from([
                "background_material",
                "background_opacity",
                "window_colorspace",
            ])
        );
        assert_eq!(
            keys(true, ConfigCapabilityPlatform::Unsupported),
            std::collections::BTreeSet::from([
                "background_material",
                "background_opacity",
                "window_colorspace",
            ])
        );

        let opaque = parsed("background_material = \"sidebar\"\n");
        assert_eq!(
            config_backend_capability_warnings(&opaque, true, ConfigCapabilityPlatform::MacOs)
                .len(),
            1
        );
        assert!(
            config_backend_capability_warnings(&opaque, true, ConfigCapabilityPlatform::Windows)
                .is_empty()
        );

        let platform_only = parsed(
            "font_thicken = true\nsecure_keyboard_entry = true\n[update]\nowner = \"safe-owner\"\nrepo = \"aterm\"\nauto_apply = true\n",
        );
        let non_macos = config_backend_capability_warnings(
            &platform_only,
            true,
            ConfigCapabilityPlatform::Unsupported,
        );
        assert_eq!(
            non_macos
                .iter()
                .map(|warning| warning.key)
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from([
                "font_thicken",
                "secure_keyboard_entry",
                "update.auto_apply",
                "update.owner",
                "update.repo",
            ])
        );
        assert!(
            config_backend_capability_warnings(
                &platform_only,
                true,
                ConfigCapabilityPlatform::MacOs,
            )
            .is_empty()
        );
    }

    #[test]
    fn platform_warning_matrix_discloses_audio_dialog_and_notification_stubs() {
        let config = parsed(
            "trail_sounds = true\ntrail_sound_volume = 0.4\nconfirm_multiline_paste = true\nallow_notifications = true\n",
        );
        let keys = |platform| {
            config_backend_capability_warnings(&config, true, platform)
                .into_iter()
                .map(|warning| warning.key)
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert!(keys(ConfigCapabilityPlatform::MacOs).is_empty());
        assert_eq!(
            keys(ConfigCapabilityPlatform::Windows),
            std::collections::BTreeSet::from(["trail_sound_volume", "trail_sounds"]),
            "Windows has dialogs/notifications but no trail-audio consumer"
        );
        // `confirm_multiline_paste` is deliberately absent: the confirm is
        // live on every platform (Linux's in-window paste_banner included),
        // so warning that it "has no protective effect" would be the lie.
        assert_eq!(
            keys(ConfigCapabilityPlatform::Unsupported),
            std::collections::BTreeSet::from([
                "allow_notifications",
                "trail_sound_volume",
                "trail_sounds",
            ])
        );
    }

    #[test]
    fn pure_semantics_report_runtime_fallbacks_instead_of_false_green() {
        let source = r##"
font_px = 201
font_variation = ["wght=450", "not-an-axis"]
font_features = ["+ss01 toolong", "cv01=2"]

[update]
owner = ""
repo = "bad/repo"

[packages]
account = "bad/account"
channel = "   "

[matrix_rain]
hue = "blue"

[sparkle_words.profanity]
palette = ["#112233", "not-a-color"]
"##;
        let warnings = config_semantic_warnings(&parsed(source));
        let joined = warnings
            .iter()
            .map(|warning| warning.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "font_px",
            "ignores it rather than clamping",
            "font_variation[1]",
            "font_features[0]",
            "update.owner",
            "update.repo",
            "packages.account",
            "packages.channel is blank",
            "matrix_rain.hue",
            "sparkle_words.profanity.palette[1]",
        ] {
            assert!(joined.contains(expected), "missing {expected:?}: {joined}");
        }

        let clean = parsed(
            r##"font_px = 200
font_variation = ["wght=450"]
font_features = ["+ss01 -calt cv01=2"]
[update]
owner = "safe-owner"
repo = "safe.repo"
[packages]
account = "safe_owner"
channel = "stable"
[matrix_rain]
hue = "#12ABef"
[sparkle_words.profanity]
palette = ["#112233"]
"##,
        );
        assert!(
            config_semantic_warnings(&clean).is_empty(),
            "valid boundary/domain values stay clean"
        );
    }

    #[test]
    fn security_capability_warnings_name_the_gui_hosts_that_are_actually_missing() {
        let enabled = config_semantic_warnings(&parsed(
            "allow_osc52_query = true\nallow_window_ops = true\n",
        ));
        assert_eq!(enabled.len(), 2, "{enabled:?}");
        assert!(enabled.iter().any(|warning| {
            warning.key == "allow_osc52_query"
                // The honest caveat states the WIDEST grant on this platform:
                // the system clipboard off-Linux (that is what the Query arm
                // answers with — the old "own-slot only" line was a false
                // promise there), the own-selections bound on X11.
                && warning.message.contains("READ")
                && if cfg!(target_os = "linux") {
                    warning.message.contains("selections aterm itself owns")
                } else {
                    warning.message.contains("system clipboard")
                        && warning.message.contains("OTHER apps")
                }
        }));
        assert!(enabled.iter().any(|warning| {
            warning.key == "allow_window_ops"
                && warning.message.contains("window-title and text-grid-size")
                && if cfg!(target_os = "linux") {
                    // The manipulation half is wired there (frame audit #4);
                    // the honest residue is the geometry-report gap.
                    warning.message.contains("wired to the window")
                        && warning.message.contains("move stays denied")
                        && warning.message.contains("remain unanswered")
                } else {
                    warning.message.contains("host manipulation")
                        && warning.message.contains("most state/geometry")
                }
        }));

        assert!(
            config_semantic_warnings(&parsed(
                "allow_osc52_query = false\nallow_window_ops = false\n"
            ))
            .is_empty(),
            "disabled capabilities are the honest fail-closed default"
        );
    }

    #[test]
    fn custom_and_connection_records_disclose_every_inert_or_refused_arm() {
        let fingerprint = "00".repeat(32);
        let source = format!(
            r#"
[[sparkle_words.custom]]
words = [" "]

[[sparkle_words.custom]]
words = ["quiet"]
burst = {{ kind = "glow", chance = 0 }}

[[sparkle_words.custom]]
words = ["live"]
ink = "rainbow"

[[net.connections]]
name = "bad/name"
host = "bad.example:7100"
fingerprint = "nope"

[[net.connections]]
name = "work"
host = "one.example:7100"
fingerprint = "{fingerprint}"

[[net.connections]]
name = "work"
host = "two.example:7100"
fingerprint = "{fingerprint}"
expect_nonce = "launch-pin"
"#
        );
        let warnings = config_semantic_warnings(&parsed(&source));
        let joined = warnings
            .iter()
            .map(|warning| warning.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "record 1 has no nonblank word",
            "record 1 is inert",
            "record 2 is inert",
            "record 1 must be a nonempty",
            "record 1 must be 64 hexadecimal",
            "record 3 duplicates",
            "record 3 currently makes dialing fail closed",
        ] {
            assert!(joined.contains(expected), "missing {expected:?}: {joined}");
        }
        assert!(
            warnings
                .iter()
                .all(|warning| !warning.message.contains("record 3 is inert")),
            "live custom record must not be called inert"
        );
    }

    #[test]
    fn saved_connection_rejects_nonblank_endpoint_syntax_before_dial() {
        let fingerprint = "00".repeat(32);
        let config = parsed(&format!(
            "[[net.connections]]\nname = \"work\"\nhost = \"missing-port\"\nfingerprint = \"{fingerprint}\"\n"
        ));
        let warnings = config_semantic_warnings(&config);
        let endpoint = warnings
            .iter()
            .find(|warning| warning.key == "net.connections.host")
            .expect("invalid endpoint warning");
        assert!(endpoint.message.contains("must be host:port"));
        assert!(endpoint.message.contains("record 1"));
    }

    #[test]
    fn theme_derived_matrix_body_discloses_conditional_head_floor() {
        let warnings = config_semantic_warnings(&parsed("[matrix_rain]\nhead_alpha = 80\n"));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].message.contains("theme-derived body alpha"));

        let max = config_semantic_warnings(&parsed("[matrix_rain]\nhead_alpha = 135\n"));
        assert!(max.is_empty(), "the body cannot exceed the cap: {max:?}");
    }

    #[test]
    fn listener_and_package_admission_warnings_are_pure_and_specific() {
        let listener = parsed("[net]\nlisten = \"127.0.0.1:7100\"\n");
        let partial = listener_capability_warnings(&listener, [true, false, false], false);
        assert_eq!(partial.len(), 1);
        assert!(partial[0].message.contains("incomplete (1/3"));
        assert!(listener_capability_warnings(&listener, [true, true, true], false).is_empty());
        let nested = listener_capability_warnings(&listener, [true, true, true], true);
        assert_eq!(nested.len(), 1);
        assert!(nested[0].message.contains("never binds in an aterm child"));

        let packages = parsed("[packages]\nauto_update = true\n");
        let disabled = package_capability_warnings(&packages, true, true);
        assert_eq!(disabled.len(), 1);
        assert!(disabled[0].message.contains("$ATPKG_DISABLE"));
        let rootless = package_capability_warnings(&packages, false, false);
        assert_eq!(rootless.len(), 1);
        assert!(rootless[0].message.contains("verification root"));
        assert!(package_capability_warnings(&packages, false, true).is_empty());
    }

    #[test]
    fn host_semantics_accept_valid_lexicon_and_report_missing_or_rejected_layers() {
        // libtest names each test thread after the test's FULL PATH — here
        // "diagnostics::tests::host_semantics_…" — and `:` is not a legal
        // character in a Windows path component, so using the name verbatim
        // made `create_dir_all` fail with os error 123 (InvalidFilename) on
        // Windows and this test never ran there. Sanitize instead of dropping
        // the name: it is what keeps the directory unique per test.
        let thread = std::thread::current();
        let root = std::env::temp_dir().join(format!(
            "aterm-config-lexicon-{}-{}",
            std::process::id(),
            thread.name().unwrap_or("test").replace(':', "-")
        ));
        std::fs::create_dir_all(&root).unwrap();
        let valid = root.join("valid.toml");
        let invalid = root.join("invalid.toml");
        let conflict = root.join("conflict.toml");
        let oversized = root.join("oversized.toml");
        std::fs::write(
            &valid,
            "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"ultrathink\"]\n",
        )
        .unwrap();
        std::fs::write(&invalid, "[[entry]\nclass = \"emphasis\"\n").unwrap();
        std::fs::write(
            &conflict,
            "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"abc猫\"]\n",
        )
        .unwrap();
        std::fs::write(
            &oversized,
            vec![b'x'; aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES + 1],
        )
        .unwrap();

        let config_for = |path: &std::path::Path| {
            parsed(&format!(
                "[sparkle_words]\nlexicon = {:?}\n",
                path.to_string_lossy()
            ))
        };
        assert!(
            config_host_semantic_warnings(&config_for(&valid))
                .iter()
                .all(|warning| warning.key != "sparkle_words.lexicon")
        );
        let rejected = config_host_semantic_warnings(&config_for(&invalid));
        assert!(rejected.iter().any(|warning| {
            warning.key == "sparkle_words.lexicon" && warning.message.contains("rejected")
        }));
        let missing = config_host_semantic_warnings(&config_for(&root.join("missing.toml")));
        assert!(missing.iter().any(|warning| {
            warning.key == "sparkle_words.lexicon" && warning.message.contains("unreadable")
        }));
        let oversized_warnings = config_host_semantic_warnings(&config_for(&oversized));
        assert!(oversized_warnings.iter().any(|warning| {
            warning.key == "sparkle_words.lexicon"
                && warning.message.contains("unreadable")
                && warning
                    .message
                    .contains(&aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES.to_string())
        }));
        let conflict = config_host_semantic_warnings(&config_for(&conflict));
        assert!(conflict.iter().any(|warning| {
            warning.key == "sparkle_words.lexicon"
                && warning.message.contains("merged-layer conflict")
                && warning.message.contains("abc猫")
        }));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn host_semantics_mirrors_runtime_merged_lexicon_conflicts_and_cjk_filter() {
        let inline = parsed(
            r#"[sparkle_words.feline]
cjk_single_char = false

[[sparkle_words.custom]]
words = ["犬", "abc猫"]
ink = "rainbow"
"#,
        );
        let warnings = config_host_semantic_warnings(&inline);
        assert!(warnings.iter().any(|warning| {
            warning.key == "sparkle_words.custom.words"
                && warning.message.contains("record 1 word 1")
                && warning.message.contains("requires cjk_single_char = true")
        }));
        assert!(warnings.iter().any(|warning| {
            warning.key == "sparkle_words.custom.words"
                && warning.message.contains("record 1 word 2")
                && warning.message.contains("dropped")
        }));

        let opted_in = parsed(
            r#"[sparkle_words.feline]
cjk_single_char = true

[[sparkle_words.custom]]
words = ["犬", "abc猫"]
ink = "rainbow"
"#,
        );
        let opted_in = config_host_semantic_warnings(&opted_in);
        assert!(
            opted_in
                .iter()
                .all(|warning| !warning.message.contains("requires cjk_single_char = true")),
            "resolved opt-in must filter the same warning as recompute_sparkle: {opted_in:?}"
        );
        assert!(opted_in.iter().any(|warning| {
            warning.key == "sparkle_words.custom.words"
                && warning.message.contains("record 1 word 2")
        }));

        let extra = parsed("[sparkle_words.emphasis]\nextra_words = [\"abc猫\"]\n");
        assert!(config_host_semantic_warnings(&extra).iter().any(|warning| {
            warning.key == "sparkle_words.emphasis.extra_words"
                && warning
                    .message
                    .starts_with("sparkle_words.emphasis.extra_words[0]")
        }));
    }

    #[test]
    fn host_validation_reports_only_authored_environment_masking() {
        const CHILD: &str = "ATERM_HOST_OVERRIDE_WARNING_CHILD";
        const EXACT: &str =
            "diagnostics::tests::host_validation_reports_only_authored_environment_masking";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", EXACT, "--nocapture"])
                .env(CHILD, "1")
                .env("ATERM_COLUMNS", "120")
                .env("RUST_TEST_THREADS", "1")
                .status()
                .expect("launch isolated environment-precedence validation");
            assert!(status.success());
            return;
        }

        let authored: crate::app_config::Config = toml::from_str("columns = 80\n").unwrap();
        let warnings = config_host_semantic_warnings(&authored);
        assert!(warnings.iter().any(|warning| {
            warning.key == "columns"
                && warning.message.contains("$ATERM_COLUMNS overrides")
                && warning.message.contains("effective value is 120")
        }));

        let absent = config_host_semantic_warnings(&crate::app_config::Config::default());
        assert!(
            absent.iter().all(|warning| warning.key != "columns"),
            "an absent TOML token must not receive a source diagnostic"
        );
    }

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

    /// W5i: the `shell` key names the execve target VERBATIM (no PATH search),
    /// so `--validate-config` must catch the three shapes that make every new
    /// session die at exec — a bare name, a nonexistent path, and (the healthy
    /// control) an absolute existing shell warns nothing.
    #[test]
    fn validate_flags_a_shell_the_spawn_cannot_exec() {
        let shell_warns = |toml: &str| -> Vec<String> {
            validate_config_text(toml)
                .expect("parses")
                .into_iter()
                .filter(|w| w.starts_with("shell:"))
                .collect()
        };
        let bare = shell_warns("shell = \"zsh\"");
        assert!(
            bare.iter().any(|w| w.contains("bare name")),
            "a bare name must warn (execve does no PATH search): {bare:?}"
        );
        let missing = shell_warns("shell = \"/nonexistent/definitely-not-a-shell\"");
        assert!(
            missing.iter().any(|w| w.contains("does not exist")),
            "a nonexistent path must warn: {missing:?}"
        );
        assert!(
            shell_warns("shell = \"/bin/sh\"").is_empty(),
            "an absolute existing shell is clean"
        );
    }

    #[test]
    fn cli_validation_rejects_the_same_oversized_generation_as_runtime_and_manual() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-validation-limit-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(crate::native_config_service::MAX_CONFIG_FILE_BYTES as u64 + 1)
            .unwrap();

        let (message, valid) = validate_config_path(&path);
        assert!(!valid);
        assert!(message.contains("exceeds"), "{message}");
        assert!(
            message.contains(&crate::native_config_service::MAX_CONFIG_FILE_BYTES.to_string()),
            "{message}"
        );
        let direct = validate_config_text(
            &" ".repeat(crate::native_config_service::MAX_CONFIG_FILE_BYTES + 1),
        )
        .unwrap_err();
        assert!(direct.contains("admission limit"), "{direct}");
        let _ = std::fs::remove_dir_all(dir);
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
    /// The shadow half follows `HARDCODED_SUPER_CHORDS`: where the Cmd/Super suite
    /// is compiled OFF (Linux), a cmd/super chord conflicts with NOTHING, so the
    /// conflict warnings must NOT fire — only the cross-table collision remains.
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
        let suite_live = crate::app_input::HARDCODED_SUPER_CHORDS;
        assert_eq!(
            joined.contains("conflicts with built-in Copy"),
            suite_live,
            "{joined}"
        );
        assert_eq!(
            joined.contains("conflicts with built-in New Tab"),
            suite_live,
            "{joined}"
        );
        assert!(joined.contains("also bound in [keybindings]"), "{joined}");
    }

    /// The documented unbind spelling (`"none"` / `"unbind"`) is a VALID
    /// [keybindings] value the loader accepts silently — the validator (and the
    /// Settings editor's live diagnostics, which share `config_semantic_warnings`)
    /// must not flag it as an unknown action.
    #[test]
    fn validate_accepts_the_unbind_spellings() {
        let cfg = r#"
[keybindings]
"f11" = "none"
"ctrl+tab" = "unbind"
"#;
        let warnings = validate_config_text(cfg).expect("structurally valid TOML");
        assert!(
            warnings.is_empty(),
            "unbind entries are not unknown actions: {warnings:?}"
        );
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

        let retired = validate_config_text("cursor_style = \"underline\"\n")
            .expect("retired compatibility spelling remains structurally loadable");
        assert_eq!(
            retired,
            [
                "cursor_style = \"underline\" is retired and renders as \"bar\"; use \"bar\" explicitly"
            ]
        );
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

    /// An unknown `trail_sound_style` silently plays `auto` at runtime, so
    /// `--validate-config` must flag it — while every picker option AND every
    /// documented alias (water/mech/bell/…) validates green (the macOS-only
    /// platform note aside), since those genuinely select a voice.
    #[test]
    fn validate_flags_unknown_trail_sound_style() {
        let joined = validate_config_text("trail_sound_style = \"glas bel\"\n")
            .expect("structurally valid TOML")
            .join("\n");
        assert!(
            joined.contains("trail_sound_style") && joined.contains("glas bel"),
            "typo'd typing sound must be flagged: {joined}"
        );
        for ok in crate::prefs::TRAIL_SOUND_STYLES
            .iter()
            .copied()
            .chain(["Glass Bell", "water", "mech", "thock", "raindrop", " felt "])
        {
            let clean = validate_config_text(&format!("trail_sound_style = \"{ok}\"\n"))
                .expect("valid");
            assert!(
                clean
                    .iter()
                    .all(|w| !w.contains("is not a typing sound")),
                "{ok:?} must validate green: {clean:?}"
            );
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
            shell_integration_runtime: "active (Zsh loader prepared)".into(),
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
        // The RUNTIME shell-integration line rides beside the advertised
        // capability row — the split between the two is the diagnosable fact.
        assert!(
            r.contains("shell-int: active (Zsh loader prepared)"),
            "runtime shell-integration outcome line"
        );
    }

    /// `collect` probes the REAL preparation, so on a dev machine (a known
    /// shell, a writable cache) it reports active — and never the advertised
    /// constant's bare true/false, which is what this line replaced.
    #[test]
    fn collect_reports_runtime_shell_integration() {
        let d = collect();
        assert!(
            d.shell_integration_runtime.starts_with("active")
                || d.shell_integration_runtime.starts_with("NOT ACTIVE"),
            "the line always states an outcome: {}",
            d.shell_integration_runtime
        );
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
        // The built-in section is PER PLATFORM: macOS documents the hardcoded
        // Cmd-* chords; everywhere else it documents the seeded platform table
        // (printing "cmd+t" on Windows would teach Win+T, which the shell owns).
        #[cfg(target_os = "macos")]
        {
            assert!(out.contains("built-in keybindings"), "builtin header");
            // A couple of the fixed Cmd-* chords are documented.
            assert!(out.contains("cmd+c"), "copy chord listed");
            assert!(out.contains("cmd+t"), "new-tab chord listed");
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert!(out.contains("built-in default keybindings"), "builtin header");
            // The section is GENERATED from the seed table, so every seeded
            // chord and action is documented — drift is structurally impossible,
            // and this asserts the generation stays wired.
            for (chord, action) in crate::keybinding::Keybindings::PLATFORM_DEFAULT_PAIRS {
                assert!(out.contains(chord), "{chord} listed");
                assert!(out.contains(action), "{action} listed");
            }
        }
        // Every bindable action NAME is offered for [keybindings] values.
        assert!(out.contains("bindable action names"), "actions header");
        assert!(
            out.contains("[key_sequences]"),
            "key_sequences section present"
        );
        for name in crate::keybinding::ACTION_NAMES {
            assert!(out.contains(name), "{name} must be listed");
        }
        // The unbind spelling is part of the value surface: it lives in the
        // action-names footer (it cannot join ACTION_NAMES — those must parse).
        assert!(
            out.contains("none | unbind"),
            "unbind spelling documented in the footer: {out}"
        );
    }

    /// The per-row [keybindings] note: an unbind entry says what it DOES
    /// ("f11 none (UNKNOWN action)" — the old print — told the user their
    /// working unbind was broken); a genuinely unknown action still shouts; the
    /// conflict note follows `builtin_shadow_label` (gated off with the Cmd
    /// suite on Linux); an ordinary binding gets no note.
    #[test]
    fn user_keybinding_note_labels_unbinds_and_unknowns() {
        assert_eq!(user_keybinding_note("f11", "none"), "  (unbinds a default)");
        // An unbind over a HARDCODED built-in says both halves — the review's
        // macOS `cmd+c` case, where the unbind masks a seed the built-in still
        // owns. Off macOS the suite is compiled out and there is nothing to add.
        let over_builtin = user_keybinding_note("cmd+c", "none");
        if crate::keybinding::builtin_shadow_label("cmd+c").is_some() {
            assert!(
                over_builtin.contains("unbinds a default")
                    && over_builtin.contains("still conflicts with"),
                "an unbind cannot imply the chord is free: {over_builtin}"
            );
        } else {
            assert_eq!(over_builtin, "  (unbinds a default)");
        }
        assert_eq!(
            user_keybinding_note("ctrl+tab", "unbind"),
            "  (unbinds a default)"
        );
        assert_eq!(
            user_keybinding_note("cmd+k", "no_such_action"),
            "  (UNKNOWN action)"
        );
        assert_eq!(user_keybinding_note("ctrl+shift+t", "new_tab"), "");
        let expected = if crate::app_input::HARDCODED_SUPER_CHORDS {
            "  (conflicts with Copy)".to_string()
        } else {
            String::new()
        };
        assert_eq!(user_keybinding_note("cmd+c", "copy"), expected);

        // THE macOS SHAPE, ON ANY HOST. Everything above is host-conditional —
        // on Linux the Cmd/Super suite is compiled off, so the branch the
        // review's escape lived in is unreachable through the wrapper and the
        // assertions above prove only the Linux half. The gate-explicit core
        // resolves the label for us, so the macOS row is testable here.
        assert_eq!(
            user_keybinding_note_for("none", Some("Copy")),
            "  (unbinds a default; still conflicts with Copy)",
            "an unbind over a live built-in cannot imply the chord is free"
        );
        assert_eq!(
            user_keybinding_note_for("unbind", Some("Next Tab")),
            "  (unbinds a default; still conflicts with Next Tab)",
            "both unbind spellings keep the caveat"
        );
        assert_eq!(
            user_keybinding_note_for("none", None),
            "  (unbinds a default)",
            "with no built-in claiming the chord there is nothing to add"
        );
        // Precedence is unchanged by the caveat: an unknown action still
        // shouts, and a live binding over a built-in keeps the plain note.
        assert_eq!(
            user_keybinding_note_for("no_such_action", Some("Copy")),
            "  (UNKNOWN action)"
        );
        assert_eq!(
            user_keybinding_note_for("copy", Some("Copy")),
            "  (conflicts with Copy)"
        );
    }

    /// The VALIDATOR half of the same review escape, on the macOS shape from
    /// any host: `"cmd+c" = "none"` used to validate fully green because the
    /// unbind arm short-circuited the built-in-conflict caveat. The unbind must
    /// still not be an "unknown action" (that was the older escape, the other
    /// way), so both halves have to hold at once.
    #[test]
    fn an_unbind_over_a_live_builtin_keeps_the_conflict_caveat() {
        let unbind = keybinding_row_warning("cmd+c", "none", Some("Copy"))
            .expect("an unbind cannot swallow the built-in it is unable to mask");
        assert_eq!(unbind.key, "keybindings");
        assert!(
            unbind.message.contains("unbinds a default")
                && unbind.message.contains("conflicts with built-in Copy"),
            "{unbind:?}"
        );
        assert!(
            !unbind.message.contains("unknown action"),
            "the documented unbind spelling is still not an unknown action: {unbind:?}"
        );

        // Where the suite is compiled off (Linux) the chord conflicts with
        // NOTHING, so an unbind stays silently valid — a warning there would
        // tell a user their working binding fights a built-in this build has
        // no such thing as.
        assert!(keybinding_row_warning("cmd+c", "none", None).is_none());
        assert!(keybinding_row_warning("f11", "unbind", None).is_none());

        // The remaining arms keep their precedence with the label resolved.
        assert!(
            keybinding_row_warning("cmd+c", "no_such_action", Some("Copy"))
                .expect("unknown action")
                .message
                .contains("unknown action \"no_such_action\"")
        );
        assert!(
            keybinding_row_warning("cmd+c", "find", Some("Copy"))
                .expect("plain shadow")
                .message
                .contains("conflicts with built-in Copy")
        );
        assert!(
            keybinding_row_warning("shift+entr", "none", Some("Copy"))
                .expect("an invalid chord outranks every later arm")
                .message
                .contains("invalid")
        );
    }

    /// The Query arm ANSWERS (it stopped being "the GUI drops every Query"
    /// when the arm was wired), and what it can reach is platform-split — so
    /// the caveat must not carry Linux's own-selection bound onto a host whose
    /// read is the whole system pasteboard. Asserted through the SHIPPING
    /// pass, which is where the wording actually reaches a user.
    #[test]
    fn the_osc52_query_caveat_names_this_platforms_real_read_reach() {
        let config = crate::app_config::Config {
            allow_osc52_query: Some(true),
            ..Default::default()
        };
        let message = config_semantic_warnings(&config)
            .into_iter()
            .find(|w| w.key == "allow_osc52_query")
            .expect("the caveat rides an enabled key")
            .message;
        assert!(
            !message.contains("drops every") && !message.contains("remain unavailable"),
            "the Query arm answers; the old claim is retired: {message}"
        );
        assert!(message.contains("READ"), "{message}");
        if cfg!(target_os = "linux") {
            assert!(
                message.contains("selections aterm itself owns"),
                "X11 reads only the slots aterm owns: {message}"
            );
        } else {
            assert!(
                message.contains("OTHER apps"),
                "off X11 the read is the whole pasteboard and must not be softened: {message}"
            );
        }
    }

    #[test]
    fn show_config_reports_resolved_launch_values() {
        let out = show_config();
        assert!(out.contains("resolved launch config"), "header present");
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

    #[test]
    fn show_config_font_size_distinguishes_auto_base_from_explicit_pixels() {
        assert_eq!(
            show_config_font_px(12.0, false),
            "12 (auto base; final physical px depends on display scale)"
        );
        assert_eq!(show_config_font_px(24.0, true), "24 (explicit physical px)");
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
