// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Latest-wins off-thread preparation of immutable config font generations.

use aterm_window::event_loop::EventLoopProxy;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

use crate::app_config::{Config, ConfigAssetCatalog, FontConfig};
use crate::native_config_service::ConfigDiskObservation;
use crate::{Wake, effective_font_family};

/// How the worker's refusal of the configured `font_family` opens.
pub(crate) const FAMILY_REFUSED: &str = "config font_family:";

/// How the worker's refusal of a font a THEME names opens.
pub(crate) const THEME_FAMILY_REFUSED: &str = "theme font:";

/// The face the terminal draws now.
#[derive(Clone)]
pub(crate) struct PrimarySeed {
    pub(crate) face: Option<aterm_render::FaceRef>,
    pub(crate) bytes: Arc<[u8]>,
}

#[derive(Clone)]
pub(crate) struct Request {
    pub(crate) sequence: u64,
    pub(crate) theme_generation: u64,
    pub(crate) prepared: crate::native_config_service::PreparedConfigObservation,
    pub(crate) px: f32,
    pub(crate) appearance: aterm_types::Appearance,
    pub(crate) previous_family: Option<String>,
    pub(crate) previous_font_config: FontConfig,
    pub(crate) previous_sources: aterm_render::AdmittedFontSources,
    pub(crate) previous_variations: Vec<(u32, f32)>,
    pub(crate) previous_dark_nudge: f32,
    pub(crate) primary: PrimarySeed,
    /// What asked for this generation: a config observation (`ConfigReload`), or a theme catalog reload or OS flip that changes the face.
    pub(crate) reason: crate::app_colorscheme::ThemeChange,
}

/// One font row whose authored name the terminal could not honour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FontRefusal {
    pub(crate) key: &'static str,
    pub(crate) family: String,
    pub(crate) reason: FontRefusalReason,
    /// The theme that named it, when the name came from a theme file rather than the config: Settings then says "(from theme X, not installed)" instead of marking the person's own value "Not found".
    pub(crate) from_theme: Option<String>,
}

/// Why a font row's name was not honoured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FontRefusalReason {
    /// No resolver found the name.
    NotFound,
    /// The name names a face that does not draw the row's style (`Menlo Italic` in the Bold
    /// row): the style it draws there, which the row says.
    WrongStyle(aterm_render::DrawnStyle),
}

pub(crate) struct PreparedFonts {
    pub(crate) renderer: aterm_render::Renderer,
    pub(crate) family: Option<String>,
    pub(crate) config: FontConfig,
    pub(crate) chrome: crate::tray_raster::PreparedChromeFonts,
}

pub(crate) struct Completion {
    pub(crate) sequence: u64,
    pub(crate) theme_generation: u64,
    pub(crate) observation: ConfigDiskObservation,
    pub(crate) config: Config,
    pub(crate) values: std::collections::BTreeMap<String, String>,
    pub(crate) disk_faults: crate::native_config_service::DiskFaults,
    pub(crate) assets: Arc<ConfigAssetCatalog>,
    pub(crate) path_feed_fps: crate::app_config::PathFeedFps,
    pub(crate) sparkle: crate::app_config::PreparedSparkleRuntime,
    pub(crate) fonts: Option<PreparedFonts>,
    pub(crate) warnings: Vec<String>,
    pub(crate) font_refusals: Vec<FontRefusal>,
    /// The OS appearance the face was resolved for (the request's).
    pub(crate) appearance: aterm_types::Appearance,
    /// The request's reason.
    pub(crate) reason: crate::app_colorscheme::ThemeChange,
}

/// Complete immutable config generation consumed by the event-loop reducer.
pub(crate) struct PreparedConfigGeneration {
    pub(crate) observation: ConfigDiskObservation,
    pub(crate) config: Config,
    pub(crate) values: std::collections::BTreeMap<String, String>,
    pub(crate) disk_faults: crate::native_config_service::DiskFaults,
    pub(crate) assets: Arc<ConfigAssetCatalog>,
    pub(crate) path_feed_fps: crate::app_config::PathFeedFps,
    pub(crate) sparkle: crate::app_config::PreparedSparkleRuntime,
    pub(crate) fonts: Option<PreparedFonts>,
    pub(crate) warnings: Vec<String>,
    /// The font rows Settings marks "Not found" for this generation.
    pub(crate) font_refusals: Vec<FontRefusal>,
    /// Why this generation was prepared: the colour install's reason.
    pub(crate) reason: crate::app_colorscheme::ThemeChange,
}

impl Completion {
    pub(crate) fn into_generation(self) -> PreparedConfigGeneration {
        PreparedConfigGeneration {
            observation: self.observation,
            config: self.config,
            values: self.values,
            disk_faults: self.disk_faults,
            assets: self.assets,
            path_feed_fps: self.path_feed_fps,
            sparkle: self.sparkle,
            fonts: self.fonts,
            warnings: self.warnings,
            font_refusals: self.font_refusals,
            reason: self.reason,
        }
    }
}

impl std::fmt::Debug for Completion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontCatalogCompletion")
            .field("sequence", &self.sequence)
            .field("theme_generation", &self.theme_generation)
            .field("appearance", &self.appearance)
            .field("reason", &self.reason)
            .field("fonts_ready", &self.fonts.is_some())
            .field("warnings", &self.warnings)
            .finish_non_exhaustive()
    }
}

pub(crate) struct Lane {
    tx: Option<SyncSender<Request>>,
    proxy: Option<EventLoopProxy<Wake>>,
    pending: Option<Request>,
}

impl Lane {
    #[expect(
        clippy::unnecessary_wraps,
        reason = "callers keep the fallible startup signature while the worker spawns on first demand"
    )]
    pub(crate) fn spawn(proxy: EventLoopProxy<Wake>) -> Result<Self, String> {
        Ok(Self {
            tx: None,
            proxy: Some(proxy),
            pending: None,
        })
    }

    fn ensure_worker(&mut self) -> Option<&SyncSender<Request>> {
        if self.tx.is_none() {
            let proxy = self.proxy.take()?;
            let (tx, rx) = sync_channel(1);
            match crate::qos::spawn_lane_thread(
                "aterm-font-catalog",
                crate::qos::Role::Background,
                move || {
                    worker(rx, proxy);
                },
            ) {
                Ok(_) => self.tx = Some(tx),
                Err(error) => {
                    crate::logging::stderr_line!(
                        "aterm-gui: could not start font catalog worker: {error}"
                    );
                }
            }
        }
        self.tx.as_ref()
    }

    /// True only when the request is queued or retained behind a live receiver.
    pub(crate) fn request(&mut self, request: Request) -> bool {
        self.pending = Some(request);
        self.dispatch()
    }

    pub(crate) fn worker_drained(&mut self) {
        let _ = self.dispatch();
    }

    fn dispatch(&mut self) -> bool {
        let Some(request) = self.pending.take() else {
            return true;
        };
        let Some(tx) = self.ensure_worker() else {
            return false;
        };
        match tx.try_send(request) {
            Ok(()) => true,
            Err(TrySendError::Full(request)) => {
                self.pending = Some(request);
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    #[cfg(test)]
    fn test_pair() -> (Self, Receiver<Request>) {
        let (tx, rx) = sync_channel(1);
        (
            Self {
                tx: Some(tx),
                proxy: None,
                pending: None,
            },
            rx,
        )
    }
}

fn worker(rx: Receiver<Request>, proxy: EventLoopProxy<Wake>) {
    while let Ok(mut request) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            request = newer;
        }
        if proxy
            .send_event(Wake::FontCatalogPrepared(Box::new(prepare(request))))
            .is_err()
        {
            return;
        }
    }
}

pub(crate) fn prepare(request: Request) -> Completion {
    let crate::native_config_service::PreparedConfigObservation {
        observation,
        config: source_config,
        values,
        disk_faults,
        assets,
        path_feeds: path_generation,
    } = request.prepared;
    let path_feed_fps = path_generation.fingerprints;
    let sparkle = path_generation.sparkle;
    debug_assert_eq!(
        assets.sparkle_spec_consumers.as_deref(),
        Some(&sparkle.consumer_capabilities())
    );
    let theme = source_config.theme_for_with_assets(request.appearance, &assets.themes);
    let typography = source_config.resolved_typography(request.appearance, &assets.themes);
    let view = typography.overlay(&source_config);
    let from_theme = |key: &str| {
        (typography.origin(key, &source_config) == crate::resolved_typography::TypeOrigin::Theme)
            .then(|| typography.id.clone())
    };
    let requested_family = effective_font_family(view.font_family_request().as_deref());
    let family_from_theme = requested_family.is_some()
        && requested_family.as_deref().map(str::trim)
            == typography.applied.font_family.as_deref().map(str::trim)
        && from_theme("font_family").is_some();
    let styles = [
        view.font_family_bold.as_deref(),
        view.font_family_italic.as_deref(),
        view.font_family_bold_italic.as_deref(),
    ];
    let style_keys = [
        "font_family_bold",
        "font_family_italic",
        "font_family_bold_italic",
    ];
    let fallbacks = view
        .fallback_fonts
        .as_ref()
        .map(|fonts| fonts.0.as_slice())
        .unwrap_or(&[]);

    let regular = aterm_render::FaceTarget::REGULAR;
    let mut names: Vec<(String, aterm_render::FaceTarget)> = Vec::new();
    let mut push = |value: &str, target: aterm_render::FaceTarget| {
        let index = names.len();
        names.push((value.to_string(), target));
        index
    };
    let primary_i = requested_family
        .as_deref()
        .map(|value| push(value, regular));
    let style_i: [Option<usize>; 3] = std::array::from_fn(|slot| {
        styles[slot]
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|value| push(value, aterm_render::FaceTarget::for_slot(slot)))
    });
    let fallback_i = fallbacks
        .iter()
        .map(|value| push(value, regular))
        .collect::<Vec<_>>();
    let symbol_i = view
        .symbol_font
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|value| push(value, regular));
    let emoji_i = view
        .emoji_font
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|value| push(value, regular));
    let batch = aterm_render::font_catalog::resolve_and_admit(&names);
    let admitted = |index: usize| {
        batch
            .get(index)
            .and_then(|result| result.as_ref().ok())
            .cloned()
    };
    let mut warnings = Vec::new();
    let theme_miss = |key: &str, name: &str, next: &str| {
        format!(
            "{THEME_FAMILY_REFUSED} {}: {key} {name:?} is not installed here; using {next}",
            typography.id
        )
    };
    let refused =
        |index: Option<usize>| index.filter(|index| batch.get(*index).is_some_and(Result::is_err));
    let mut font_refusals = Vec::new();
    let family_requested = family_from_theme
        || requested_family.as_deref().map(str::trim)
            == source_config.font_family.as_deref().map(str::trim);
    for (key, theme_key, index) in [
        (
            crate::prefs::EDIT_FONT_FAMILY,
            "font_family",
            primary_i.filter(|_| family_requested),
        ),
        (
            crate::prefs::EDIT_FONT_FAMILY_BOLD,
            style_keys[0],
            style_i[0],
        ),
        (
            crate::prefs::EDIT_FONT_FAMILY_ITALIC,
            style_keys[1],
            style_i[1],
        ),
        (
            crate::prefs::EDIT_FONT_FAMILY_BOLD_ITALIC,
            style_keys[2],
            style_i[2],
        ),
        (crate::prefs::EDIT_SYMBOL_FONT, "symbol_font", symbol_i),
        (crate::prefs::EDIT_EMOJI_FONT, "emoji_font", emoji_i),
    ] {
        if let Some(index) = refused(index) {
            font_refusals.push(FontRefusal {
                key,
                family: names[index].0.clone(),
                reason: FontRefusalReason::NotFound,
                from_theme: from_theme(theme_key),
            });
        }
    }

    let primary_admitted = primary_i.and_then(admitted);
    if primary_admitted.is_none()
        && family_from_theme
        && let Some(name) = requested_family.as_deref()
    {
        warnings.push(theme_miss("font_family", name, "aterm's default face"));
    }
    let requested_family = if primary_admitted.is_none() && family_from_theme {
        None
    } else {
        requested_family
    };
    let (primary_face, primary_bytes, family) = match primary_admitted {
        Some(asset) => (
            Some(asset.face()),
            Arc::<[u8]>::from(asset.bytes.as_slice()),
            requested_family,
        ),
        None if requested_family.is_some() => {
            let error = primary_i
                .and_then(|i| batch.get(i))
                .and_then(|r| r.as_ref().err())
                .cloned()
                .unwrap_or_else(|| "font resolution failed".into());
            warnings.push(format!(
                "{FAMILY_REFUSED} {error}; keeping the current working face"
            ));
            (
                request.primary.face.clone(),
                Arc::clone(&request.primary.bytes),
                request.previous_family.clone(),
            )
        }
        None if request.previous_family.is_some() => {
            let candidates = aterm_render::primary_font_candidate_paths()
                .into_iter()
                .map(|candidate| (candidate, regular))
                .collect::<Vec<_>>();
            let batch = aterm_render::font_catalog::resolve_and_admit(&candidates);
            match batch
                .entries
                .into_iter()
                .find_map(|entry| entry.result.ok())
            {
                Some(asset) => (
                    Some(asset.face()),
                    Arc::<[u8]>::from(asset.bytes.as_slice()),
                    None,
                ),
                None => (
                    request.primary.face.clone(),
                    Arc::clone(&request.primary.bytes),
                    request.previous_family.clone(),
                ),
            }
        }
        None => (
            request.primary.face.clone(),
            Arc::clone(&request.primary.bytes),
            None,
        ),
    };

    let renderer = match primary_face {
        Some(face) => {
            aterm_render::Renderer::from_resolved_font_file(face, &primary_bytes, request.px, theme)
        }
        None => aterm_render::Renderer::from_bytes(&primary_bytes, request.px, theme),
    };
    let mut renderer = match renderer {
        Ok(renderer) => renderer,
        Err(error) => {
            warnings.push(format!(
                "prepared primary font rejected ({error}); keeping current face"
            ));
            return Completion {
                sequence: request.sequence,
                theme_generation: request.theme_generation,
                observation,
                config: source_config,
                values,
                disk_faults,
                assets,
                path_feed_fps,
                sparkle,
                fonts: None,
                warnings,
                font_refusals,
                appearance: request.appearance,
                reason: request.reason,
            };
        }
    };

    let mut config = FontConfig {
        display_faces: crate::effective_display_faces(&view),
        ..FontConfig::default()
    };
    if let Err(error) = renderer.set_display_faces(&config.display_faces) {
        warnings.push(format!("display_font rejected ({error})"));
        config.display_faces = renderer.display_faces();
    }
    let mut preserve_current_generation = false;
    config.synthetic_style = view.font_synthetic_style.unwrap_or(true);
    renderer.set_synthetic_styles(config.synthetic_style);
    let style_keys = [
        crate::prefs::EDIT_FONT_FAMILY_BOLD,
        crate::prefs::EDIT_FONT_FAMILY_ITALIC,
        crate::prefs::EDIT_FONT_FAMILY_BOLD_ITALIC,
    ];
    for (slot, index) in style_i.into_iter().enumerate() {
        let Some(index) = index else { continue };
        let themed = from_theme(style_keys[slot]).is_some();
        match admitted(index) {
            Some(asset) => match renderer.set_named_styled_face(
                slot,
                &asset.bytes,
                asset.index,
                asset.instance.clone(),
            ) {
                Ok(()) => {
                    // A name that resolved to a face of another style draws
                    // that face; the row says which style rather than "Changed".
                    let target = aterm_render::FaceTarget::for_slot(slot);
                    if let Some(drawn) = target
                        .drawn_style_in(&asset.bytes, asset.index, asset.instance.as_ref())
                        .filter(|drawn| !target.suits_style(*drawn))
                    {
                        font_refusals.push(FontRefusal {
                            key: style_keys[slot],
                            family: names[index].0.clone(),
                            reason: FontRefusalReason::WrongStyle(drawn),
                            from_theme: from_theme(style_keys[slot]),
                        });
                    }
                    config.styled_paths[slot] = Some(asset.face());
                }
                Err(error) => warnings.push(format!("config styled font rejected ({error})")),
            },
            None if themed => warnings.push(theme_miss(
                style_keys[slot],
                &names[index].0,
                "aterm's own choice",
            )),
            None => warnings.push(format!(
                "config styled font {:?} was not admitted",
                names[index].0
            )),
        }
        if !themed
            && config.styled_paths[slot].is_none()
            && request.previous_font_config.styled_paths[slot].is_some()
        {
            preserve_current_generation = true;
        }
    }
    let fallbacks_themed = from_theme("fallback_fonts").is_some();
    let mut first_fallback = true;
    for index in fallback_i {
        let Some(asset) = admitted(index) else {
            warnings.push(if fallbacks_themed {
                theme_miss("fallback_fonts", &names[index].0, "the rest of the list")
            } else {
                format!("config fallback font {:?} was not admitted", names[index].0)
            });
            continue;
        };
        let result = if first_fallback {
            renderer.set_fallback_bytes(&asset.bytes)
        } else {
            renderer.add_fallback_bytes(&asset.bytes)
        };
        match result {
            Ok(()) => {
                first_fallback = false;
                config.fallback_fonts.push(asset.path);
            }
            Err(error) => warnings.push(format!("config fallback font rejected ({error})")),
        }
    }
    if !fallbacks_themed
        && !fallbacks.is_empty()
        && config.fallback_fonts.is_empty()
        && !request.previous_font_config.fallback_fonts.is_empty()
    {
        preserve_current_generation = true;
    }
    let symbol_themed = from_theme("symbol_font").is_some();
    if let Some(asset) = symbol_i.and_then(admitted) {
        match renderer.set_symbol_fallback_bytes(&asset.bytes) {
            Ok(()) => config.symbol_font = Some(asset.path),
            Err(error) => warnings.push(format!("config symbol_font rejected ({error})")),
        }
    } else if let Some(index) = symbol_i {
        warnings.push(if symbol_themed {
            theme_miss("symbol_font", &names[index].0, "aterm's own choice")
        } else {
            format!("config symbol_font {:?} was not admitted", names[index].0)
        });
    }
    if !symbol_themed
        && symbol_i.is_some()
        && config.symbol_font.is_none()
        && request.previous_font_config.symbol_font.is_some()
    {
        preserve_current_generation = true;
    }
    let emoji_themed = from_theme("emoji_font").is_some();
    if let Some(asset) = emoji_i.and_then(admitted) {
        match renderer.set_color_font_arc(asset.bytes) {
            Ok(()) => config.emoji_font = Some(asset.path),
            Err(error) => warnings.push(format!("config emoji_font rejected ({error})")),
        }
    } else if let Some(index) = emoji_i {
        warnings.push(if emoji_themed {
            theme_miss("emoji_font", &names[index].0, "aterm's own choice")
        } else {
            format!("config emoji_font {:?} was not admitted", names[index].0)
        });
    }
    if !emoji_themed
        && emoji_i.is_some()
        && config.emoji_font.is_none()
        && request.previous_font_config.emoji_font.is_some()
    {
        preserve_current_generation = true;
    }
    let (variations, _) = view.font_variation_requests();
    let dark_nudge = view.font_weight_dark_nudge_or_default();
    renderer.set_font_variations(&variations, dark_nudge);
    let sources = renderer.seal_admitted_font_sources();
    let install_required = family != request.previous_family
        || config != request.previous_font_config
        || sources != request.previous_sources
        || variations != request.previous_variations
        || (dark_nudge - request.previous_dark_nudge).abs() > f32::EPSILON;
    let chrome_primary = renderer.chrome_primary_face();
    let chrome_bold = renderer.chrome_bold_face();
    let semantic = renderer.fork_semantic_surface(request.px, theme);
    let chrome = crate::tray_raster::prepare_chrome_fonts(chrome_primary, chrome_bold, semantic);

    Completion {
        sequence: request.sequence,
        theme_generation: request.theme_generation,
        observation,
        config: source_config,
        values,
        disk_faults,
        assets,
        path_feed_fps,
        sparkle,
        fonts: (!preserve_current_generation && install_required).then_some(PreparedFonts {
            renderer,
            family,
            config,
            chrome,
        }),
        warnings,
        font_refusals,
        appearance: request.appearance,
        reason: request.reason,
    }
}

/// Main-thread decision for one completed immutable font generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionDisposition {
    Publish,
    RejectStaleConfig,
    ReprepareLatestTheme,
}

pub(crate) fn completion_disposition(
    requested: u64,
    completed: u64,
    current_theme: u64,
    completed_theme: u64,
    current_appearance: aterm_types::Appearance,
    completed_appearance: aterm_types::Appearance,
) -> CompletionDisposition {
    if requested != completed {
        CompletionDisposition::RejectStaleConfig
    } else if current_theme != completed_theme || current_appearance != completed_appearance {
        CompletionDisposition::ReprepareLatestTheme
    } else {
        CompletionDisposition::Publish
    }
}

#[cfg(test)]
#[path = "native_font_catalog_tests.rs"]
mod tests;
