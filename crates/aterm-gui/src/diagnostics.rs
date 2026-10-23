// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window binary's text reports: `--diagnose`, `--validate-config`, `--show-config`,
//! `--show-face`, `--list-fonts`, `--list-keybinds` and the theme list.

use std::fmt::Write as _;

/// A captured snapshot of what the diagnostics report prints.
pub(crate) struct DiagInfo {
    pub version: String,
    pub git_commit: &'static str,
    pub build_time: &'static str,
    pub target_os: &'static str,
    pub target_arch: &'static str,
    pub profile: &'static str,
    /// Compiler provenance line.
    pub compiler: String,
    /// SHA-256 of the raw 32-byte paper-master public key (the anchor that authorizes a release).
    pub update_pin_sha256: String,
    pub renderer_default: &'static str,
    /// What shell-integration preparation did for the configured shell.
    pub shell_integration_runtime: String,
    /// Registry agent primer state under `$HOME` and whether fresh spawns auto-prime.
    pub agent_primer: String,
    /// Resolved `[privacy]` posture plus full-disk-access state via an injected probe.
    pub privacy: String,
    /// macOS debugger rule (developer mode) via an injected probe.
    pub debugger: String,
    pub features: Vec<(&'static str, bool)>,
    pub capabilities: Vec<(&'static str, bool)>,
    /// Claimed Objective-C protocols ([`CLAIMED_OBJC_PROTOCOLS`]) absent from this host's
    /// frameworks (e.g. `NSApplicationDelegate` on macOS 14.4.1).
    pub objc_protocols_absent: Vec<&'static str>,
    pub config_path: String,
    pub config_presence: ConfigPresence,
    pub env: Vec<(String, String)>,
}

/// What `aterm.toml` is doing for this process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConfigPresence {
    /// No file at the path; built-in defaults are in force.
    Absent,
    /// The file was read and parsed.
    Loaded,
    /// Present on disk but unreadable or invalid; carries `app_config::launch_config_notice`.
    Rejected(String),
}

impl ConfigPresence {
    const fn token(&self) -> &'static str {
        match self {
            Self::Absent => "absent — defaults",
            Self::Loaded => "present",
            Self::Rejected(_) => "present but REJECTED — running defaults",
        }
    }

    const fn rejection_reason(&self) -> Option<&str> {
        match self {
            Self::Rejected(reason) => Some(reason.as_str()),
            _ => None,
        }
    }
}

fn config_line(label: &str, path: &str, presence: &ConfigPresence) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "{label}{path} [{}]", presence.token());
    let Some(reason) = presence.rejection_reason() else {
        return s;
    };
    let indent = " ".repeat(label.chars().count());
    let _ = writeln!(
        s,
        "{indent}every value below is a built-in DEFAULT — nothing came from this file."
    );
    for (i, line) in reason.lines().enumerate() {
        let row = if i == 0 {
            format!("{indent}reason: {line}")
        } else {
            format!("{indent}        {line}")
        };
        let _ = writeln!(s, "{}", row.trim_end());
    }
    s
}

fn report_config_target(launch_failure: Option<&str>) -> (String, ConfigPresence) {
    let Some(path) = crate::app_config::config_path() else {
        return (
            format!("(unresolved: {} unset)", crate::cli::CONFIG_PATH_VARS),
            ConfigPresence::Absent,
        );
    };
    let presence = match launch_failure {
        Some(reason) => ConfigPresence::Rejected(reason.to_string()),
        None if path.exists() => ConfigPresence::Loaded,
        None => ConfigPresence::Absent,
    };
    (path.display().to_string(), presence)
}

const fn checkbox(on: bool) -> char {
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
        let _ = writeln!(s, "primer:    {}", self.agent_primer);
        let _ = writeln!(s, "privacy:   {}", self.privacy);
        let _ = writeln!(s, "debugger:  {}", self.debugger);
        if !cfg!(target_os = "macos") {
            let _ = writeln!(s, "objc:      n/a (not macOS)");
        } else if self.objc_protocols_absent.is_empty() {
            let _ = writeln!(
                s,
                "objc:      every claimed protocol is registered by this host"
            );
        } else {
            let _ = writeln!(
                s,
                "objc:      protocols this host does not register: {} (aterm supplies a \
                 name-only stand-in at declaration)",
                self.objc_protocols_absent.join(", ")
            );
        }
        s.push_str(&config_line(
            "config:    ",
            &self.config_path,
            &self.config_presence,
        ));
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

/// The GPU backend each platform compiles.
#[cfg(target_os = "macos")]
const GPU_BACKEND_LABEL: &str = "gpu (metal)";
/// Linux: the first-party GLES renderer over EGL.
#[cfg(target_os = "linux")]
const GPU_BACKEND_LABEL: &str = "gpu (gl)";
/// Windows restricts wgpu to DX12, the only backend wired for the HDR/scRGB present.
#[cfg(windows)]
const GPU_BACKEND_LABEL: &str = "gpu (dx12)";
/// Everything else: wgpu's Vulkan backend.
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
const GPU_BACKEND_LABEL: &str = "gpu (vulkan)";

const fn renderer_label(gpu: bool) -> &'static str {
    if gpu { GPU_BACKEND_LABEL } else { "cpu" }
}

fn shell_integration_runtime(config: &crate::app_config::Config) -> String {
    use aterm_core::shell_integration as si;
    let hint = crate::app_config::resolve_shell_override(config);
    let shell = crate::spawn::detect_spawn_shell(hint.as_deref());
    match si::prepare(shell) {
        Ok(Some(mut injection)) => {
            let args = config.shell_args.as_deref().unwrap_or_default();
            match si::merge_shell_args(shell, &mut injection, args) {
                si::ArgsMerge::Integrated => format!("active ({shell:?} loader prepared)"),
                si::ArgsMerge::Bypass(why) => format!(
                    "NOT ACTIVE — {why}; aterm runs those shell_args as given, \
                     without integration"
                ),
            }
        }
        Ok(None) => {
            let name = crate::spawn::selected_shell_name(hint.as_deref());
            format!(
                "NOT ACTIVE — no integration for {name}; ALab tools reach new shells \
                 only via shell.d, so add <prefix>/bin to PATH manually"
            )
        }
        Err(e) => format!("NOT ACTIVE — loader cache unwritable: {e}"),
    }
}

fn agent_primer_line(config: &crate::app_config::Config) -> String {
    let home = match aterm_types::dirs::test_root() {
        Some(root) => Some(root.join("home")),
        None => aterm_primer::home_dir(),
    };
    let status = home.map_or_else(
        || "(no HOME — nowhere to look)".to_string(),
        |home| aterm_primer::status_line(&home),
    );
    let auto = if config.agents_auto_prime_or_default() {
        "on"
    } else {
        "off"
    };
    format!("{status} — auto-prime: {auto} (agents_auto_prime)")
}

/// The `privacy:` line.
pub(crate) fn privacy_line(
    config: &crate::app_config::Config,
    probe: Option<aterm_containment::FdaProbe>,
) -> String {
    if !config.privacy_enabled() {
        return "off ([privacy] enabled = false) — every consent field reads unknown".to_string();
    }
    let (fda, probe_label) = match (config.privacy_probe_gate().permits(), probe) {
        (false, _) => (aterm_containment::FdaState::Unknown, "off"),
        (true, None) => (aterm_containment::FdaState::Unknown, "none"),
        (true, Some(probe)) => (probe.state, probe.label.as_str()),
    };
    let folders = config.privacy_warmup_folders();
    let folders = if folders.is_empty() {
        "-".to_string()
    } else {
        folders.join(",")
    };
    let switch = |on: bool| if on { "on" } else { "off" };
    format!(
        "full_disk_access={fda} probe={probe_label} fda_scope={scope} warmup={warmup} \
         folders={folders} hold_ms={hold} probe_interval_ms={interval} protected_roots={roots} \
         notice={notice} report_attribution={attribution}",
        fda = fda.as_str(),
        scope =
            crate::privacy_report::observed_fda_scope(fda, aterm_containment::FdaScope::Unknown,)
                .as_str(),
        warmup = config.privacy_warmup().as_str(),
        hold = config.privacy_warmup_hold_ms(),
        interval = config.privacy_probe_interval_ms(),
        roots = config.privacy_protected_roots().len(),
        notice = switch(config.privacy_notice()),
        attribution = switch(config.privacy_report_attribution()),
    )
}

/// The `debugger:` line.
pub(crate) fn debugger_line(
    config: &crate::app_config::Config,
    probe: Option<aterm_containment::consent::DebuggerProbe>,
    hardening: aterm_containment::consent::SelfHardening,
) -> String {
    use aterm_containment::consent::{DebuggerMember, DebuggerMode, TASKPORT_DEBUG_RIGHT};

    if !config.privacy_enabled() {
        return "off ([privacy] enabled = false) — every debugger field reads unknown".to_string();
    }
    let (mode, member, probe_label) = match (config.privacy_probe_gate().permits(), probe) {
        (false, _) => (DebuggerMode::Unknown, DebuggerMember::Unknown, "off"),
        (true, None) => (DebuggerMode::Unknown, DebuggerMember::Unknown, "none"),
        (true, Some(probe)) => (probe.mode, probe.member, probe.label.as_str()),
    };
    format!(
        "mode={mode} member={member} self_hardened={hardened} right={TASKPORT_DEBUG_RIGHT} \
         probe={probe_label}",
        mode = mode.as_str(),
        member = member.as_str(),
        hardened = hardening.as_str(),
    )
}

/// Collect diagnostics from the live build + environment.
pub(crate) fn collect() -> DiagInfo {
    let config = crate::app_config::load_config();
    let gpu = crate::app_config::resolve_want_gpu(&config);
    let renderer_default = renderer_label(gpu);

    let (config_path, config_presence) =
        report_config_target(crate::app_config::launch_load_failure());

    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("ATERM_"))
        .collect();
    env.sort();

    DiagInfo {
        version: crate::build_info::version_display().to_string(),
        git_commit: crate::build_info::git_commit(),
        build_time: crate::build_info::build_time(),
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
        agent_primer: agent_primer_line(&config),
        privacy: privacy_line(&config, None),
        debugger: debugger_line(&config, None, aterm_containment::consent::self_hardening()),
        features: Vec::new(),
        capabilities: capability_list(),
        objc_protocols_absent: objc_protocols_absent(),
        config_path,
        config_presence,
        env,
    }
}

/// Every Objective-C protocol a `declare_class!` site in the shipped window layer claims.
#[cfg(target_os = "macos")]
pub(crate) const CLAIMED_OBJC_PROTOCOLS: &[&std::ffi::CStr] = &[
    c"NSApplicationDelegate",
    c"NSDraggingDestination",
    c"NSDraggingSource",
    c"NSMenuDelegate",
    c"NSObject",
    c"NSTextFieldDelegate",
    c"NSTextInputClient",
    c"NSToolbarDelegate",
    c"NSWindowDelegate",
];

/// The claimed protocols this host's frameworks do not register.
#[cfg(target_os = "macos")]
fn objc_protocols_absent() -> Vec<&'static str> {
    CLAIMED_OBJC_PROTOCOLS
        .iter()
        .copied()
        .filter(|p| aterm_objc::protocol(p).is_null())
        .map(|p| p.to_str().unwrap_or("?"))
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn objc_protocols_absent() -> Vec<&'static str> {
    Vec::new()
}

/// One deterministic, source-addressable warning shared by `--validate-config` and the native
/// Manual language service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigSemanticWarning {
    pub(crate) key: &'static str,
    pub(crate) message: String,
}

fn keybinding_row_warning(
    chord: &str,
    action: &str,
    shadow: Option<&'static str>,
) -> Option<ConfigSemanticWarning> {
    match crate::keybinding::Chord::parse(chord) {
        Err(e) => {
            return Some(ConfigSemanticWarning {
                key: "keybindings",
                message: format!("keybindings: chord {chord:?} invalid ({e})"),
            });
        }
        Ok(parsed) => {
            if let Some(glyph) = crate::keybinding::shift_composed_key(&parsed) {
                let base = crate::keybinding::unshifted_us_spelling(glyph);
                let advice = base.map_or_else(
                    || " — write the UNSHIFTED key instead".to_string(),
                    |b| format!(" — write it with the unshifted key, e.g. {b:?} plus shift"),
                );
                return Some(ConfigSemanticWarning {
                    key: "keybindings",
                    message: format!(
                        "keybindings: chord {chord:?} names {glyph:?}, which a US/UK \
                         layout only produces WITH shift; chords match the unshifted \
                         key, so this row will not fire on such a layout{advice}"
                    ),
                });
            }
        }
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

fn repoint_warnings(
    config: &crate::app_config::Config,
    update_dev: bool,
    packages_dev: bool,
) -> Vec<ConfigSemanticWarning> {
    let mut warnings = Vec::new();
    let mut check = |key: &'static str, value: Option<&str>, dev: bool, fallback: &str| {
        let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
            return;
        };
        let message = if !dev {
            format!("{key}: a development setting this build does not read; remove it")
        } else if !aterm_update_core::is_valid_slug(value.trim()) {
            format!("{key}: {value:?} is not a GitHub name; {fallback}")
        } else {
            return;
        };
        warnings.push(ConfigSemanticWarning { key, message });
    };
    if let Some(update) = config.update.as_ref() {
        check(
            "update.owner",
            update.owner.as_deref(),
            update_dev,
            "the default owner is used",
        );
        check(
            "update.repo",
            update.repo.as_deref(),
            update_dev,
            "the default repo is used",
        );
    }
    if let Some(packages) = config.packages.as_ref() {
        let account = packages.account.as_deref();
        check(
            "packages.account",
            account,
            packages_dev,
            "the default index is used",
        );
    }
    warnings
}

/// Semantic checks that need no filesystem.
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
                "font_px: {font_px:?} is not accepted (expected {}–{}; the value is ignored)",
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

    warnings.extend(repoint_warnings(
        config,
        aterm_update_core::REPOINT_IS_A_DEV_SEAM,
        atpkg::config::DEV_SEAMS,
    ));
    if let Some(privacy) = config.privacy.as_ref() {
        if privacy.auto_accept == Some(true) {
            warnings.push(ConfigSemanticWarning {
                key: "privacy.auto_accept",
                message: "privacy.auto_accept is reserved and not implemented: aterm answers no \
                          macOS consent or password dialogs; grant Full Disk Access or configure \
                          Debugger access in Settings \u{25b8} Security instead"
                    .to_string(),
            });
        }
        if privacy.enabled == Some(false) {
            if privacy.notice == Some(true) {
                warnings.push(ConfigSemanticWarning {
                    key: "privacy.notice",
                    message: "privacy.notice = true has no effect while privacy.enabled = false; \
                              the consent notice can never fire"
                        .to_string(),
                });
            }
            if let Some(warmup) = privacy
                .warmup
                .as_deref()
                .and_then(crate::app_config::PrivacyWarmup::parse)
                .filter(|warmup| warmup.offered())
            {
                warnings.push(ConfigSemanticWarning {
                    key: "privacy.warmup",
                    message: format!(
                        "privacy.warmup = \"{}\" has no effect while privacy.enabled = false; \
                         the folder-access gesture is never offered and nothing is asked at \
                         launch",
                        warmup.as_str()
                    ),
                });
            }
            if privacy.observer == Some(true) {
                warnings.push(ConfigSemanticWarning {
                    key: "privacy.observer",
                    message: "privacy.observer = true has no effect while privacy.enabled = \
                              false; the tccd log is never read"
                        .to_string(),
                });
            }
        }
        if let Some(warmup) = privacy.warmup.as_deref()
            && crate::app_config::PrivacyWarmup::parse(warmup).is_none()
        {
            let accepted = crate::app_config::PrivacyWarmup::ALL
                .iter()
                .map(|mode| format!("{:?}", mode.as_str()))
                .collect::<Vec<_>>()
                .join(", ");
            warnings.push(ConfigSemanticWarning {
                key: "privacy.warmup",
                message: format!(
                    "privacy.warmup value {warmup:?} is not one of {accepted}; {:?} is used",
                    crate::app_config::PrivacyWarmup::default().as_str()
                ),
            });
        }
        if let Some(folders) = privacy.warmup_folders.as_ref() {
            let known = aterm_containment::Folder::ALL
                .iter()
                .map(|folder| folder.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            for (index, name) in folders.iter().enumerate() {
                if aterm_containment::Folder::parse(name).is_none() {
                    warnings.push(ConfigSemanticWarning {
                        key: "privacy.warmup_folders",
                        message: format!(
                            "privacy.warmup_folders[{index}]: unknown folder {name:?} is skipped \
                             by the warm-up; known names are {known}"
                        ),
                    });
                }
            }
        }
        if let Some(roots) = privacy.protected_roots.as_ref() {
            for (index, entry) in roots.iter().enumerate() {
                let trimmed = entry.trim();
                let usable = trimmed == "~"
                    || trimmed.starts_with("~/")
                    || std::path::Path::new(trimmed).is_absolute();
                if !usable {
                    warnings.push(ConfigSemanticWarning {
                        key: "privacy.protected_roots",
                        message: format!(
                            "privacy.protected_roots[{index}]: {entry:?} is neither an absolute \
                             path nor ~-prefixed and is ignored"
                        ),
                    });
                }
            }
        }
        if let Some(hold_ms) = privacy.warmup_hold_ms
            && hold_ms > crate::app_config::PRIVACY_WARMUP_HOLD_MS_MAX
        {
            warnings.push(ConfigSemanticWarning {
                key: "privacy.warmup_hold_ms",
                message: format!(
                    "privacy.warmup_hold_ms is configured as {hold_ms} ms but is effectively {} \
                     ms (clamped to maximum)",
                    crate::app_config::PRIVACY_WARMUP_HOLD_MS_MAX
                ),
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
    for issue in config.color_issues() {
        if issue.key.starts_with("palette[") {
            warnings.push(ConfigSemanticWarning {
                key: "palette",
                message: format!("{issue} (ignored at load)"),
            });
        }
    }
    if let Some(palette) = config
        .sparkle_words
        .as_ref()
        .and_then(|sparkle| sparkle.profanity.as_ref())
        .and_then(|profanity| profanity.palette.as_ref())
    {
        for (index, color) in palette.iter().enumerate() {
            if aterm_types::scheme::parse_hex(color).is_none() {
                warnings.push(ConfigSemanticWarning {
                    key: "sparkle_words.profanity.palette",
                    message: format!(
                        "sparkle_words.profanity.palette[{index}]: expected RRGGBB, #RRGGBB or #RGB, got {color:?}; this color is ignored"
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
            || (hue.starts_with('#') && aterm_types::scheme::parse_hex(hue).is_some());
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
        let message = if cfg!(target_os = "linux") {
            "allow_osc52_query is enabled: programs can READ clipboard selections owned by \
             aterm (foreign X11 copies are not read). Leave off unless a tool needs it"
        } else if cfg!(target_os = "macos") {
            "allow_osc52_query is enabled: programs can READ the system clipboard (including \
             copies from other apps) and trigger macOS's \"aterm would like to paste from …\" \
             alert. Leave off unless a tool needs it (e.g. remote vim/tmux clipboard sync)"
        } else {
            "allow_osc52_query is enabled: programs can READ the system clipboard (including \
             copies from other apps). Leave off unless a tool needs it (e.g. remote vim/tmux \
             clipboard sync)"
        };
        warnings.push(ConfigSemanticWarning {
            key: "allow_osc52_query",
            message: message.to_string(),
        });
    }
    if config.allow_window_ops == Some(true) {
        let message = if cfg!(target_os = "linux") {
            "allow_window_ops is enabled: window ops (iconify, maximize, fullscreen, resize; move denied) and title/grid/pixel/cell-size reports are active; position and screen-size reports stay unanswered"
        } else {
            "allow_window_ops answers title, text-grid-size, text-area-pixels and cell-size reports only; window ops and position/screen-size reports are ignored on this platform"
        };
        warnings.push(ConfigSemanticWarning {
            key: "allow_window_ops",
            message: message.to_string(),
        });
    }
    warnings
}

/// Runtime validation that owns ambient filesystem access.
pub(crate) fn config_host_semantic_warnings(
    config: &crate::app_config::Config,
) -> Vec<ConfigSemanticWarning> {
    config_host_semantic_warnings_with_backend(config, crate::app_config::resolve_want_gpu(config))
}

/// Host validation with the caller's renderer backend.
pub(crate) fn config_host_semantic_warnings_with_backend(
    config: &crate::app_config::Config,
    backend_gpu: bool,
) -> Vec<ConfigSemanticWarning> {
    let assets =
        config.resolve_asset_catalog_with_themes(crate::app_config::ThemeCatalog::discover());
    config_host_semantic_warnings_with_backend_and_assets(config, backend_gpu, &assets)
}

/// Host validation against the immutable non-text asset generation admitted with `config`.
pub(crate) fn config_host_semantic_warnings_with_backend_and_assets(
    config: &crate::app_config::Config,
    backend_gpu: bool,
    assets: &crate::app_config::ConfigAssetCatalog,
) -> Vec<ConfigSemanticWarning> {
    let mut warnings = Vec::new();

    for key in ["columns", "lines", "font_px", "font_family", "gpu", "shell"] {
        if config_key_is_authored(config, key)
            && let Some(active) = crate::app_config::active_environment_override(key)
        {
            warnings.push(ConfigSemanticWarning {
                key,
                message: format!(
                    "{key}: {} overrides the saved value; effective value is {}",
                    active.variable, active.effective
                ),
            });
        }
    }

    if let Some(shell) = config
        .shell
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        && let Some(message) = shell_config_warning(shell)
    {
        warnings.push(ConfigSemanticWarning {
            key: "shell",
            message,
        });
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

    warnings.extend(
        crate::app_config::theme_catalog_misses(config, &assets.themes)
            .into_iter()
            .map(|message| ConfigSemanticWarning {
                key: "theme",
                message,
            }),
    );
    warnings.extend(
        config
            .split_override_warnings(&assets.themes)
            .into_iter()
            .map(|(key, message)| ConfigSemanticWarning { key, message }),
    );

    if let Some(message) = config.cursor_trail_style_warning() {
        warnings.push(ConfigSemanticWarning {
            key: "cursor_trail_style",
            message,
        });
    }
    if let Some(message) = config.spare_shell_warning() {
        warnings.push(ConfigSemanticWarning {
            key: "spare_shell",
            message,
        });
    }
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
    let package_root_available = !atpkg::PINNED_PKG_ROOTKEY.is_empty();
    warnings.extend(package_capability_warnings(config, package_root_available));
    warnings.extend(config_backend_capability_warnings(
        config,
        backend_gpu,
        ConfigCapabilityPlatform::CURRENT,
    ));
    warnings
}

/// Why the authored `shell` value cannot spawn, or `None` when it can.
#[cfg(not(windows))]
fn shell_config_warning(shell: &str) -> Option<String> {
    if !shell.contains('/') {
        return Some(format!(
            "shell: {shell:?} is a bare name, and the spawn execs it verbatim \
             (no PATH search) — new sessions will fail. Use an absolute path \
             (e.g. /bin/zsh)"
        ));
    }
    match std::fs::metadata(shell) {
        Err(_) => Some(format!(
            "shell: {shell:?} does not exist — new sessions will fail to spawn"
        )),
        Ok(md) => {
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt as _;
                md.is_file() && md.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = md.is_file();
            (!executable).then(|| {
                format!(
                    "shell: {shell:?} is not an executable file — new sessions \
                     will fail to spawn"
                )
            })
        }
    }
}

/// Why the authored `shell` value cannot spawn, or `None` when it can.
#[cfg(windows)]
fn shell_config_warning(shell: &str) -> Option<String> {
    // A quoted value reaches `lpApplicationName` as a filename containing `"` whatever is
    // inside, so it is checked first.
    if shell.contains('"') {
        return Some(format!(
            "shell: {shell:?} is quoted, and `shell` names one program rather than a \
             command line — new sessions will fail. Drop the quotes and put any \
             arguments in `shell_args`"
        ));
    }
    let verdict = match aterm_pty::classify_shell_name(std::ffi::OsStr::new(shell)) {
        aterm_pty::ShellResolution::Resolved(_) => None,
        aterm_pty::ShellResolution::Unresolved => Some(format!(
            "shell: {shell:?} is not on %PATH% and is not a known shell alias — new \
             sessions will fail. Use a name that resolves (e.g. pwsh, cmd, bash) or a \
             full path (e.g. C:\\Windows\\System32\\cmd.exe)"
        )),
        aterm_pty::ShellResolution::Verbatim(path) => windows_shell_path_warning(shell, &path),
    };
    verdict.map(|message| {
        if let Some(var) = unexpanded_windows_env_var(shell) {
            format!(
                "shell: {shell:?} contains %{var}%, and `shell` is never environment-expanded \
                 — new sessions will fail. Write the value the variable holds \
                 (e.g. C:\\Windows\\System32\\cmd.exe)"
            )
        } else if let Some(program) = windows_shell_command_line_head(shell) {
            format!(
                "shell: {shell:?} carries arguments, and `shell` names one program rather \
                 than a command line — new sessions will fail. Write shell = {program:?} \
                 and put the arguments in `shell_args`"
            )
        } else {
            message
        }
    })
}

/// The program at the head of `value` when `value` is a command line (arguments in `shell`).
#[cfg(windows)]
fn windows_shell_command_line_head(value: &str) -> Option<&str> {
    let (head, args) = value.split_once(char::is_whitespace)?;
    if head.is_empty() || args.trim().is_empty() {
        return None;
    }
    match aterm_pty::classify_shell_name(std::ffi::OsStr::new(head)) {
        aterm_pty::ShellResolution::Resolved(_) => Some(head),
        aterm_pty::ShellResolution::Unresolved => None,
        aterm_pty::ShellResolution::Verbatim(path) => {
            let path = std::path::Path::new(&path);
            (path.is_absolute() && path.is_file()).then_some(head)
        }
    }
}

/// The name inside the first `%VAR%` pair in `value`.
#[cfg(windows)]
fn unexpanded_windows_env_var(value: &str) -> Option<&str> {
    let mut rest = value;
    while let Some(open) = rest.find('%') {
        let after = &rest[open + 1..];
        let close = after.find('%')?;
        let name = &after[..close];
        if !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '(' | ')'))
        {
            return Some(name);
        }
        rest = &after[close..];
    }
    None
}

/// Why an explicit path (`ShellResolution::Verbatim`) cannot be spawned.
#[cfg(windows)]
fn windows_shell_path_warning(shell: &str, path: &std::ffi::OsStr) -> Option<String> {
    let path = std::path::Path::new(path);
    if !path.is_absolute() {
        return Some(format!(
            "shell: {shell:?} is a relative path, so each session resolves it against \
             ITS OWN working directory and drive — use a full path \
             (e.g. C:\\Windows\\System32\\cmd.exe)"
        ));
    }
    match std::fs::metadata(path) {
        Err(_) => {
            let suggestion = (path.extension().is_none())
                .then(|| {
                    ["exe", "cmd", "bat", "com"].into_iter().find_map(|ext| {
                        let candidate = path.with_extension(ext);
                        candidate.is_file().then(|| candidate.display().to_string())
                    })
                })
                .flatten();
            Some(match suggestion {
                Some(hit) => format!(
                    "shell: {shell:?} does not exist — the spawn appends no extension. \
                     Write {hit:?}"
                ),
                None => {
                    format!("shell: {shell:?} does not exist — new sessions will fail to spawn")
                }
            })
        }
        Ok(md) => (!md.is_file()).then(|| {
            format!(
                "shell: {shell:?} is not an executable file — new sessions \
                 will fail to spawn"
            )
        }),
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
    } else if packages.seed_install.is_some() {
        Some("packages.seed_install")
    } else if packages.account.is_some() {
        Some("packages.account")
    } else if packages.exclude.is_some() {
        Some("packages.exclude")
    } else {
        Some("packages")
    }
}

fn package_capability_warnings(
    config: &crate::app_config::Config,
    root_available: bool,
) -> Vec<ConfigSemanticWarning> {
    let Some(key) = first_authored_package_key(config) else {
        return Vec::new();
    };
    let reason =
        (!root_available).then_some("no package verification root is pinned in this build");
    reason.map_or_else(Vec::new, |reason| {
        vec![ConfigSemanticWarning {
            key,
            message: format!(
                "{key}: package install/update operations are inert because {reason}; atpkg fails closed and performs no install or update"
            ),
        }]
    })
}

/// Native compositor families relevant to config capability validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfigCapabilityPlatform {
    MacOs,
    Windows,
    /// Linux: [`Self::Unsupported`] for every desktop capability below but the updater,
    /// notifications and the bell.
    Linux,
    Unsupported,
}

impl ConfigCapabilityPlatform {
    const CURRENT: Self = if cfg!(target_os = "macos") {
        Self::MacOs
    } else if cfg!(windows) {
        Self::Windows
    } else if cfg!(target_os = "linux") {
        Self::Linux
    } else {
        Self::Unsupported
    };

    const fn has_native_updater(self) -> bool {
        matches!(self, Self::MacOs | Self::Linux)
    }
}

/// Deterministic capability half of host validation.
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
            ConfigCapabilityPlatform::Unsupported | ConfigCapabilityPlatform::Linux => Some((
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
    if config.secure_keyboard_entry.is_some() && platform != ConfigCapabilityPlatform::MacOs {
        warnings.push(ConfigSemanticWarning {
            key: crate::prefs::EDIT_SECURE_KEYBOARD_ENTRY,
            message: "secure_keyboard_entry is parsed and preserved but has no protective effect on this platform; Secure Keyboard Entry is a macOS mechanism (Carbon secure input)"
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
            (
                crate::prefs::EDIT_TRAIL_SOUND_STYLE,
                config.trail_sound_style.is_some(),
            ),
            (crate::prefs::EDIT_TONE_MELODY, config.tone_melody.is_some()),
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
                        "{key} is parsed and preserved but has no effect on this platform; cursor-trail audio is implemented only on macOS"
                    ),
                });
            }
        }
    }
    if matches!(
        platform,
        ConfigCapabilityPlatform::Unsupported | ConfigCapabilityPlatform::Linux
    ) {
        if platform == ConfigCapabilityPlatform::Unsupported && config.allow_notifications.is_some()
        {
            warnings.push(ConfigSemanticWarning {
                key: "allow_notifications",
                message: "allow_notifications is parsed and preserved but has no effect on this platform because desktop-notification delivery is implemented only on macOS, Windows and Linux"
                    .to_string(),
            });
        }
        if platform == ConfigCapabilityPlatform::Unsupported && config.bell_sound.is_some() {
            warnings.push(ConfigSemanticWarning {
                key: crate::prefs::EDIT_BELL_SOUND,
                message: "bell_sound is parsed and preserved but has nothing to suppress on this platform because the audible terminal bell is implemented only with the macOS and Windows system alert sound and the X11 bell"
                    .to_string(),
            });
        }
    }
    if !platform.has_native_updater()
        && let Some(update) = config.update.as_ref()
    {
        for (key, authored) in [
            ("update.enabled", update.enabled.is_some()),
            ("update.owner", update.owner.is_some()),
            ("update.repo", update.repo.is_some()),
            ("update.auto_apply", update.auto_apply.is_some()),
        ] {
            if authored {
                warnings.push(ConfigSemanticWarning {
                    key,
                    message: format!(
                        "{key} is parsed and preserved but has no effect on this platform because the native updater runs only on macOS and Linux"
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
        _ => false,
    }
}

/// Parse `text` as `aterm.toml`.
pub(crate) fn validate_config_text(text: &str) -> Result<Vec<String>, String> {
    if text.len() > crate::native_config_service::MAX_CONFIG_FILE_BYTES {
        return Err(format!(
            "config exceeds the {}-byte admission limit",
            crate::native_config_service::MAX_CONFIG_FILE_BYTES
        ));
    }
    let config =
        aterm_toml::from_str::<crate::app_config::Config>(text).map_err(|e| e.to_string())?;
    let mut warnings = crate::native_config_language::ignored_key_warnings(text);
    warnings.extend(
        config_semantic_warnings(&config)
            .into_iter()
            .map(|warning| warning.message),
    );
    if let Some(style) = config.cursor_style.as_deref() {
        let normalized = style.trim().to_ascii_lowercase();
        if !matches!(normalized.as_str(), "block" | "bar" | "beam") {
            warnings.push(format!(
                "cursor_style: expected block|bar, got {style:?} (block used at load)"
            ));
        }
    }
    if let Some(note) = config.font_hinting.as_deref().and_then(|hinting| {
        crate::native_config_language::retired_but_resolving_note("font_hinting", hinting)
    }) {
        warnings.push(note);
    }
    warnings.extend(
        config
            .color_issues()
            .into_iter()
            .filter(|issue| !issue.key.starts_with("palette["))
            .map(|issue| format!("{issue} (ignored at load)")),
    );
    for (key, value) in [
        (
            crate::prefs::EDIT_CURSOR_TRAIL_COLOR,
            config.cursor_trail_color.as_deref(),
        ),
        (
            crate::prefs::EDIT_CURSOR_TRAIL_ACCENT,
            config.cursor_trail_accent.as_deref(),
        ),
    ] {
        if let Some(s) = value
            && !crate::prefs::color_value_is_valid(key, s)
        {
            let expected = if key == crate::prefs::EDIT_CURSOR_TRAIL_COLOR {
                "#RRGGBB, #RGB or cursor"
            } else {
                "#RRGGBB or #RGB"
            };
            warnings.push(format!(
                "{key}: expected {expected}, got {s:?} (ignored at load)"
            ));
        }
    }
    warnings.extend(crate::app_config::theme_selector_notice(
        config.theme.as_deref(),
    ));
    warnings.extend(
        config_host_semantic_warnings(&config)
            .into_iter()
            .map(|warning| warning.message),
    );
    if let Some(args) = config.shell_args.as_deref().filter(|args| !args.is_empty()) {
        let shell = crate::spawn::detect_spawn_shell(config.shell.as_deref());
        if let Some(why) = aterm_core::shell_integration::shell_args_bypass(shell, args) {
            warnings.push(format!(
                "shell_args: {why}, so this shell runs without shell integration"
            ));
        }
    }
    let unaccepted = crate::native_config_language::unaccepted_value_warnings(text, &warnings);
    warnings.extend(unaccepted);
    Ok(warnings)
}

/// `--validate-config`: parse the config at its canonical path.
pub(crate) fn validate_config() -> (String, bool) {
    match crate::app_config::config_path() {
        None => (
            format!(
                "no config path ({} unset); built-in defaults in use",
                crate::cli::CONFIG_PATH_VARS
            ),
            true,
        ),
        Some(p) => validate_config_path(&p),
    }
}

fn validate_config_path(path: &std::path::Path) -> (String, bool) {
    match crate::native_config_service::VersionedConfigService::observe_path(path, true) {
        Err(error) => (format!("config not read: {error}"), false),
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
                    "config {} loads, with {} warning{}:\n  {}",
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

/// `--list-fonts`: the font search directories the resolver scans, then every discoverable
/// family stem.
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

/// How [`theme_list_report`] prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeListFormat {
    /// Aligned columns, a reason or warning on an indented line under its row.
    Text,
    /// Tab-separated values with a fixed column count.
    Tsv,
}

/// The `--format tsv` columns, in order.
pub(crate) const THEME_LIST_TSV_COLUMNS: [&str; 11] = [
    "id",
    "label",
    "appearance",
    "appearance_source",
    "pair",
    "source",
    "status",
    "notes",
    "description",
    "typography",
    "skipped",
];

struct ThemeListRow {
    /// What `theme = "<id>"` takes (a file's stem, a built-in's name).
    id: String,
    /// What a person sees: a file's `name =`, else the id.
    label: String,
    /// `None` for a file that did not load (nothing was measured).
    appearance: Option<aterm_types::Appearance>,
    measured: bool,
    pair: Option<String>,
    builtin: bool,
    /// `ready`, `invalid`, `shadowed` or `unreadable`.
    status: &'static str,
    /// The reason a row did not load, or a loaded row's warnings.
    notes: Vec<String>,
    description: Option<String>,
    /// The type settings the theme names, summarised; `None` for none.
    typography: Option<String>,
    /// How many keys a newer-schema file set that this aterm skipped.
    skipped: usize,
}

impl ThemeListRow {
    fn of_meta(
        id: &str,
        meta: aterm_types::scheme::ThemeMeta,
        builtin: bool,
        typography: Option<String>,
    ) -> Self {
        let skipped_note = crate::app_config::skipped_keys_reason(&meta);
        let skipped = meta
            .warnings
            .iter()
            .find_map(|warning| match warning {
                aterm_types::scheme::ThemeWarning::SkippedNewerKeys { count, .. } => {
                    Some(usize::from(*count))
                }
                _ => None,
            })
            .unwrap_or(0);
        let notes = meta
            .warnings
            .iter()
            .filter(|warning| {
                !matches!(
                    warning,
                    aterm_types::scheme::ThemeWarning::SkippedNewerKeys { .. }
                )
            })
            .map(ToString::to_string)
            .chain(skipped_note)
            .collect();
        Self {
            id: id.to_string(),
            label: meta.display_name,
            appearance: Some(meta.appearance),
            measured: meta.appearance_source == aterm_types::scheme::AppearanceSource::Measured,
            pair: meta.pair,
            builtin,
            status: "ready",
            notes,
            description: meta.description,
            typography,
            skipped,
        }
    }
}

fn theme_list_rows(
    catalog: &crate::app_config::ThemeCatalog,
    appearance: Option<aterm_types::Appearance>,
) -> Vec<ThemeListRow> {
    let builtins = aterm_types::scheme::builtin_theme_list()
        .into_iter()
        .map(|theme| {
            ThemeListRow::of_meta(
                theme.name,
                aterm_types::scheme::ThemeMeta::of_builtin(theme),
                true,
                None,
            )
        });
    let files = catalog.listing().map(|(name, status)| {
        use crate::app_config::ThemeEntryStatus;
        let meta = match status {
            ThemeEntryStatus::Ready => catalog.meta(name),
            _ => None,
        };
        if let Some(meta) = meta {
            let typography = catalog
                .resolve_package(name)
                .ok()
                .and_then(|package| package.typography.summary());
            return ThemeListRow::of_meta(name, meta, false, typography);
        }
        let label = match status {
            ThemeEntryStatus::Ready => "ready",
            ThemeEntryStatus::Invalid(_) => "invalid",
            ThemeEntryStatus::Shadowed(_) => "shadowed",
            ThemeEntryStatus::Unreadable => "unreadable",
        };
        ThemeListRow {
            id: name.to_string(),
            label: name.to_string(),
            appearance: None,
            measured: false,
            pair: None,
            builtin: false,
            status: label,
            notes: status.reason().into_iter().collect(),
            description: None,
            typography: None,
            skipped: 0,
        }
    });
    builtins
        .chain(files)
        .filter(|row| appearance.is_none_or(|want| row.appearance == Some(want)))
        .collect()
}

/// The `theme list` control verb's rows over the live `catalog`.
pub(crate) fn theme_list_wire_rows(
    catalog: &crate::app_config::ThemeCatalog,
    dir: Option<&std::path::Path>,
    drawn: &str,
) -> Vec<String> {
    let opt = |value: Option<&str>| value.map_or_else(|| "-".to_string(), |v| format!("{v:?}"));
    let mut rows: Vec<String> = theme_list_rows(catalog, None)
        .into_iter()
        .map(|row| {
            format!(
                "theme id={:?} label={:?} appearance={} measured={} pair={} source={} \
                 status={} drawn={} notes={} description={} type={} skipped={}",
                row.id,
                row.label,
                row.appearance.map_or("-", appearance_word),
                u8::from(row.measured),
                opt(row.pair.as_deref()),
                if row.builtin { "builtin" } else { "file" },
                row.status,
                u8::from(row.status == "ready" && row.id.eq_ignore_ascii_case(drawn)),
                opt((!row.notes.is_empty())
                    .then(|| row.notes.join("; "))
                    .as_deref()),
                opt(row.description.as_deref()),
                opt(row.typography.as_deref()),
                row.skipped,
            )
        })
        .collect();
    let limits = catalog.limits();
    rows.push(format!(
        "folder path={} kept={} seen={} truncated={}",
        opt(dir.map(|d| d.display().to_string()).as_deref()),
        limits.kept,
        limits.seen,
        u8::from(catalog.truncated()),
    ));
    rows
}

fn listing_field(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

/// `aterm theme list` and `--list-themes`.
#[must_use]
pub fn theme_list_report(
    appearance: Option<aterm_types::Appearance>,
    format: ThemeListFormat,
) -> String {
    let dir = aterm_types::scheme::user_theme_dir();
    let catalog = dir
        .as_deref()
        .map(crate::app_config::ThemeCatalog::discover_in)
        .unwrap_or_default();
    theme_list(&catalog, dir.as_deref(), appearance, format)
}

/// The theme `id` names (case-insensitive).
/// # Errors
/// The reason no theme by that id can be had.
pub fn theme_catalog_entry(id: &str) -> Result<aterm_types::scheme::CatalogTheme, String> {
    // A built-in needs no look at the themes folder (and always wins there).
    if let Some(builtin) = aterm_types::scheme::CatalogTheme::builtin(id) {
        return Ok(builtin);
    }
    let catalog = aterm_types::scheme::user_theme_dir()
        .as_deref()
        .map(crate::app_config::ThemeCatalog::discover_in)
        .unwrap_or_default();
    catalog_theme(&catalog, id)
}

fn catalog_theme(
    catalog: &crate::app_config::ThemeCatalog,
    id: &str,
) -> Result<aterm_types::scheme::CatalogTheme, String> {
    let package = catalog.resolve_package(id)?;
    let (name, meta) = catalog
        .ready_themes()
        .find(|(name, _)| name.eq_ignore_ascii_case(id))
        .ok_or_else(|| "no built-in or loaded custom theme by that name".to_string())?;
    let mut theme = aterm_types::scheme::ParsedTheme::colours_only(
        package.colors.clone(),
        meta.appearance_source == aterm_types::scheme::AppearanceSource::Declared,
        meta.description.clone(),
        meta.author.clone(),
    );
    theme.typography = package.typography.clone();
    theme.bold_is_bright = package.bold_is_bright;
    Ok(aterm_types::scheme::CatalogTheme {
        id: name.to_string(),
        theme,
        meta: meta.clone(),
    })
}

/// Whether the font family a theme names is installed here.
#[must_use]
pub fn theme_font_available(family: &str) -> bool {
    let name = family.trim();
    let lower = name.to_ascii_lowercase();
    if let Some(ids) = lower.strip_prefix(aterm_render::DISPLAY_FACE_SCHEME) {
        return !ids.is_empty()
            && ids
                .split('+')
                .all(|id| aterm_render::display_face_canonical_id(id).is_some());
    }
    aterm_render::font_catalog::resolve_and_admit(&[(
        name.to_string(),
        aterm_render::FaceTarget::REGULAR,
    )])
    .entries
    .first()
    .is_some_and(|entry| entry.result.is_ok())
}

/// The appearance a background measures as, by the one dark/light rule the catalog admits a theme
/// file with (so `aterm theme check` measures a file exactly as the themes folder will).
#[must_use]
pub fn theme_measured_appearance(background: aterm_types::Rgb) -> aterm_types::Appearance {
    crate::app_config::measured_appearance(background)
}

fn theme_list(
    catalog: &crate::app_config::ThemeCatalog,
    dir: Option<&std::path::Path>,
    appearance: Option<aterm_types::Appearance>,
    format: ThemeListFormat,
) -> String {
    let rows = theme_list_rows(catalog, appearance);
    let truncated = catalog.truncated().then(|| catalog.limits());
    match format {
        ThemeListFormat::Tsv => theme_list_tsv(&rows, dir, truncated),
        ThemeListFormat::Text => theme_list_text(&rows, dir, truncated),
    }
}

const fn appearance_word(appearance: aterm_types::Appearance) -> &'static str {
    match appearance {
        aterm_types::Appearance::Dark => "dark",
        aterm_types::Appearance::Light => "light",
    }
}

fn theme_list_tsv(
    rows: &[ThemeListRow],
    dir: Option<&std::path::Path>,
    truncated: Option<crate::app_config::ThemeCatalogLimits>,
) -> String {
    let mut s = THEME_LIST_TSV_COLUMNS.join("\t");
    s.push('\n');
    for row in rows {
        let fields: [String; THEME_LIST_TSV_COLUMNS.len()] = [
            listing_field(&row.id),
            listing_field(&row.label),
            row.appearance
                .map(appearance_word)
                .unwrap_or_default()
                .to_string(),
            match (row.appearance, row.measured) {
                (None, _) => "",
                (Some(_), true) => "measured",
                (Some(_), false) => "declared",
            }
            .to_string(),
            listing_field(row.pair.as_deref().unwrap_or_default()),
            if row.builtin { "builtin" } else { "file" }.to_string(),
            row.status.to_string(),
            listing_field(&row.notes.join("; ")),
            listing_field(row.description.as_deref().unwrap_or_default()),
            listing_field(row.typography.as_deref().unwrap_or_default()),
            row.skipped.to_string(),
        ];
        s.push_str(&fields.join("\t"));
        s.push('\n');
    }
    let folder = dir.map(|d| listing_field(&d.display().to_string()));
    let _ = writeln!(s, "#folder\t{}", folder.unwrap_or_default());
    if let Some(limits) = truncated {
        let _ = writeln!(
            s,
            "#truncated\t{}\t{}\t{}\t{}",
            limits.kept,
            limits.seen,
            crate::app_config::MAX_USER_THEME_FILES,
            crate::app_config::MAX_USER_THEME_DIRECTORY_ENTRIES,
        );
    }
    s
}

const THEME_LIST_MAX_COLUMN: usize = 28;

fn theme_list_text(
    rows: &[ThemeListRow],
    dir: Option<&std::path::Path>,
    truncated: Option<crate::app_config::ThemeCatalogLimits>,
) -> String {
    const HEADER: [&str; 8] = [
        "ID",
        "LABEL",
        "APPEARANCE",
        "PAIR",
        "SOURCE",
        "STATUS",
        "TYPE",
        "DESCRIPTION",
    ];
    let cells: Vec<[String; 8]> = rows
        .iter()
        .map(|row| {
            let appearance = match row.appearance {
                None => "-".to_string(),
                Some(a) if row.measured => format!("{} (measured)", appearance_word(a)),
                Some(a) => appearance_word(a).to_string(),
            };
            [
                listing_field(&row.id),
                listing_field(&row.label),
                appearance,
                listing_field(row.pair.as_deref().unwrap_or("-")),
                if row.builtin { "built-in" } else { "file" }.to_string(),
                row.status.to_string(),
                listing_field(row.typography.as_deref().unwrap_or("-")),
                listing_field(row.description.as_deref().unwrap_or_default()),
            ]
        })
        .collect();
    let mut widths = HEADER.map(str::len);
    for cell in &cells {
        for (width, text) in widths.iter_mut().zip(cell.iter()) {
            *width = (*width).max(text.chars().count().min(THEME_LIST_MAX_COLUMN));
        }
    }
    let line = |cells: &[String]| -> String {
        let mut out = String::new();
        let last = cells.len() - 1;
        for (index, (text, width)) in cells.iter().zip(widths).enumerate() {
            if index == last {
                out.push_str(text);
            } else {
                let pad = width.saturating_sub(text.chars().count());
                out.push_str(text);
                out.extend(std::iter::repeat_n(' ', pad + 2));
            }
        }
        out.trim_end().to_string()
    };
    let mut s = String::from(
        "themes (set one with theme = \"<id>\" in aterm.toml; \"dark:<id>,light:<id>\" follows \
         the system):\n",
    );
    let _ = writeln!(s, "{}", line(&HEADER.map(str::to_string)));
    for (row, cell) in rows.iter().zip(&cells) {
        let _ = writeln!(s, "{}", line(cell));
        let lead = if row.status == "ready" {
            "warning"
        } else {
            "not loaded"
        };
        for note in &row.notes {
            let _ = writeln!(s, "    {lead}: {}", listing_field(note));
        }
    }
    match dir {
        Some(dir) => {
            let _ = writeln!(
                s,
                "\nthemes folder: {} (a <id>.conf there is a theme; `aterm theme import <file>` \
                 converts another terminal's)",
                listing_field(&dir.display().to_string())
            );
        }
        None => {
            let _ = writeln!(
                s,
                "\nthemes folder: none (no config directory could be found)"
            );
        }
    }
    if let Some(limits) = truncated {
        let _ = writeln!(
            s,
            "truncated: read {} of the {} .conf files seen; aterm reads at most {} theme files \
             and inspects at most {} folder entries",
            limits.kept,
            limits.seen,
            crate::app_config::MAX_USER_THEME_FILES,
            crate::app_config::MAX_USER_THEME_DIRECTORY_ENTRIES,
        );
    }
    s
}

/// `--list-keybinds`: the built-in default chords, then the user's `[keybindings]` and
/// `[key_sequences]`, then the bindable action names.
pub(crate) fn list_keybinds() -> String {
    let mut s = String::new();
    #[cfg(target_os = "macos")]
    {
        let _ = writeln!(s, "built-in keybindings (in the window):");
        for (chord, label) in
            crate::keybinding::builtin_cmd_chords(crate::keybinding::MENU_BAR_CHORDS)
        {
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
    let _ = writeln!(
        s,
        "  none | unbind      (not an action: unbinds the chord's built-in default)"
    );
    s
}

fn user_keybinding_note(chord: &str, action: &str) -> String {
    user_keybinding_note_for(action, crate::keybinding::builtin_shadow_label(chord))
}

fn user_keybinding_note_for(action: &str, shadow: Option<&'static str>) -> String {
    if crate::keybinding::is_unbind_action(action) {
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

fn show_config_theme(
    config: &crate::app_config::Config,
    themes: &crate::app_config::ThemeCatalog,
) -> String {
    let Some(raw) = config.theme.clone() else {
        return "Default".to_string();
    };
    let sides = [
        aterm_types::Appearance::Dark,
        aterm_types::Appearance::Light,
    ];
    let named = sides.map(|appearance| config.resolve_theme_name(appearance));
    let drawn = named.clone().map(|name| match name {
        Some(name) if name.eq_ignore_ascii_case("default") || themes.resolve(&name).is_ok() => name,
        _ => "Default".to_string(),
    });
    if named
        .iter()
        .zip(&drawn)
        .all(|(named, drawn)| named.as_deref() == Some(drawn.as_str()))
    {
        return raw;
    }
    let [dark, light] = drawn;
    if dark == light {
        format!("{raw} ({dark} in use)")
    } else {
        format!("{raw} (in use: dark {dark}, light {light})")
    }
}

fn show_config_shell(config: &crate::app_config::Config) -> (String, String) {
    let from_flag = crate::cli::launch_flags()
        .shell
        .is_some_and(|flag| !flag.is_empty());
    let chosen = crate::app_config::resolve_shell_override(config);
    let (program, origin) =
        aterm_pty::select_shell_with_origin(chosen.as_deref().map(std::ffi::OsStr::new));
    (
        program.to_string_lossy().into_owned(),
        shell_origin_token(origin, from_flag),
    )
}

/// `--show-config`'s console-host row (Windows).
#[cfg(windows)]
fn show_config_console_host(config: &crate::app_config::Config) -> String {
    aterm_pty::preview_console_host(&aterm_pty::ConsoleHostChoice::from_config(
        config.conpty.as_deref(),
    ))
    .line
}

fn shell_origin_token(origin: aterm_pty::ShellOrigin, from_flag: bool) -> String {
    match origin {
        aterm_pty::ShellOrigin::Override if from_flag => "--shell".to_string(),
        aterm_pty::ShellOrigin::Override => "aterm.toml".to_string(),
        other => format!("default:{}", other.word()),
    }
}

fn show_config_font_px(value: f32, explicit: bool) -> String {
    let source = if explicit { "set" } else { "default" };
    format!("{value} ({source}; px at 1×, each window draws it × its display scale)")
}

/// `--show-config`: the resolved launch config (flag > config > default).
pub(crate) fn show_config() -> String {
    show_config_report(None)
}

fn show_config_report(launch_failure: Option<&str>) -> String {
    let config = crate::app_config::load_config();
    let failure: Option<&str> = match launch_failure {
        Some(reason) => Some(reason),
        None => crate::app_config::launch_load_failure(),
    };
    let (config_path, config_presence) = report_config_target(failure);
    let gpu = crate::app_config::resolve_want_gpu(&config);
    let tab_strip_rows = crate::app_config::resolve_tab_strip_rows(&config);
    let themes = crate::app_config::ThemeCatalog::discover();
    let theme_name = show_config_theme(&config, &themes);
    let [dark, light] = [
        aterm_types::Appearance::Dark,
        aterm_types::Appearance::Light,
    ]
    .map(|appearance| config.resolved_colors(appearance, &themes, None));
    let type_rows = show_config_type_rows(
        &config,
        &themes,
        crate::launch::flags().font_family.as_deref(),
        crate::app_config::Config::font_family_admission,
    );
    let font_px = appearance_labels(
        show_config_font_px(
            type_rows.font_px[0],
            crate::app_config::font_px_is_explicit(&config),
        ),
        show_config_font_px(
            type_rows.font_px[1],
            crate::app_config::font_px_is_explicit(&config),
        ),
    );
    let font_family = type_rows.font_family.clone();
    let columns = crate::app_config::resolve_initial_columns(&config);
    let lines = crate::app_config::resolve_initial_lines(&config);

    let mut s = String::new();
    let _ = writeln!(s, "resolved launch config (flag > config > default)");
    let _ = writeln!(s, "================================================");
    s.push_str(&config_line(
        "config file: ",
        &config_path,
        &config_presence,
    ));
    let _ = writeln!(s);
    let _ = writeln!(s, "font_px:        {font_px}");
    let _ = writeln!(s, "font_family:    {font_family}");
    let _ = writeln!(s, "line_height:    {}", type_rows.line_height);
    let _ = writeln!(s, "font_size_scale: {}", type_rows.font_size_scale);
    let _ = writeln!(s, "renderer:       {}", renderer_label(gpu));
    let _ = writeln!(s, "columns:        {columns}");
    let _ = writeln!(s, "lines:          {lines}");
    let _ = writeln!(s, "tab_strip_rows: {tab_strip_rows}");
    let (shell, shell_origin) = show_config_shell(&config);
    let _ = writeln!(s, "shell:          {shell}");
    let _ = writeln!(s, "shell_origin:   {shell_origin}");
    #[cfg(windows)]
    let _ = writeln!(s, "console_host:   {}", show_config_console_host(&config));
    let _ = writeln!(s, "theme:          {theme_name}");
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
    let _ = writeln!(
        s,
        "foreground:     {}",
        appearance_colours(dark.scheme.foreground, light.scheme.foreground)
    );
    let _ = writeln!(
        s,
        "background:     {}",
        appearance_colours(dark.scheme.background, light.scheme.background)
    );
    let _ = writeln!(
        s,
        "colour source:  {}",
        appearance_labels(color_source_label(&dark), color_source_label(&light))
    );
    s
}

/// `--show-config`'s typography rows.
pub(crate) struct ShowConfigTypeRows {
    pub(crate) font_family: String,
    pub(crate) line_height: String,
    pub(crate) font_size_scale: String,
    /// The derived font size per side (dark, light), the scale applied.
    pub(crate) font_px: [f32; 2],
}

/// Build [`ShowConfigTypeRows`] for `config` over `themes`.
pub(crate) fn show_config_type_rows(
    config: &crate::app_config::Config,
    themes: &crate::app_config::ThemeCatalog,
    flag: Option<&str>,
    admit: impl Fn(Option<&str>) -> Result<Option<aterm_render::FaceRef>, String>,
) -> ShowConfigTypeRows {
    use crate::resolved_typography::TypeOrigin;
    let sides = [
        aterm_types::Appearance::Dark,
        aterm_types::Appearance::Light,
    ]
    .map(|appearance| config.resolved_typography_with_flag(appearance, themes, flag));
    let source = |typo: &crate::resolved_typography::ResolvedTypography, key: &str| {
        let origin = if key == "font_family" && flag.is_some() {
            TypeOrigin::Flag
        } else if key == "font_family" && config.font_family_request().is_some() {
            TypeOrigin::Config
        } else {
            let origin = typo.origin(key, config);
            if origin == TypeOrigin::Flag {
                TypeOrigin::Default
            } else {
                origin
            }
        };
        match origin {
            TypeOrigin::Theme => format!("theme {}", typo.id),
            TypeOrigin::Config => "config".to_string(),
            TypeOrigin::Flag => "--font".to_string(),
            TypeOrigin::Default => "default".to_string(),
        }
    };
    let row = |value: [String; 2], source: [String; 2]| {
        if value[0] == value[1] && source[0] == source[1] {
            format!("{}  (source: {})", value[0], source[0])
        } else {
            format!(
                "{} (dark, {}) / {} (light, {})",
                value[0], source[0], value[1], source[1]
            )
        }
    };
    let views = [sides[0].overlay(config), sides[1].overlay(config)];
    let default_face = || {
        aterm_render::default_primary_identity().map_or_else(
            || "the embedded last-resort face".to_string(),
            |face| face.path,
        )
    };
    let family = |view: &crate::app_config::Config| {
        let font =
            match crate::effective_font_family_with(flag, view.font_family_request().as_deref()) {
                None => format!("(default) {}", default_face()),
                Some(family) if admit(Some(&family)).is_err() => {
                    format!("(default) {}; {family:?} did not load", default_face())
                }
                Some(family) => family,
            };
        let layer = flag
            .and_then(aterm_render::display_face_ids_for_family)
            .map(aterm_render::display_layer_ids)
            .unwrap_or_else(|| view.display_faces_request());
        if layer.is_empty() {
            font
        } else {
            format!("{font} with display faces {}", layer.join("+"))
        }
    };
    let font_family = row(
        [family(&views[0]), family(&views[1])],
        [
            source(&sides[0], "font_family"),
            source(&sides[1], "font_family"),
        ],
    );
    let line_height = row(
        [
            views[0].line_height_or_default().to_string(),
            views[1].line_height_or_default().to_string(),
        ],
        [
            source(&sides[0], "line_height"),
            source(&sides[1], "line_height"),
        ],
    );
    let scale = |view: &crate::app_config::Config| {
        aterm_types::scheme_typography::milli_text(view.font_size_scale_milli_or_default())
    };
    let font_size_scale = row(
        [scale(&views[0]), scale(&views[1])],
        [
            source(&sides[0], "font_size_scale"),
            source(&sides[1], "font_size_scale"),
        ],
    );
    ShowConfigTypeRows {
        font_family,
        line_height,
        font_size_scale,
        font_px: [
            crate::app_config::resolve_scaled_font_px(&views[0]),
            crate::app_config::resolve_scaled_font_px(&views[1]),
        ],
    }
}

fn color_source_label(resolved: &crate::resolved_colors::ResolvedColors) -> String {
    use crate::resolved_colors::ThemeSource;
    let mut label = match &resolved.source {
        ThemeSource::Builtin => format!("{} (built-in)", resolved.id),
        ThemeSource::File(id) => format!("{id} (themes folder)"),
        ThemeSource::Pack(id) => format!("{id} (community pack preview)"),
        ThemeSource::DefaultFallback { reason } => format!("Default (fallback: {reason})"),
    };
    let keys = resolved.config_keys();
    if !keys.is_empty() {
        let _ = write!(label, "; config sets {}", keys.join(", "));
    }
    if !resolved.issues.is_empty() {
        let ignored: Vec<&str> = resolved.issues.iter().map(|i| i.key.as_str()).collect();
        let _ = write!(label, "; ignored {}", ignored.join(", "));
    }
    label
}

fn appearance_labels(dark: String, light: String) -> String {
    if dark == light {
        dark
    } else {
        format!("{dark} (dark) / {light} (light)")
    }
}

fn appearance_colours(dark: aterm_types::Rgb, light: aterm_types::Rgb) -> String {
    let hex = |c: aterm_types::Rgb| format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b);
    if dark == light {
        hex(dark)
    } else {
        format!("{} (dark) / {} (light)", hex(dark), hex(light))
    }
}

/// `--show-face`: the face `family` resolves to (the configured or default font when empty).
pub(crate) fn show_face(family: &str) -> (String, bool) {
    let family = family.trim();
    let mut side_note = "";
    let (resolved_family, header) = if family.is_empty() {
        let config = crate::app_config::load_config();
        let themes = crate::app_config::ThemeCatalog::discover();
        let [dark, light] = [
            aterm_types::Appearance::Dark,
            aterm_types::Appearance::Light,
        ]
        .map(|appearance| {
            let typo = config.resolved_typography(appearance, &themes);
            typo.overlay(&config).font_family_request()
        });
        if dark != light {
            side_note = " (dark side)";
        }
        match crate::effective_font_family(dark.as_deref()) {
            Some(configured) if !configured.trim().is_empty() => {
                (configured.clone(), format!("{configured:?}"))
            }
            _ => match aterm_render::default_primary_identity() {
                Some(default) => (default.path, "the default font".to_string()),
                None => {
                    return (
                        "no font family configured and no default face loads; the \
                         embedded last-resort face draws text\n"
                            .to_string(),
                        false,
                    );
                }
            },
        }
    } else {
        (family.to_string(), format!("{family:?}"))
    };
    match aterm_render::face_info(&resolved_family) {
        None => (
            format!("font family {resolved_family:?} does not resolve to a loadable face\n"),
            false,
        ),
        Some(info) => {
            let mut s = String::new();
            let _ = writeln!(s, "resolved face for {header}{side_note}:");
            let _ = writeln!(s, "  path:        {}", info.path);
            let _ = writeln!(s, "  face index:  {}", info.index);
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
        aterm_toml::from_str(source).expect("test config")
    }

    const ABS_TMP: &str = if cfg!(windows) { "C:/tmp" } else { "/tmp" };

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
        for platform in [
            ConfigCapabilityPlatform::Linux,
            ConfigCapabilityPlatform::Unsupported,
        ] {
            assert_eq!(
                keys(true, platform),
                std::collections::BTreeSet::from([
                    "background_material",
                    "background_opacity",
                    "window_colorspace",
                ]),
                "{platform:?}"
            );
        }

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
            "font_thicken = true\nsecure_keyboard_entry = true\n[update]\nenabled = false\nowner = \"safe-owner\"\nrepo = \"aterm\"\nauto_apply = true\n",
        );
        let platform_keys = |platform| {
            config_backend_capability_warnings(&platform_only, true, platform)
                .into_iter()
                .map(|warning| warning.key)
                .collect::<std::collections::BTreeSet<_>>()
        };
        let non_updater = config_backend_capability_warnings(
            &platform_only,
            true,
            ConfigCapabilityPlatform::Unsupported,
        );
        assert_eq!(
            non_updater
                .iter()
                .map(|warning| warning.key)
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from([
                "font_thicken",
                "secure_keyboard_entry",
                "update.auto_apply",
                "update.enabled",
                "update.owner",
                "update.repo",
            ])
        );
        assert!(
            non_updater
                .iter()
                .filter(|warning| warning.key.starts_with("update."))
                .all(|warning| warning.message.contains("only on macOS and Linux")),
            "{non_updater:?}"
        );
        assert_eq!(
            platform_keys(ConfigCapabilityPlatform::Linux),
            std::collections::BTreeSet::from(["font_thicken", "secure_keyboard_entry"])
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
        assert_eq!(
            keys(ConfigCapabilityPlatform::Linux),
            std::collections::BTreeSet::from(["trail_sound_volume", "trail_sounds"]),
            "Linux delivers notifications but has no trail-audio consumer"
        );
        assert_eq!(
            keys(ConfigCapabilityPlatform::Unsupported),
            std::collections::BTreeSet::from([
                "allow_notifications",
                "trail_sound_volume",
                "trail_sounds",
            ]),
        );

        let bell = parsed("bell_sound = false\n");
        for (platform, inert) in [
            (ConfigCapabilityPlatform::MacOs, false),
            (ConfigCapabilityPlatform::Windows, false),
            (ConfigCapabilityPlatform::Linux, false),
            (ConfigCapabilityPlatform::Unsupported, true),
        ] {
            assert_eq!(
                config_backend_capability_warnings(&bell, true, platform)
                    .iter()
                    .any(|warning| warning.key == crate::prefs::EDIT_BELL_SOUND),
                inert,
                "{platform:?}"
            );
        }
    }

    #[test]
    fn repoint_keys_say_a_shipped_build_does_not_read_them() {
        let config =
            parsed("[update]\nowner = \"me\"\nrepo = \"\"\n[packages]\naccount = \"me/x\"\n");
        let keys = |update_dev, packages_dev| {
            repoint_warnings(&config, update_dev, packages_dev)
                .into_iter()
                .map(|warning| warning.key)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            keys(false, false),
            ["update.owner", "packages.account"],
            "a valid name is still ignored by a shipped build; a blank one is harmless"
        );
        assert_eq!(keys(true, true), ["packages.account"], "not a GitHub name");
    }

    #[test]
    fn pure_semantics_report_runtime_fallbacks_instead_of_false_green() {
        let source = r##"
font_px = 201
font_variation = ["wght=450", "not-an-axis"]
font_features = ["+ss01 toolong", "cv01=2"]

[update]
owner = "bad owner"
repo = "bad/repo"

[packages]
account = "bad/account"

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
            "is not accepted (expected",
            "font_variation[1]",
            "font_features[0]",
            "update.owner",
            "update.repo",
            "packages.account",
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
        let keys: Vec<&str> = enabled.iter().map(|warning| warning.key).collect();
        assert_eq!(keys.len(), 2, "{enabled:?}");
        assert!(keys.contains(&"allow_osc52_query") && keys.contains(&"allow_window_ops"));
        assert!(
            config_semantic_warnings(&parsed(
                "allow_osc52_query = false\nallow_window_ops = false\n"
            ))
            .is_empty(),
            "disabled capabilities are the honest fail-closed default"
        );
    }

    #[test]
    fn custom_records_disclose_every_inert_arm() {
        let source = r#"
[[sparkle_words.custom]]
words = [" "]

[[sparkle_words.custom]]
words = ["quiet"]
burst = { kind = "glow", chance = 0 }

[[sparkle_words.custom]]
words = ["live"]
ink = "rainbow"
"#;
        let warnings = config_semantic_warnings(&parsed(source));
        let joined = warnings
            .iter()
            .map(|warning| warning.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "record 1 has no nonblank word",
            "record 1 is inert",
            "record 2 is inert",
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
    fn theme_derived_matrix_body_discloses_conditional_head_floor() {
        let warnings = config_semantic_warnings(&parsed("[matrix_rain]\nhead_alpha = 80\n"));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].message.contains("theme-derived body alpha"));

        let max = config_semantic_warnings(&parsed("[matrix_rain]\nhead_alpha = 135\n"));
        assert!(max.is_empty(), "the body cannot exceed the cap: {max:?}");
    }

    #[test]
    fn package_admission_warnings_are_pure_and_specific() {
        let packages = parsed("[packages]\nenabled = true\n");
        let rootless = package_capability_warnings(&packages, false);
        assert_eq!(rootless.len(), 1);
        assert!(rootless[0].message.contains("verification root"));
        assert!(
            !rootless[0].message.contains("ATPKG_"),
            "no environment switch is named"
        );
        assert!(package_capability_warnings(&packages, true).is_empty());
    }

    #[test]
    fn privacy_auto_accept_is_refused_and_names_the_grant_that_works() {
        let warnings = config_semantic_warnings(&parsed("[privacy]\nauto_accept = true\n"));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].key, "privacy.auto_accept");
        assert!(config_semantic_warnings(&parsed("[privacy]\nauto_accept = false\n")).is_empty());
    }

    #[test]
    fn privacy_master_switch_off_flags_the_features_it_silences() {
        let warnings = config_semantic_warnings(&parsed(
            "[privacy]\nenabled = false\nnotice = true\nwarmup = \"on-request\"\nobserver = true\n",
        ));
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(
            warnings.iter().any(|w| w.key == "privacy.notice"),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.key == "privacy.warmup"),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.key == "privacy.observer"),
            "{warnings:?}"
        );
        assert!(config_semantic_warnings(&parsed("[privacy]\nenabled = false\n")).is_empty());
        assert!(
            config_semantic_warnings(&parsed(
                "[privacy]\nenabled = true\nnotice = true\nwarmup = \"never\"\nobserver = true\n"
            ))
            .is_empty()
        );
    }

    #[test]
    fn privacy_flags_an_unknown_warmup_spelling_and_folder_name() {
        let warnings = config_semantic_warnings(&parsed(
            "[privacy]\nwarmup = \"first-launch\"\nwarmup_folders = [\"desktop\", \"keychain\"]\n",
        ));
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        let warmup = warnings
            .iter()
            .find(|w| w.key == "privacy.warmup")
            .expect("warmup warning");
        assert!(
            warmup.message.contains("\"first-launch\"")
                && warmup.message.contains("\"never\"")
                && warmup.message.contains("\"on-request\""),
            "the accepted vocabulary is listed: {}",
            warmup.message
        );
        let folder = warnings
            .iter()
            .find(|w| w.key == "privacy.warmup_folders")
            .expect("folder warning");
        assert!(
            folder.message.contains("warmup_folders[1]") && folder.message.contains("\"keychain\""),
            "the index and the name are both named: {}",
            folder.message
        );
    }

    #[test]
    fn privacy_flags_a_protected_root_it_cannot_resolve() {
        let warnings = config_semantic_warnings(&parsed(&format!(
            "[privacy]\nprotected_roots = [\"~/vault\", \"{ABS_TMP}/vault\", \"vault\", \"\"]\n"
        )));
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings.iter().all(|w| w.key == "privacy.protected_roots"),
            "{warnings:?}"
        );
        assert!(warnings[0].message.contains("protected_roots[2]"));
        assert!(
            warnings[1].message.contains("protected_roots[3]"),
            "a blank entry is dropped just as silently: {}",
            warnings[1].message
        );
    }

    #[test]
    fn privacy_flags_an_apply_hold_that_could_pin_a_build() {
        let warnings = config_semantic_warnings(&parsed("[privacy]\nwarmup_hold_ms = 3600000\n"));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].key, "privacy.warmup_hold_ms");
        assert!(
            warnings[0].message.contains("effectively 600000 ms"),
            "{}",
            warnings[0].message
        );
        assert_eq!(
            parsed("[privacy]\nwarmup_hold_ms = 3600000\n").privacy_warmup_hold_ms(),
            crate::app_config::PRIVACY_WARMUP_HOLD_MS_MAX,
            "the warning and the resolver agree on the effective value"
        );
        assert!(
            config_semantic_warnings(&parsed("[privacy]\nwarmup_hold_ms = 600000\n")).is_empty()
        );
    }

    #[test]
    fn a_valid_privacy_section_emits_no_semantic_warning() {
        let clean = parsed(&format!(
            concat!(
                "[privacy]\n",
                "enabled = true\n",
                "check = true\n",
                "notice = true\n",
                "report_attribution = true\n",
                "warmup = \"on-request\"\n",
                "warmup_folders = [\"documents\", \"desktop\", \"downloads\", \"app-data\"]\n",
                "warmup_hold_ms = 120000\n",
                "probe_interval_ms = 5000\n",
                "protected_roots = [\"~/vault\", \"{}/vault\"]\n",
                "auto_accept = false\n",
            ),
            ABS_TMP
        ));
        let warnings = config_semantic_warnings(&clean);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(config_semantic_warnings(&parsed("[privacy]")).is_empty());
        assert!(
            config_semantic_warnings(&crate::app_config::Config::default())
                .iter()
                .all(|w| !w.key.starts_with("privacy"))
        );
    }

    #[test]
    fn host_semantics_accept_valid_lexicon_and_report_missing_or_rejected_layers() {
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
    fn host_validation_reports_only_authored_launch_flag_masking() {
        if !crate::test_alone::alone() {
            return;
        }
        let _ = crate::cli::parse_cli(vec!["--columns".into(), "120".into()]);

        let authored: crate::app_config::Config = aterm_toml::from_str("columns = 80\n").unwrap();
        let warnings = config_host_semantic_warnings(&authored);
        assert!(warnings.iter().any(|warning| {
            warning.key == "columns"
                && warning.message.contains("--columns overrides")
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
        assert!(validate_config_text("font_px = 14.0\ngpu = true\ntheme = \"Dracula\"").is_ok());
        assert!(validate_config_text("").is_ok());
        let err = validate_config_text("font_px = \"big\"").unwrap_err();
        assert!(
            !err.is_empty(),
            "a type mismatch must yield an error message"
        );
        assert!(validate_config_text("font_px = = 1").is_err());
    }

    fn shell_warns(toml: &str) -> Vec<String> {
        validate_config_text(toml)
            .expect("parses")
            .into_iter()
            .filter(|w| w.starts_with("shell:") && !w.contains("overrides the saved value"))
            .collect()
    }

    /// Unix: `shell` names the execve target verbatim (no PATH search).
    #[cfg(not(windows))]
    #[test]
    fn validate_flags_a_shell_the_spawn_cannot_exec() {
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

    /// Windows: the spellings a user writes validate clean.
    #[cfg(windows)]
    #[test]
    fn validate_accepts_the_windows_shell_spellings_a_user_would_write() {
        let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        let cmd_backslash = format!("{sysroot}\\System32\\cmd.exe");
        assert!(
            std::path::Path::new(&cmd_backslash).is_file(),
            "test premise: cmd.exe must exist at {cmd_backslash}"
        );

        let root = std::env::temp_dir().join(format!("aterm-shellcfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let fixture = |dir: &str, file: &str| -> String {
            let d = root.join(dir);
            std::fs::create_dir_all(&d).expect("fixture dir");
            let f = d.join(file);
            std::fs::write(&f, b"placeholder, never executed").expect("fixture file");
            f.display().to_string()
        };
        let spaced = fixture("Program Files\\Git\\bin", "bash.exe");
        assert!(
            spaced.contains(' '),
            "premise: {spaced:?} must hold a space"
        );
        let apostrophe = fixture("O'Brien", "bash.exe");
        assert!(apostrophe.contains('\''), "premise: {apostrophe:?}");
        let percent = fixture("%tools%", "nu.exe");
        assert!(percent.contains("%tools%"), "premise: {percent:?}");

        let mut accepted = vec![
            "cmd".to_string(),
            "cmd.exe".to_string(),
            "CMD.EXE".to_string(),
            "powershell".to_string(),
            cmd_backslash.clone(),
            cmd_backslash.replace('\\', "/"),
            cmd_backslash.to_uppercase(),
            spaced.clone(),
            spaced.replace('\\', "/"),
            apostrophe.clone(),
            percent.clone(),
        ];
        for optional in ["bash", "pwsh", "nu", "wsl"] {
            if matches!(
                aterm_pty::classify_shell_name(std::ffi::OsStr::new(optional)),
                aterm_pty::ShellResolution::Resolved(_)
            ) {
                accepted.push(optional.to_string());
            }
        }
        let authored = |value: &str| -> String {
            if value.contains('\'') {
                format!("shell = \"{}\"", value.replace('\\', "\\\\"))
            } else {
                format!("shell = '{value}'")
            }
        };
        for value in &accepted {
            let warnings = shell_warns(&authored(value));
            assert!(
                warnings.is_empty(),
                "{value:?} is a legitimate Windows shell and must validate clean, got {warnings:?}"
            );
        }

        let missing_path = format!("{sysroot}\\System32\\aterm-no-such-shell-xyz.exe");
        let cmd_with_args = format!("{cmd_backslash} /K dir");
        let mut rejections = Vec::new();
        for (value, needle) in [
            ("aterm-no-such-shell-xyz", "is not on %PATH%"),
            (missing_path.as_str(), "does not exist"),
            (sysroot.as_str(), "not an executable file"),
            ("System32\\cmd.exe", "relative path"),
            ("\\Windows\\System32\\cmd.exe", "relative path"),
            ("C:cmd.exe", "relative path"),
            (r#""C:\Windows\System32\cmd.exe" /K dir"#, "quoted"),
            ("cmd /K dir", "carries arguments"),
            (cmd_with_args.as_str(), "carries arguments"),
            ("%COMSPEC%", "never environment-expanded"),
            ("%USERPROFILE%\\bin\\bash.exe", "never environment-expanded"),
        ] {
            let warnings = shell_warns(&authored(value));
            assert!(
                warnings.iter().any(|w| w.contains(needle)),
                "{value:?} must be rejected with {needle:?}, got {warnings:?}"
            );
            rejections.extend(warnings);
        }

        for (value, program) in [
            ("cmd /K dir", "cmd"),
            (cmd_with_args.as_str(), cmd_backslash.as_str()),
        ] {
            let keep = format!("Write shell = {program:?}");
            let warnings = shell_warns(&authored(value));
            assert!(
                warnings
                    .iter()
                    .any(|w| w.contains(&keep) && w.contains("shell_args")),
                "{value:?} must say {keep:?} and point at shell_args, got {warnings:?}"
            );
        }

        let missing_spaced = format!("{}\\Program Files\\Git\\bin\\nosuch.exe", root.display());
        let spaced_warnings = shell_warns(&authored(&missing_spaced));
        assert!(
            spaced_warnings.iter().any(|w| w.contains("does not exist"))
                && !spaced_warnings
                    .iter()
                    .any(|w| w.contains("carries arguments")),
            "a missing path that merely holds a space is not a command line: {spaced_warnings:?}"
        );
        rejections.extend(spaced_warnings);

        let stem = cmd_backslash.trim_end_matches(".exe").to_string();
        let ext_warnings = shell_warns(&authored(&stem));
        assert!(
            ext_warnings
                .iter()
                .any(|w| w.contains("appends no extension") && w.contains("cmd.exe")),
            "{stem:?} must be rejected by offering the .exe spelling, got {ext_warnings:?}"
        );
        rejections.extend(ext_warnings);

        for warning in &rejections {
            assert!(
                !warning.contains("/bin/") && !warning.contains("/usr/"),
                "Windows advice must never name a POSIX path: {warning:?}"
            );
            assert!(
                !warning.contains("bare name"),
                "a Windows bare name is legal and must not be called out as the defect: {warning:?}"
            );
        }

        assert_eq!(
            super::windows_shell_command_line_head("cmd /K dir"),
            Some("cmd")
        );
        for not_a_command_line in [
            "cmd",
            "cmd ",
            " ",
            spaced.as_str(),
            "aterm-no-such-shell-xyz -l",
            "%COMSPEC% /K dir",
        ] {
            assert_eq!(
                super::windows_shell_command_line_head(not_a_command_line),
                None,
                "{not_a_command_line:?} is one program's name, not a command line"
            );
        }

        assert_eq!(
            super::unexpanded_windows_env_var("%COMSPEC%"),
            Some("COMSPEC")
        );
        assert_eq!(
            super::unexpanded_windows_env_var("%ProgramFiles(x86)%\\sh.exe"),
            Some("ProgramFiles(x86)")
        );
        for literal in ["50%.exe", "100% done\\sh.exe", "%%", "C:\\a%b\\sh.exe"] {
            assert_eq!(
                super::unexpanded_windows_env_var(literal),
                None,
                "{literal:?} holds no environment-variable reference"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
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
        assert!(!valid, "{message}");
        assert!(
            validate_config_text(
                &" ".repeat(crate::native_config_service::MAX_CONFIG_FILE_BYTES + 1),
            )
            .is_err()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cli_validation_verdict_says_the_file_loads_and_counts_the_warnings() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-validation-verdict-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        std::fs::write(&path, "qqq_unknown = 1\n").unwrap();
        let (message, valid) = validate_config_path(&path);
        assert!(valid, "{message}");
        assert!(message.contains("qqq_unknown"), "{message}");
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
            joined.contains("conflicts with built-in New Terminal Tab"),
            suite_live,
            "{joined}"
        );
        assert!(joined.contains("also bound in [keybindings]"), "{joined}");
    }

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

        let clean = validate_config_text(
            "theme = \"Default\"\ncursor_style = \"bar\"\nforeground = \"#aabbcc\"\n\
             minimum_contrast = 4.5\nline_height = 1.2\n",
        )
        .expect("valid");
        assert!(clean.is_empty(), "clean config must not warn: {clean:?}");

        for (source, token) in [
            ("theme = \"dark:Nord,lite:Dracula\"\n", "\"lite\""),
            ("theme = \"dark:,light:\"\n", "dark"),
        ] {
            let warnings = validate_config_text(source).expect("valid");
            assert!(
                warnings.len() == 1 && warnings[0].contains(token),
                "{source}: {warnings:?}"
            );
        }

        let native = validate_config_text("font_hinting = \"native\"\n")
            .expect("the retired hinting spelling remains loadable");
        assert_eq!(native.len(), 1, "{native:?}");
        assert!(native[0].contains("\"light\""), "{native:?}");
    }

    #[test]
    fn a_shift_composed_chord_is_reported_rather_than_accepted_in_silence() {
        for (chord, glyph, base) in [
            ("ctrl+shift+?", '?', '/'),
            ("cmd+shift+}", '}', ']'),
            ("ctrl+_", '_', '-'),
            ("alt+:", ':', ';'),
        ] {
            let warning = super::keybinding_row_warning(chord, "next_tab", None)
                .unwrap_or_else(|| panic!("{chord} can never fire and must say so"));
            assert!(
                warning.message.contains(chord),
                "the warning names the row: {}",
                warning.message
            );
            assert!(
                warning.message.contains(glyph),
                "...and the glyph that cannot arrive: {}",
                warning.message
            );
            assert!(
                warning.message.contains(base),
                "...and the spelling that works: {}",
                warning.message
            );
        }

        for ok in [
            "ctrl+shift+t",
            "cmd+1",
            "ctrl+shift+right",
            "alt+enter",
            "cmd+=",
        ] {
            assert!(
                super::keybinding_row_warning(ok, "next_tab", None).is_none(),
                "{ok} is reachable and must not warn"
            );
        }
    }

    #[test]
    fn validate_refuses_to_green_light_a_config_whose_key_is_misspelled() {
        let joined = validate_config_text("cursor_trail_stlye = \"rainbow kitty\"\n")
            .expect("structurally valid TOML")
            .join("\n");
        assert!(
            joined.contains("cursor_trail_stlye")
                && joined.contains("did you mean \"cursor_trail_style\""),
            "a misspelled key must be reported with its spelling: {joined}"
        );
        assert!(
            validate_config_text("cursor_trail_style = \"rainbow kitty\"\n")
                .expect("valid")
                .is_empty(),
            "the correct spelling must still validate green"
        );
    }

    #[test]
    fn validate_refuses_to_green_light_a_value_outside_the_vocabulary_it_accepts() {
        for (source, key, intended) in [
            ("window_theme = \"drak\"\n", "window_theme", "dark"),
            ("motion = \"redcued\"\n", "motion", "reduced"),
            ("right_click = \"of\"\n", "right_click", "off"),
        ] {
            let warnings = validate_config_text(source).expect("structurally valid TOML");
            let joined = warnings.join("\n");
            assert!(
                joined.contains(key) && joined.contains(&format!("did you mean {intended:?}")),
                "{key} must name {intended}: {joined}"
            );
        }
        for accepted in ["window_theme = \"dark\"\n", "right_click = \"off\"\n"] {
            let warnings = validate_config_text(accepted).expect("valid");
            assert!(warnings.is_empty(), "{accepted:?} warned: {warnings:?}");
        }
        for (source, key) in [
            ("cursor_style = \"blok\"\n", "cursor_style"),
            ("cursor_trail_style = \"phasr\"\n", "cursor_trail_style"),
        ] {
            let warnings = validate_config_text(source).expect("structurally valid TOML");
            assert_eq!(
                warnings.iter().filter(|line| line.starts_with(key)).count(),
                1,
                "{key} must be explained once, not twice: {warnings:?}"
            );
        }
    }

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

    #[test]
    fn validate_flags_unknown_trail_sound_style() {
        let joined = validate_config_text("trail_sound_style = \"glas bel\"\n")
            .expect("structurally valid TOML")
            .join("\n");
        assert!(
            joined.contains("trail_sound_style") && joined.contains("glas bel"),
            "typo'd typing sound must be flagged: {joined}"
        );
        for ok in crate::prefs::TRAIL_SOUND_STYLES.iter().copied().chain([
            "Glass Bell",
            "water",
            "mech",
            "thock",
            "raindrop",
            " felt ",
        ]) {
            let clean =
                validate_config_text(&format!("trail_sound_style = \"{ok}\"\n")).expect("valid");
            assert!(
                clean.iter().all(|w| !w.contains("is not a typing sound")),
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
                .to_string(),
            renderer_default: "cpu",
            shell_integration_runtime: "active (Zsh loader prepared)".into(),
            agent_primer: "claude installed, codex not detected — auto-prime: on \
                           (agents_auto_prime)"
                .into(),
            privacy: "full_disk_access=unknown probe=none fda_scope=unknown \
                      warmup=on-request folders=documents,desktop,downloads hold_ms=120000 \
                      probe_interval_ms=5000 protected_roots=17 notice=on report_attribution=on"
                .into(),
            debugger: "mode=unknown member=unknown self_hardened=yes \
                       right=system.privilege.taskport.debug probe=none"
                .into(),
            features: vec![("a11y-appkit", true), ("a11y-accesskit", false)],
            capabilities: vec![("kitty_graphics", true), ("soft_fonts", false)],
            objc_protocols_absent: vec!["NSApplicationDelegate"],
            config_path: "/home/u/.config/aterm/aterm.toml".into(),
            config_presence: ConfigPresence::Absent,
            env: vec![("ATERM_CHILD".into(), "1".into())],
        }
    }

    fn rejected_invalid() -> String {
        crate::app_config::launch_config_notice(
            std::path::Path::new("/home/u/.config/aterm/aterm.toml"),
            crate::app_config::LaunchConfigProblem::Invalid(
                &"TOML parse error at line 1, column 11\n  |\n1 | font_px = \n  |           \
                  ^\nexpected a value",
            ),
        )
    }

    #[test]
    fn diagnose_says_a_rejected_config_is_not_the_one_in_force() {
        let mut info = sample();
        info.config_presence = ConfigPresence::Rejected(rejected_invalid());
        let r = info.render();
        assert!(
            r.contains("REJECTED"),
            "the rejected state has its own token: {r}"
        );
        assert!(
            !r.contains("[present]"),
            "the in-force token must never stand for a rejected file: {r}"
        );
        assert!(
            r.contains("expected a value"),
            "the launch's reason, every line of it, is carried onto stdout: {r}"
        );
        let mut loaded = sample();
        loaded.config_presence = ConfigPresence::Loaded;
        assert!(loaded.render().contains("[present]"));
    }

    #[test]
    fn show_config_marks_a_rejected_config_and_its_listing_as_defaults() {
        let notice = crate::app_config::launch_config_notice(
            std::path::Path::new("/home/u/.config/aterm/aterm.toml"),
            crate::app_config::LaunchConfigProblem::Unreadable(&"Permission denied (os error 13)"),
        );
        let out = show_config_report(Some(&notice));
        assert!(
            out.contains("REJECTED"),
            "the header names the third state: {out}"
        );
        assert!(
            !out.contains("[present]") && !out.contains("[absent"),
            "neither honest-file token may stand in for a rejected one: {out}"
        );
        assert!(
            out.contains("os error 13"),
            "the reason reaches stdout: {out}"
        );
        assert!(out.contains("font_px:"), "the resolved values still print");
    }

    #[test]
    fn the_privacy_line_reports_unknown_from_the_inert_arm_and_never_probes() {
        use aterm_containment::{FdaProbe, FdaState, ProbeLabel};

        let config = parsed(&format!(
            "[privacy]\nprotected_roots = [\"{ABS_TMP}/one\", \"{ABS_TMP}/two\"]\n"
        ));
        let inert = privacy_line(&config, None);
        for token in [
            "full_disk_access=unknown",
            "probe=none",
            "fda_scope=unknown",
            "warmup=automatic",
            "hold_ms=120000",
            "probe_interval_ms=5000",
            "protected_roots=2",
            "notice=on",
            "report_attribution=on",
        ] {
            assert!(inert.contains(token), "{token} missing from {inert}");
        }

        let granted = FdaProbe {
            state: FdaState::Granted,
            label: ProbeLabel::OpenOk,
        };
        let live = privacy_line(&config, Some(granted));
        assert!(
            live.contains("full_disk_access=granted probe=open_ok"),
            "{live}"
        );
        assert!(
            live.contains("fda_scope=this_process"),
            "a completed grant establishes the observing host's scope, and this line \
             renders the same rule the privacy verb does: {live}"
        );

        let unchecked = privacy_line(&parsed("[privacy]\ncheck = false\n"), Some(granted));
        assert!(
            unchecked.contains("full_disk_access=unknown probe=off"),
            "{unchecked}"
        );

        let disabled = privacy_line(&parsed("[privacy]\nenabled = false\n"), Some(granted));
        assert!(
            disabled.starts_with("off ([privacy] enabled = false)")
                && disabled.contains("reads unknown"),
            "{disabled}"
        );

        assert!(
            sample()
                .render()
                .contains("privacy:   full_disk_access=unknown probe=none"),
            "the section is rendered under its own label"
        );
        let live_collect = collect().privacy;
        assert!(
            !live_collect.contains("probe=open_"),
            "collect() must never perform the probe: {live_collect}"
        );
        assert!(
            live_collect.starts_with("off ([privacy]")
                || live_collect.contains("full_disk_access=unknown"),
            "{live_collect}"
        );
    }

    #[test]
    fn the_debugger_line_reports_unknown_from_the_inert_arm_and_never_probes() {
        use aterm_containment::consent::{
            DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel, SelfHardening,
        };

        let config = parsed("");
        let inert = debugger_line(&config, None, SelfHardening::NotHardened);
        assert_eq!(
            inert,
            "mode=unknown member=unknown self_hardened=no \
             right=system.privilege.taskport.debug probe=none"
        );

        let observed = DebuggerProbe {
            mode: DebuggerMode::Disabled,
            member: DebuggerMember::Admin,
            label: DebuggerProbeLabel::StatusOk,
        };
        let live = debugger_line(&config, Some(observed), SelfHardening::Hardened);
        assert!(
            live.starts_with("mode=disabled member=admin self_hardened=yes ")
                && live.ends_with(" probe=status_ok"),
            "{live}"
        );
        let refused = debugger_line(
            &config,
            Some(DebuggerProbe::refused(
                DebuggerProbeLabel::RefusedOutOfBundle,
            )),
            SelfHardening::Unknown,
        );
        assert!(
            refused.starts_with("mode=unknown member=unknown self_hardened=unknown ")
                && refused.ends_with(" probe=refused_out_of_bundle"),
            "{refused}"
        );

        let unchecked = debugger_line(
            &parsed("[privacy]\ncheck = false\n"),
            Some(observed),
            SelfHardening::Hardened,
        );
        assert!(
            unchecked.starts_with("mode=unknown member=unknown ")
                && unchecked.ends_with(" probe=off"),
            "{unchecked}"
        );

        let disabled = debugger_line(
            &parsed("[privacy]\nenabled = false\n"),
            Some(observed),
            SelfHardening::Hardened,
        );
        assert!(
            disabled.starts_with("off ([privacy] enabled = false)")
                && disabled.contains("reads unknown"),
            "{disabled}"
        );

        assert!(
            sample()
                .render()
                .contains("\ndebugger:  mode=unknown member=unknown "),
            "the section is rendered under its own label"
        );
        let live_collect = collect().debugger;
        assert!(
            live_collect.starts_with("off ([privacy]")
                || (live_collect.starts_with("mode=unknown member=unknown ")
                    && (live_collect.ends_with(" probe=none")
                        || live_collect.ends_with(" probe=off"))),
            "collect() must never perform the probe: {live_collect}"
        );
    }

    #[test]
    fn agent_primer_line_states_the_knob_as_configured() {
        let on = agent_primer_line(&crate::app_config::Config::default());
        assert!(on.contains("auto-prime: on"), "{on}");
        let config = crate::app_config::Config {
            agents_auto_prime: Some(false),
            ..crate::app_config::Config::default()
        };
        let off = agent_primer_line(&config);
        assert!(off.contains("auto-prime: off"), "{off}");
    }

    fn theme_list_scratch(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aterm-theme-list-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn tsv_rows(out: &str) -> Vec<Vec<&str>> {
        out.lines()
            .skip(1)
            .filter(|line| !line.starts_with('#'))
            .map(|line| line.split('\t').collect())
            .collect()
    }

    fn tsv_field<'a>(row: &[&'a str], column: &str) -> &'a str {
        let index = THEME_LIST_TSV_COLUMNS
            .iter()
            .position(|c| *c == column)
            .expect("a listing column");
        row[index]
    }

    #[test]
    fn catalog_theme_is_the_catalogs_view_of_a_theme() {
        let dir = theme_list_scratch("entry");
        std::fs::write(
            dir.join("Paper.conf"),
            "name = Paper Day\nauthor = Me\nforeground = #202020\nbackground = #f4f4f0\n",
        )
        .unwrap();
        std::fs::write(dir.join("Broken.conf"), "foreground = #ffffff\n").unwrap();
        let catalog = crate::app_config::ThemeCatalog::discover_in(&dir);

        let paper = catalog_theme(&catalog, "paper").expect("a loaded file");
        assert_eq!(paper.id, "Paper");
        assert_eq!(paper.meta.display_name, "Paper Day");
        assert_eq!(paper.theme.author.as_deref(), Some("Me"));
        assert!(!paper.theme.appearance_declared);
        assert_eq!(paper.meta.appearance, aterm_types::Appearance::Light);
        assert_eq!(
            paper.theme.scheme.appearance,
            aterm_types::Appearance::Light
        );
        assert_eq!(
            theme_measured_appearance(paper.theme.scheme.background),
            aterm_types::Appearance::Light
        );

        let broken = catalog_theme(&catalog, "Broken").expect_err("did not load");
        assert!(broken.contains("Broken"), "{broken}");
        assert!(catalog_theme(&catalog, "Absent").is_err());
        let nord = theme_catalog_entry("nord").expect("a built-in");
        assert_eq!(nord.id, "Nord");
        assert!(nord.theme.appearance_declared);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_theme_list_has_one_row_per_theme_default_first() {
        let dir = theme_list_scratch("rows");
        let theme = "foreground = #e0e0e0\nbackground = #101010\n";
        std::fs::write(dir.join("Work.conf"), format!("name = Work Night\n{theme}")).unwrap();
        std::fs::write(dir.join("Broken.conf"), "foreground = #ffffff\n").unwrap();
        std::fs::write(dir.join("Nord.conf"), theme).unwrap();
        let catalog = crate::app_config::ThemeCatalog::discover_in(&dir);
        let builtins = aterm_types::scheme::builtin_theme_list();
        let expected = builtins.len() + catalog.listing().count();
        assert_eq!(catalog.listing().count(), 3, "every file has a catalog row");

        let tsv = theme_list(&catalog, Some(&dir), None, ThemeListFormat::Tsv);
        assert_eq!(
            tsv.lines().next(),
            Some(THEME_LIST_TSV_COLUMNS.join("\t").as_str()),
            "the header names the columns"
        );
        let rows = tsv_rows(&tsv);
        assert_eq!(rows.len(), expected, "{tsv}");
        assert_eq!(tsv_field(&rows[0], "id"), "Default", "{tsv}");
        for (row, builtin) in rows.iter().zip(&builtins) {
            assert_eq!(tsv_field(row, "id"), builtin.name);
            assert_eq!(tsv_field(row, "source"), "builtin");
            assert_eq!(tsv_field(row, "pair"), builtin.pair.unwrap_or_default());
        }
        let file = |id: &str| {
            rows.iter()
                .find(|row| tsv_field(row, "id") == id && tsv_field(row, "source") == "file")
                .unwrap_or_else(|| panic!("no file row {id}: {tsv}"))
        };
        let work = file("Work");
        assert_eq!(tsv_field(work, "label"), "Work Night");
        assert_eq!(tsv_field(work, "appearance"), "dark");
        assert_eq!(tsv_field(work, "appearance_source"), "measured");
        assert_eq!(tsv_field(work, "status"), "ready");
        let broken = file("Broken");
        assert_eq!(tsv_field(broken, "status"), "invalid");
        assert!(!tsv_field(broken, "notes").is_empty(), "a refusal says why");
        assert_eq!(tsv_field(broken, "appearance"), "");
        let nord = file("Nord");
        assert_eq!(tsv_field(nord, "status"), "shadowed");
        assert!(!tsv_field(nord, "notes").is_empty());
        assert_eq!(
            tsv.lines().last(),
            Some(format!("#folder\t{}", dir.display()).as_str()),
            "the folder closes the listing"
        );

        let text = theme_list(&catalog, Some(&dir), None, ThemeListFormat::Text);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[1].starts_with("ID "), "{text}");
        let row_ids: Vec<&str> = lines[2..]
            .iter()
            .take_while(|line| !line.is_empty())
            .filter(|line| !line.starts_with(' '))
            .map(|line| line.split("  ").next().unwrap_or_default())
            .collect();
        let tsv_ids: Vec<&str> = rows.iter().map(|row| tsv_field(row, "id")).collect();
        assert_eq!(row_ids, tsv_ids, "{text}");
        let broken_at = lines
            .iter()
            .position(|line| line.starts_with("Broken "))
            .expect("broken row");
        assert!(
            lines[broken_at + 1].starts_with("    ") && lines[broken_at + 1].len() > 8,
            "the reason follows its row: {text}"
        );
        assert!(text.contains(&dir.display().to_string()), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_theme_list_names_typography_and_skipped_keys() {
        let dir = theme_list_scratch("typography");
        let base = "foreground = #e0e0e0\nbackground = #101010\n";
        std::fs::write(
            dir.join("Studio.conf"),
            format!("{base}font_family = Berkeley Mono\nline_height = 1.15\n"),
        )
        .unwrap();
        std::fs::write(
            dir.join("Future.conf"),
            format!("schema = 2\n{base}future_key = x\n"),
        )
        .unwrap();
        std::fs::write(dir.join("Plain.conf"), base).unwrap();
        let catalog = crate::app_config::ThemeCatalog::discover_in(&dir);
        let tsv = theme_list(&catalog, Some(&dir), None, ThemeListFormat::Tsv);
        let rows = tsv_rows(&tsv);
        let row = |id: &str| {
            rows.iter()
                .find(|row| tsv_field(row, "id") == id)
                .unwrap_or_else(|| panic!("{id}: {tsv}"))
        };
        assert!(tsv_field(row("Studio"), "typography").contains("Berkeley Mono"));
        assert_eq!(tsv_field(row("Studio"), "skipped"), "0");
        assert_eq!(tsv_field(row("Future"), "skipped"), "1");
        assert_eq!(tsv_field(row("Future"), "status"), "ready");
        assert!(tsv_field(row("Future"), "notes").contains("future_key"));
        assert_eq!(tsv_field(row("Plain"), "typography"), "");
        let text = theme_list(&catalog, Some(&dir), None, ThemeListFormat::Text);
        assert!(
            text.lines().nth(1).is_some_and(|h| h.contains("TYPE")),
            "{text}"
        );
        let future_at = text
            .lines()
            .position(|line| line.starts_with("Future "))
            .expect("the Future row");
        assert!(
            text.lines()
                .nth(future_at + 1)
                .is_some_and(
                    |line| line.trim_start().starts_with("warning:") && line.contains("future_key")
                ),
            "{text}"
        );
        let wire = theme_list_wire_rows(&catalog, Some(&dir), "Default");
        let studio = wire
            .iter()
            .find(|line| line.contains("id=\"Studio\""))
            .expect("Studio's row");
        assert!(studio.contains("type=\"Berkeley Mono"), "{studio}");
        assert!(studio.ends_with("skipped=0"), "{studio}");
        let plain = wire
            .iter()
            .find(|line| line.contains("id=\"Plain\""))
            .expect("Plain's row");
        assert!(plain.contains("type=- "), "{plain}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_tsv_row_has_the_header_column_count() {
        let dir = theme_list_scratch("tsv");
        let hostile = "Tab\there\u{1b}[31m";
        if cfg!(unix) {
            std::fs::write(
                dir.join(format!("{hostile}.conf")),
                "foreground = #e0e0e0\nbackground = #101010\n",
            )
            .unwrap();
        }
        std::fs::write(dir.join("Plain.conf"), "nonsense\twith\ttabs\n").unwrap();
        let catalog = crate::app_config::ThemeCatalog::discover_in(&dir);
        let tsv = theme_list(&catalog, Some(&dir), None, ThemeListFormat::Tsv);
        let rows = tsv_rows(&tsv);
        assert!(rows.len() > aterm_types::scheme::builtin_theme_list().len());
        for row in &rows {
            assert_eq!(row.len(), THEME_LIST_TSV_COLUMNS.len(), "{row:?}");
        }
        assert!(
            !tsv.contains('\u{1b}'),
            "no raw escape reaches the terminal"
        );
        assert!(hostile.split('\t').count() > 1, "negative control");
        assert_eq!(listing_field(hostile).split('\t').count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_appearance_filter_keeps_only_that_side() {
        let dir = theme_list_scratch("side");
        std::fs::write(
            dir.join("Paper.conf"),
            "foreground = #202020\nbackground = #fafafa\n",
        )
        .unwrap();
        std::fs::write(dir.join("Broken.conf"), "foreground = #ffffff\n").unwrap();
        let catalog = crate::app_config::ThemeCatalog::discover_in(&dir);
        let all = tsv_rows(&theme_list(&catalog, None, None, ThemeListFormat::Tsv)).len();
        let mut kept = 0;
        for (side, word) in [
            (aterm_types::Appearance::Light, "light"),
            (aterm_types::Appearance::Dark, "dark"),
        ] {
            let tsv = theme_list(&catalog, None, Some(side), ThemeListFormat::Tsv);
            let rows = tsv_rows(&tsv);
            assert!(!rows.is_empty());
            for row in &rows {
                assert_eq!(tsv_field(row, "appearance"), word, "{tsv}");
            }
            kept += rows.len();
        }
        assert_eq!(kept + 1, all, "only the unloaded file drops out");
        let light = theme_list(
            &catalog,
            None,
            Some(aterm_types::Appearance::Light),
            ThemeListFormat::Tsv,
        );
        assert!(light.lines().any(|l| l.starts_with("Paper\t")), "{light}");
        assert!(
            !light.lines().any(|l| l.starts_with("Default\t")),
            "{light}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_truncated_folder_says_so_in_both_formats() {
        let dir = theme_list_scratch("cap");
        let theme = "foreground = #e0e0e0\nbackground = #101010\n";
        std::fs::write(dir.join("Only.conf"), theme).unwrap();
        let small = crate::app_config::ThemeCatalog::discover_in(&dir);
        let tsv = theme_list(&small, Some(&dir), None, ThemeListFormat::Tsv);
        assert!(!tsv.lines().any(|l| l.starts_with("#truncated")), "{tsv}");
        let cap = crate::app_config::MAX_USER_THEME_FILES;
        for index in 0..=cap {
            std::fs::write(dir.join(format!("Theme-{index:03}.conf")), theme).unwrap();
        }
        let catalog = crate::app_config::ThemeCatalog::discover_in(&dir);
        let tsv = theme_list(&catalog, Some(&dir), None, ThemeListFormat::Tsv);
        let trailer: Vec<&str> = tsv.lines().last().unwrap_or_default().split('\t').collect();
        assert_eq!(trailer[0], "#truncated", "{tsv}");
        assert_eq!(trailer[1], cap.to_string());
        assert_eq!(trailer[2], (cap + 2).to_string());
        let text = theme_list(&catalog, Some(&dir), None, ThemeListFormat::Text);
        let last = text.lines().last().unwrap_or_default();
        assert!(last.contains(&cap.to_string()) && last.contains(&(cap + 2).to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn user_keybinding_note_labels_unbinds_and_unknowns() {
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
        assert_eq!(
            user_keybinding_note_for("no_such_action", Some("Copy")),
            "  (UNKNOWN action)"
        );
        assert_eq!(
            user_keybinding_note_for("copy", Some("Copy")),
            "  (conflicts with Copy)"
        );
    }

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

        assert!(keybinding_row_warning("cmd+c", "none", None).is_none());
        assert!(keybinding_row_warning("f11", "unbind", None).is_none());

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

    #[test]
    fn show_config_names_the_shell_a_new_tab_starts() {
        use aterm_pty::ShellOrigin as O;
        let out = show_config();
        let config = crate::app_config::load_config();
        let chosen = crate::app_config::resolve_shell_override(&config);
        let (spawned, _) =
            aterm_pty::select_shell_with_origin(chosen.as_deref().map(std::ffi::OsStr::new));
        let spawned = spawned.to_string_lossy().into_owned();
        let (program, origin) = show_config_shell(&config);
        assert!(!program.is_empty());
        assert_eq!(program, spawned, "the row is the program the spawn starts");
        assert!(
            out.contains(&format!("\nshell:          {program}\n")),
            "{out}"
        );
        assert!(
            out.contains(&format!("\nshell_origin:   {origin}\n")),
            "{out}"
        );
        assert_eq!(shell_origin_token(O::Override, true), "--shell");
        assert_eq!(shell_origin_token(O::Override, false), "aterm.toml");
        #[cfg(unix)]
        {
            assert_eq!(shell_origin_token(O::Env, false), "default:SHELL");
            assert_eq!(shell_origin_token(O::Passwd, false), "default:passwd");
            assert_eq!(shell_origin_token(O::Default, true), "default:/bin/sh");
        }
    }

    #[test]
    fn show_config_names_the_console_host_on_windows_only() {
        let out = show_config();
        let row = out.lines().find(|l| l.starts_with("console_host:"));
        #[cfg(windows)]
        {
            let config = crate::app_config::load_config();
            let host = aterm_pty::preview_console_host(&aterm_pty::ConsoleHostChoice::from_config(
                config.conpty.as_deref(),
            ));
            assert!(!host.line.is_empty());
            let want = format!("console_host:   {}", host.line);
            assert_eq!(row, Some(want.as_str()), "{out}");
            assert!(
                out.find("\nshell_origin:") < out.find("\nconsole_host:"),
                "the host follows the shell it hosts: {out}"
            );
        }
        #[cfg(not(windows))]
        assert_eq!(row, None, "{out}");
    }

    #[test]
    fn show_config_type_rows_name_their_sources() {
        let parse = |extra: &str| {
            aterm_types::scheme::parse_theme_str(&format!(
                "foreground = #e0e0e0\nbackground = #101418\n{extra}"
            ))
            .expect("theme")
        };
        let themes = crate::app_config::ThemeCatalog::from_parsed([
            (
                "Studio".to_string(),
                parse("font_family = display:pixel\nline_height = 1.25\nfont_size_scale = 1.1"),
            ),
            ("Plain".to_string(), parse("")),
        ]);
        let admit = |_: Option<&str>| Ok(None);
        let rows = |text: &str, flag: Option<&str>| {
            let config: crate::app_config::Config = aterm_toml::from_str(text).expect("toml");
            show_config_type_rows(&config, &themes, flag, admit)
        };
        let themed = rows("theme = \"Studio\"\nfont_px = 20.0\n", None);
        let default = aterm_render::default_primary_identity().map_or_else(
            || "the embedded last-resort face".to_string(),
            |face| face.path,
        );
        assert_eq!(
            themed.font_family,
            format!("(default) {default} with display faces pixel  (source: theme Studio)")
        );
        assert_eq!(themed.line_height, "1.25  (source: theme Studio)");
        assert_eq!(themed.font_size_scale, "1.1  (source: theme Studio)");
        assert_eq!(themed.font_px, [22.0, 22.0], "the scale applies to font_px");
        let mine = rows("theme = \"Studio\"\nline_height = 1.0\n", Some("Menlo"));
        assert_eq!(mine.font_family, "Menlo  (source: --font)");
        assert_eq!(mine.line_height, "1  (source: config)");
        let split = rows("theme = \"dark:Studio,light:Plain\"\n", None);
        assert_eq!(
            split.line_height,
            format!(
                "1.25 (dark, theme Studio) / {} (light, default)",
                crate::app_config::DEFAULT_LINE_HEIGHT
            )
        );
        let plain = rows("theme = \"Plain\"\n", None);
        assert!(
            plain.font_family.ends_with("(source: default)"),
            "{}",
            plain.font_family
        );
        let default = crate::app_config::DEFAULT_LINE_HEIGHT.to_string();
        assert_eq!(plain.line_height, format!("{default}  (source: default)"));
        let nan = rows("theme = \"Plain\"\nline_height = nan\n", None);
        assert_eq!(
            nan.line_height,
            format!("{default}  (source: default)"),
            "a non-finite config line height falls back to the platform default"
        );
        let themed_nan = rows("theme = \"Studio\"\nline_height = nan\n", None);
        assert_eq!(
            themed_nan.line_height, "1.25  (source: theme Studio)",
            "a non-finite config line height leaves the theme's in force"
        );
    }

    #[test]
    fn show_config_colour_source_names_theme_and_overrides() {
        let themes = crate::app_config::ThemeCatalog::default();
        let config: crate::app_config::Config = aterm_toml::from_str(
            "theme = \"dark:Dracula,light:no-such-theme\"\nforeground = \"#102030\"\n\
             palette = [\"#010203\", \"nope\"]",
        )
        .unwrap();
        let [dark, light] = [
            aterm_types::Appearance::Dark,
            aterm_types::Appearance::Light,
        ]
        .map(|appearance| {
            super::color_source_label(&config.resolved_colors(appearance, &themes, None))
        });
        assert!(dark.starts_with("Dracula (built-in)"), "{dark}");
        assert!(light.starts_with("Default (fallback: "), "{light}");
        for side in [&dark, &light] {
            assert!(
                side.contains("config sets foreground, palette[0]"),
                "{side}"
            );
            assert!(side.contains("ignored palette[1]"), "{side}");
        }
        let row = super::appearance_labels(dark.clone(), light.clone());
        assert!(
            row.contains(" (dark) / ") && row.ends_with(" (light)"),
            "{row}"
        );
        let plain =
            super::color_source_label(&crate::app_config::Config::default().resolved_colors(
                aterm_types::Appearance::Dark,
                &themes,
                None,
            ));
        assert_eq!(plain, "Default (built-in)");
    }

    #[test]
    fn renderer_label_tracks_resolved_gpu_precedence() {
        use crate::app_config::resolve_want_gpu_with;
        assert_eq!(
            renderer_label(resolve_want_gpu_with(true, true, Some(true))),
            "cpu"
        );
        assert_eq!(
            renderer_label(resolve_want_gpu_with(false, true, Some(false))),
            GPU_BACKEND_LABEL
        );
        assert_eq!(
            renderer_label(resolve_want_gpu_with(false, false, Some(false))),
            "cpu"
        );
        assert_eq!(
            renderer_label(resolve_want_gpu_with(false, false, Some(true))),
            GPU_BACKEND_LABEL
        );
        assert_eq!(
            renderer_label(resolve_want_gpu_with(false, false, None)),
            GPU_BACKEND_LABEL
        );
    }

    #[test]
    fn show_face_rejects_an_unresolvable_family() {
        let (msg, ok) = show_face("definitely-not-a-real-font-xyzzy");
        assert!(!ok, "unresolvable family must report failure");
        assert!(
            msg.ends_with('\n'),
            "a message is always a whole line: {msg:?}"
        );
    }

    #[test]
    fn show_face_reports_the_default_face_for_the_generic_monospace_name() {
        let default = aterm_render::default_primary_identity().expect("a default face loads");
        let (msg, ok) = show_face("monospace");
        assert!(ok, "{msg}");
        assert!(
            msg.contains(&format!("path:        {}\n", default.path)),
            "{msg}"
        );
    }
}
