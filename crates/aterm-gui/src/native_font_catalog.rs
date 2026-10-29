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
mod tests {
    use std::sync::Arc;

    use super::{
        Completion, CompletionDisposition, FontRefusal, FontRefusalReason, Lane, PrimarySeed,
        Request, completion_disposition, prepare,
    };

    const DARK: aterm_types::Appearance = aterm_types::Appearance::Dark;

    fn request(sequence: u64) -> Request {
        request_with_text(sequence, "")
    }

    fn request_with_text(sequence: u64, text: &str) -> Request {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-font-lane-")
            .tempdir()
            .unwrap();
        let path = dir.path().join("aterm.toml");
        std::fs::write(&path, text).unwrap();
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .unwrap();
        let prepared = crate::native_config_service::VersionedConfigService::prepare_observation(
            observation,
            crate::app_config::ThemeCatalog::empty(),
        )
        .unwrap();
        Request {
            sequence,
            theme_generation: 0,
            prepared,
            px: 12.0,
            appearance: aterm_types::Appearance::Dark,
            previous_family: None,
            previous_font_config: crate::app_config::FontConfig::default(),
            previous_sources: {
                let mut renderer = aterm_render::Renderer::from_bytes(
                    include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf"),
                    12.0,
                    aterm_render::Theme::default(),
                )
                .unwrap();
                renderer.seal_admitted_font_sources()
            },
            previous_variations: Vec::new(),
            previous_dark_nudge: 0.0,
            primary: PrimarySeed {
                face: None,
                bytes: Arc::from(
                    include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf").as_slice(),
                ),
            },
            reason: crate::app_colorscheme::ThemeChange::ConfigReload,
        }
    }

    #[test]
    fn an_unknown_font_name_is_refused_on_its_own_row() {
        let completion = prepare(request_with_text(
            1,
            "font_family = \"NoSuchFontXYZ\"\nemoji_font = \"NoSuchEmoji\"\n",
        ));
        let refused = completion
            .font_refusals
            .iter()
            .map(|refusal| (refusal.key, refusal.family.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            refused,
            [
                (crate::prefs::EDIT_FONT_FAMILY, "NoSuchFontXYZ"),
                (crate::prefs::EDIT_EMOJI_FONT, "NoSuchEmoji"),
            ]
        );
        assert!(
            completion
                .warnings
                .iter()
                .any(|warning| warning.contains("emoji_font \"NoSuchEmoji\"")),
            "{:?}",
            completion.warnings
        );
        let display = prepare(request_with_text(
            2,
            "font_family = \"NoSuchFontXYZ\"\ndisplay_font = \"pixel\"\n",
        ));
        assert_eq!(
            display
                .font_refusals
                .iter()
                .map(|refusal| refusal.key)
                .collect::<Vec<_>>(),
            [crate::prefs::EDIT_FONT_FAMILY],
        );
    }

    #[test]
    fn a_display_face_is_drawn_over_the_font_family_never_in_its_place() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-font-lane-layer-")
            .tempdir()
            .unwrap();
        let font = dir.path().join("Mono.ttf");
        std::fs::write(
            &font,
            include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Regular.ttf"),
        )
        .unwrap();
        let family = crate::app_config::toml_basic_string(&font.to_string_lossy());
        let chrome = |completion: &Completion| {
            let fonts = completion.fonts.as_ref().expect("a generation installs");
            (
                fonts
                    .renderer
                    .chrome_primary_face()
                    .map(|(bytes, index)| (bytes.to_vec(), index)),
                fonts.renderer.cell_size(),
                fonts.renderer.primary_source_path().map(str::to_string),
            )
        };
        let plain = prepare(request_with_text(1, &format!("font_family = {family}\n")));
        for faces in ["engraved", "pixel+engraved", "chunky+pixel+bubble"] {
            let layered = prepare(request_with_text(
                2,
                &format!("font_family = {family}\ndisplay_font = \"{faces}\"\n"),
            ));
            assert_eq!(chrome(&layered), chrome(&plain), "{faces}");
            let fonts = layered.fonts.as_ref().unwrap();
            assert_eq!(
                fonts.config.display_faces,
                aterm_render::display_layer_ids(faces.split('+')),
                "{faces}"
            );
            assert_eq!(fonts.renderer.display_faces(), fonts.config.display_faces);
            assert!(layered.font_refusals.is_empty(), "{faces}");
        }
        assert_eq!(
            chrome(&plain).0.map(|(bytes, _)| bytes),
            Some(
                include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Regular.ttf")
                    .to_vec()
            ),
            "the chrome is the person's font"
        );
        let automatic = prepare(request_with_text(1, "display_font = \"engraved\"\n"));
        assert!(
            automatic.fonts.as_ref().is_none_or(|fonts| {
                fonts.renderer.display_faces() == ["engraved"]
                    && fonts
                        .renderer
                        .chrome_primary_face()
                        .is_some_and(|(bytes, _)| {
                            bytes.as_ref() != aterm_render::display_face_bytes("engraved").unwrap()
                        })
            }),
            "the automatic font stays the primary under the layer"
        );
    }

    #[test]
    fn a_style_row_loads_its_own_face_of_a_collection_and_names_a_wrong_style() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-font-lane-faces-")
            .tempdir()
            .unwrap();
        let collection = dir.path().join("Mono.ttc");
        std::fs::write(
            &collection,
            aterm_render::font_catalog::ttc_of(&[
                include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Regular.ttf"),
                include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Bold.ttf"),
                include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Italic.ttf"),
            ]),
        )
        .unwrap();
        let bold = dir.path().join("Bold.ttf");
        std::fs::write(
            &bold,
            include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Bold.ttf"),
        )
        .unwrap();
        let toml = format!(
            "font_family_bold = {}\nfont_family_italic = {}\n",
            crate::app_config::toml_basic_string(&collection.to_string_lossy()),
            crate::app_config::toml_basic_string(&bold.to_string_lossy()),
        );
        let completion = prepare(request_with_text(1, &toml));
        let fonts = completion.fonts.expect("a styled face installs");
        assert_eq!(
            fonts.config.styled_paths[0],
            Some(aterm_render::FaceRef::new(collection.to_string_lossy(), 1)),
            "the bold row takes the collection's Bold, face 1"
        );
        assert_eq!(
            completion.font_refusals,
            [FontRefusal {
                key: crate::prefs::EDIT_FONT_FAMILY_ITALIC,
                family: bold.to_string_lossy().into_owned(),
                reason: FontRefusalReason::WrongStyle(aterm_render::DrawnStyle {
                    bold: true,
                    slanted: false,
                }),
                from_theme: None,
            }],
            "a bold face in the italic row is named"
        );
    }

    #[test]
    fn ui_request_lane_is_bounded_and_retains_latest_generation() {
        let (mut lane, receiver) = Lane::test_pair();
        lane.request(request(1));
        lane.request(request(2));
        lane.request(request(3));
        assert_eq!(receiver.recv().unwrap().sequence, 1);
        lane.worker_drained();
        assert_eq!(receiver.recv().unwrap().sequence, 3);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn shipping_stale_guard_conforms_to_font_generation_model() {
        let model = aterm_models_misc::font_catalog_generation_model();
        let mut state = model.init_state();
        for action in ["RequestFirst", "RequestSecond", "CompleteFirst"] {
            assert!(model.fire(action, &mut state));
        }
        let requested = state[&"requested"] as u64;
        let stale = state[&"completed"] as u64;
        assert_eq!(
            completion_disposition(requested, stale, 0, 0, DARK, DARK),
            CompletionDisposition::RejectStaleConfig
        );
        assert!(model.fire("RejectStale", &mut state));
        assert!(model.fire("CompleteSecond", &mut state));
        let current = state[&"completed"] as u64;
        assert_eq!(
            completion_disposition(requested, current, 0, 0, DARK, DARK),
            CompletionDisposition::Publish
        );
    }

    fn theme_text(bg: &str, extra: &str) -> String {
        format!("foreground = #e0e0e0\nbackground = {bg}\n{extra}\n")
    }

    fn work_catalog(
        dark: (&str, &str),
        light: (&str, &str),
    ) -> Arc<crate::app_config::ThemeCatalog> {
        let parse = |(bg, extra): (&str, &str)| {
            aterm_types::scheme::parse_theme_str(&theme_text(bg, extra)).expect("theme parses")
        };
        crate::app_config::ThemeCatalog::from_parsed([
            ("Work".to_string(), parse(dark)),
            ("Work Day".to_string(), parse(light)),
        ])
    }

    const SPLIT: &str = "theme = \"dark:Work,light:Work Day\"\n";

    struct ThemeTrace {
        /// `themes[1]` is the catalog a theme change admits (its colours are
        /// theme generation 1's).
        themes: [Arc<crate::app_config::ThemeCatalog>; 2],
        prepared: Vec<(i64, Arc<crate::app_config::ConfigAssetCatalog>, i64)>,
        completed_theme: i64,
        completed_side: i64,
        base_sequence: u64,
        base_theme: u64,
        /// The ghost `degraded`: set when the trace drives a refused worker.
        degraded: i64,
    }

    fn side(appearance: aterm_types::Appearance) -> i64 {
        i64::from(appearance == aterm_types::Appearance::Light)
    }

    impl ThemeTrace {
        fn project(
            &mut self,
            app: &crate::App,
            in_hand: Option<&Completion>,
        ) -> Result<aterm_spec::interp::State, String> {
            let generation = |n: u64, base: u64| {
                i64::try_from(n.saturating_sub(base)).expect("a bounded generation")
            };
            let held_assets = app.native_config_service.snapshot().assets;
            let (published, published_side) = self
                .prepared
                .iter()
                .find(|(_, assets, _)| Arc::ptr_eq(assets, &held_assets))
                .map_or((0, 0), |(sequence, _, side)| (*sequence, *side));
            let published_theme = if published == 0 {
                0
            } else {
                i64::from(Arc::ptr_eq(&self.themes[1], &held_assets.themes))
            };
            let installed = &app.installed_colors;
            let gen1 = self.themes[1]
                .resolve(&installed.id)
                .is_ok_and(|scheme| scheme == installed.scheme);
            Ok(std::collections::BTreeMap::from([
                (
                    "requested",
                    generation(app.requested_font_catalog_sequence, self.base_sequence),
                ),
                (
                    "completed",
                    in_hand.map_or(0, |c| generation(c.sequence, self.base_sequence)),
                ),
                ("published", published),
                (
                    "theme",
                    generation(app.theme_catalog_generation, self.base_theme),
                ),
                ("completed_theme", self.completed_theme),
                ("published_theme", published_theme),
                ("side", side(app.os_appearance)),
                ("completed_side", self.completed_side),
                ("published_side", published_side),
                ("colour_theme", i64::from(gen1)),
                ("colour_side", side(installed.side)),
                ("held", i64::from(app.held_theme_change.is_some())),
                ("degraded", self.degraded),
            ]))
        }

        fn complete(&mut self, request: Request) -> Completion {
            let completion = prepare(request);
            self.completed_theme =
                i64::try_from(completion.theme_generation.saturating_sub(self.base_theme))
                    .expect("bounded");
            self.completed_side = side(completion.appearance);
            self.prepared.push((
                i64::try_from(completion.sequence.saturating_sub(self.base_sequence))
                    .expect("bounded"),
                Arc::clone(&completion.assets),
                side(completion.appearance),
            ));
            completion
        }
    }

    type FinishFn = fn(&mut crate::App, Completion);

    fn shipped_finish(app: &mut crate::App, completion: Completion) {
        app.finish_font_catalog_generation(completion);
    }

    fn sequence_only_finish(app: &mut crate::App, completion: Completion) {
        if completion.sequence == app.requested_font_catalog_sequence {
            app.finished_font_catalog_sequence = completion.sequence;
            app.apply_prepared_config_generation(completion.into_generation());
        }
    }

    fn colour_first_finish(app: &mut crate::App, completion: Completion) {
        if let Some(reason) = app.held_theme_change.take() {
            app.install_resolved_colors(reason);
        }
        app.finish_font_catalog_generation(completion);
    }

    fn split_app(
        catalog: &Arc<crate::app_config::ThemeCatalog>,
    ) -> (crate::App, std::sync::mpsc::Receiver<Request>) {
        let mut app = crate::App::headless_for_test();
        let (lane, requests) = Lane::test_pair();
        app.native_font_catalog = Some(lane);
        app.reload_theme_catalog(Arc::clone(catalog));
        app.reload_prepared_config_observation(request_with_text(80, SPLIT).prepared);
        let setup = requests.try_recv().expect("the split config is prepared");
        app.finish_font_catalog_generation(prepare(setup));
        assert_eq!(
            app.config.theme.as_deref(),
            Some("dark:Work,light:Work Day")
        );
        (app, requests)
    }

    fn check_step(
        model: &aterm_spec::derive::Model,
        action: &str,
        state: &mut aterm_spec::interp::State,
        after: aterm_spec::interp::State,
        visited: &mut std::collections::BTreeSet<aterm_spec::interp::State>,
    ) -> Result<(), String> {
        let expected = model.successors(action, state);
        if expected != vec![after.clone()] {
            let broken: Vec<&str> = model
                .invariants
                .iter()
                .map(|inv| inv.name)
                .filter(|name| !model.check_invariant(name, &after))
                .collect();
            return Err(format!(
                "real {action} took {state:?} to {after:?} (breaks {broken:?}), the model's \
                 {action} to {expected:?}"
            ));
        }
        visited.insert(after.clone());
        *state = after;
        Ok(())
    }

    fn font_theme_trace(
        model: &aterm_spec::derive::Model,
        finish: FinishFn,
    ) -> Result<std::collections::BTreeSet<aterm_spec::interp::State>, String> {
        let gen0 = work_catalog(("#101418", ""), ("#f4f4f0", ""));
        let gen1 = work_catalog(
            ("#141018", "line_height = 1.2"),
            ("#f0f4f4", "line_height = 1.3"),
        );
        let (mut app, requests) = split_app(&gen0);
        let mut trace = ThemeTrace {
            themes: [gen0, gen1],
            prepared: Vec::new(),
            completed_theme: 0,
            completed_side: 0,
            base_sequence: app.requested_font_catalog_sequence,
            base_theme: app.theme_catalog_generation,
            degraded: 0,
        };
        let mut state = trace.project(&app, None)?;
        if state != model.init_state() {
            return Err(format!("the App starts at {state:?}, the model at Init"));
        }
        let mut visited = std::collections::BTreeSet::from([state.clone()]);

        app.reload_prepared_config_observation(
            request_with_text(90, &format!("{SPLIT}trail_sounds = false\n")).prepared,
        );
        let after = trace.project(&app, None)?;
        check_step(model, "RequestConfig", &mut state, after, &mut visited)?;
        app.reload_theme_catalog(Arc::clone(&trace.themes[1]));
        if !Arc::ptr_eq(&app.config_assets.themes, &trace.themes[1]) {
            trace.themes[1] = Arc::clone(&app.config_assets.themes);
        }
        let after = trace.project(&app, None)?;
        check_step(model, "ThemeChanged", &mut state, after, &mut visited)?;
        app.sync_app_theme_to_appearance(aterm_types::Appearance::Light);
        let after = trace.project(&app, None)?;
        check_step(model, "OsFlip", &mut state, after, &mut visited)?;
        let old = requests
            .try_recv()
            .map_err(|e| format!("no queued request: {e}"))?;
        let old = trace.complete(old);
        let after = trace.project(&app, Some(&old))?;
        check_step(model, "CompleteOld", &mut state, after, &mut visited)?;
        finish(&mut app, old);
        let after = trace.project(&app, None)?;
        check_step(
            model,
            "ReprepareLatestTheme",
            &mut state,
            after,
            &mut visited,
        )?;
        let latest = requests
            .try_recv()
            .map_err(|e| format!("the re-preparation was not queued: {e}"))?;
        for request in [latest.clone(), latest] {
            let completion = trace.complete(request);
            let after = trace.project(&app, Some(&completion))?;
            check_step(model, "CompleteLatest", &mut state, after, &mut visited)?;
            finish(&mut app, completion);
            let after = trace.project(&app, None)?;
            check_step(model, "PublishCurrent", &mut state, after, &mut visited)?;
        }
        Ok(visited)
    }

    fn refused_worker_trace(
        model: &aterm_spec::derive::Model,
    ) -> Result<std::collections::BTreeSet<aterm_spec::interp::State>, String> {
        let catalog = work_catalog(("#101418", ""), ("#f4f4f0", "line_height = 1.3"));
        let (mut app, _requests) = split_app(&catalog);
        let mut trace = ThemeTrace {
            themes: [
                Arc::clone(&catalog),
                work_catalog(("#000001", ""), ("#000002", "")),
            ],
            prepared: Vec::new(),
            completed_theme: 0,
            completed_side: 0,
            base_sequence: app.requested_font_catalog_sequence,
            base_theme: app.theme_catalog_generation,
            degraded: 0,
        };
        let mut state = trace.project(&app, None)?;
        if state != model.init_state() {
            return Err(format!("the App starts at {state:?}, the model at Init"));
        }
        let mut visited = std::collections::BTreeSet::from([state.clone()]);
        app.font_worker_refuses_for_test = true;
        app.sync_app_theme_to_appearance(aterm_types::Appearance::Light);
        trace.degraded = 1;
        let after = trace.project(&app, None)?;
        check_step(model, "WorkerRefused", &mut state, after, &mut visited)?;
        Ok(visited)
    }

    #[test]
    fn overtaking_theme_catalog_conforms_to_font_theme_generation_model() {
        let model = aterm_models_misc::font_theme_generation_model();
        let mut visited =
            font_theme_trace(&model, shipped_finish).unwrap_or_else(|why| panic!("{why}"));
        visited.extend(refused_worker_trace(&model).unwrap_or_else(|why| panic!("{why}")));
        assert_eq!(
            Ok(visited.len()),
            aterm_spec::interp::bmc(&model),
            "the real traces must visit every state the model reaches"
        );

        let stale = font_theme_trace(&model, sequence_only_finish)
            .expect_err("the model must refuse a stale-face publication");
        assert!(
            stale.contains("real ReprepareLatestTheme"),
            "the refusal must land where the stale generation publishes: {stale}"
        );
        let early = font_theme_trace(&model, colour_first_finish)
            .expect_err("the model must refuse colours installed before their face");
        assert!(
            early.contains("real ReprepareLatestTheme") && early.contains("FaceMatchesColours"),
            "{early}"
        );
        for dial in [1, 2] {
            let buggy = aterm_spec::interp::with_buggy(&model, dial);
            font_theme_trace(&buggy, shipped_finish)
                .expect_err("the shipped App must not conform to a mutant");
        }
        let lost = aterm_spec::interp::with_buggy(&model, 3);
        refused_worker_trace(&lost).expect_err("the shipped App never loses a flip");
    }

    fn request_with_themes(
        sequence: u64,
        text: &str,
        themes: &Arc<crate::app_config::ThemeCatalog>,
    ) -> Request {
        let mut request = request_with_text(sequence, text);
        let mut assets = (*request.prepared.assets).clone();
        assets.themes = Arc::clone(themes);
        request.prepared.assets = Arc::new(assets);
        request
    }

    fn studio(extra: &str) -> Arc<crate::app_config::ThemeCatalog> {
        let parsed = aterm_types::scheme::parse_theme_str(&theme_text("#101418", extra))
            .expect("theme parses");
        crate::app_config::ThemeCatalog::from_parsed([("Studio".to_string(), parsed)])
    }

    #[test]
    fn worker_applies_theme_family() {
        let iosevka = aterm_render::bundled::IOSEVKA_ID;
        let themes = studio(&format!("font_family = {iosevka}"));
        let themed = prepare(request_with_themes(1, "theme = \"Studio\"\n", &themes));
        let fonts = themed.fonts.expect("a new face");
        assert_eq!(fonts.family.as_deref(), Some(iosevka));
        assert!(fonts.config.display_faces.is_empty());
        assert!(
            themed.config.font_family.is_none(),
            "the authored config is kept"
        );
        let bold = aterm_render::bundled::IOSEVKA_BOLD_ID;
        let mine = prepare(request_with_themes(
            2,
            &format!("theme = \"Studio\"\nfont_family = \"{bold}\"\n"),
            &themes,
        ));
        assert_eq!(
            mine.fonts.expect("a new face").family.as_deref(),
            Some(bold)
        );
        let layered = prepare(request_with_themes(
            3,
            "theme = \"Studio\"\n",
            &studio("font_family = display:pixel"),
        ));
        let fonts = layered.fonts.expect("a new layer");
        assert_eq!(fonts.family, None, "the default font stays the font");
        assert_eq!(fonts.config.display_faces, ["pixel"]);
    }

    #[test]
    fn theme_refusal_takes_next_rung() {
        let themes = studio("font_family = Zz No Such Font Aterm");
        let mut request = request_with_themes(1, "theme = \"Studio\"\n", &themes);
        request.previous_family = Some("display:pixel".to_string());
        let completion = prepare(request);
        let family = completion.fonts.as_ref().and_then(|f| f.family.clone());
        assert_ne!(family.as_deref(), Some("display:pixel"), "no keep-current");
        assert!(
            completion
                .warnings
                .iter()
                .any(|w| w.starts_with(super::THEME_FAMILY_REFUSED)),
            "{:?}",
            completion.warnings
        );
        assert!(
            !completion
                .warnings
                .iter()
                .any(|w| w.starts_with(super::FAMILY_REFUSED)),
            "a theme miss never raises the config verdict: {:?}",
            completion.warnings
        );
        assert_eq!(
            completion.font_refusals[0].from_theme.as_deref(),
            Some("Studio")
        );

        let mut mine = request_with_themes(
            2,
            "font_family = \"Zz No Such Font Aterm\"\n",
            &crate::app_config::ThemeCatalog::empty(),
        );
        let iosevka = aterm_render::bundled::IOSEVKA_ID;
        mine.previous_family = Some(iosevka.to_string());
        let mine = prepare(mine);
        let kept = mine
            .fonts
            .as_ref()
            .and_then(|f| f.family.clone())
            .or_else(|| (mine.fonts.is_none()).then(|| iosevka.to_string()));
        assert_eq!(kept.as_deref(), Some(iosevka));
        assert!(
            mine.warnings
                .iter()
                .any(|w| w.starts_with(super::FAMILY_REFUSED))
        );
        assert_eq!(mine.font_refusals[0].from_theme, None);
    }

    #[test]
    fn preserve_cliff_matrix() {
        let symbol = aterm_render::bundled::IOSEVKA_ITALIC_ID;
        let previous_symbol = Some(symbol.to_string());
        for themed in [true, false] {
            for (name, admits) in [(symbol, true), ("Zz No Such Symbol Aterm", false)] {
                for previous in [previous_symbol.clone(), None] {
                    let (theme_line, config_line) = if themed {
                        (format!("symbol_font = {name}"), String::new())
                    } else {
                        (String::new(), format!("symbol_font = {name:?}\n"))
                    };
                    let themes = studio(&theme_line);
                    let text = format!(
                        "theme = \"Studio\"\nfont_family = \"{}\"\n{config_line}",
                        aterm_render::bundled::IOSEVKA_ID
                    );
                    let mut request = request_with_themes(1, &text, &themes);
                    request
                        .previous_font_config
                        .symbol_font
                        .clone_from(&previous);
                    let completion = prepare(request);
                    let preserved = completion.fonts.is_none();
                    let expect = !themed && !admits && previous.is_some();
                    assert_eq!(
                        preserved, expect,
                        "themed={themed} admits={admits} previous={previous:?}: {:?}",
                        completion.warnings
                    );
                }
            }
        }
    }

    #[test]
    fn disposition_appearance() {
        use aterm_types::Appearance::{Dark, Light};
        assert_eq!(
            completion_disposition(1, 1, 0, 0, Light, Dark),
            CompletionDisposition::ReprepareLatestTheme
        );
        assert_eq!(
            completion_disposition(1, 1, 0, 1, Light, Light),
            CompletionDisposition::ReprepareLatestTheme
        );
        assert_eq!(
            completion_disposition(1, 1, 0, 0, Light, Light),
            CompletionDisposition::Publish
        );
    }

    fn app_with(
        themes: &Arc<crate::app_config::ThemeCatalog>,
        text: &str,
    ) -> (crate::App, std::sync::mpsc::Receiver<Request>) {
        let mut app = crate::App::headless_for_test();
        let (lane, requests) = Lane::test_pair();
        app.native_font_catalog = Some(lane);
        app.reload_theme_catalog(Arc::clone(themes));
        app.reload_prepared_config_observation(request_with_text(70, text).prepared);
        let setup = requests.try_recv().expect("the config is prepared");
        app.finish_font_catalog_generation(prepare(setup));
        (app, requests)
    }

    #[test]
    fn reload_applies_theme_typography() {
        let (app, _requests) = app_with(&studio("line_height = 1.2"), "theme = \"Studio\"\n");
        assert_eq!(
            app.render_knobs.line_height,
            crate::resolved_typography::milli_f32(1200)
        );
        let (mine, _requests) = app_with(
            &studio("line_height = 1.2"),
            "theme = \"Studio\"\nline_height = 1.0\n",
        );
        assert_eq!(mine.render_knobs.line_height, 1.0);

        let (mut app, requests) = app_with(&studio("line_height = 1.2"), "theme = \"Studio\"\n");
        app.reload_theme_catalog(studio("line_height = 1.4"));
        let request = requests.try_recv().expect("a face refresh is prepared");
        let completion = prepare(request);
        let old_gate = completion.config == app.config && completion.fonts.is_none();
        assert!(
            old_gate,
            "the pre-typography dedupe would have stopped here"
        );
        app.finish_font_catalog_generation(completion);
        assert_eq!(
            app.render_knobs.line_height,
            crate::resolved_typography::milli_f32(1400)
        );
    }

    #[test]
    fn catalog_reload_with_face_change_holds_colours() {
        let (mut app, requests) = app_with(&studio(""), "theme = \"Studio\"\n");
        let before = app.installed_colors.scheme.clone();
        app.theme_preview = Some(crate::control_theme::ThemePreview {
            id: "Studio".to_string(),
            restore: "Studio".to_string(),
            hold: crate::control_theme::PreviewHold::Settings { keeping: false },
            commit_request: None,
            generation: 0,
        });
        let next = Arc::new(
            (*crate::app_config::ThemeCatalog::from_parsed([(
                "Studio".to_string(),
                aterm_types::scheme::parse_theme_str(&theme_text("#201418", "line_height = 1.2"))
                    .expect("parses"),
            )]))
            .clone(),
        );
        app.reload_theme_catalog(Arc::clone(&next));
        let request = requests.try_recv().expect("one face refresh is queued");
        assert_eq!(
            request.reason,
            crate::app_colorscheme::ThemeChange::CatalogReload
        );
        assert!(requests.try_recv().is_err(), "exactly one request");
        assert_eq!(
            app.installed_colors.scheme, before,
            "the swap waits for its face"
        );
        assert!(app.held_theme_change.is_some());
        app.finish_font_catalog_generation(prepare(request));
        assert_ne!(app.installed_colors.scheme, before);
        assert_eq!(
            app.render_knobs.line_height,
            crate::resolved_typography::milli_f32(1200)
        );
        assert!(app.held_theme_change.is_none());
        assert!(
            app.theme_preview.is_some(),
            "a catalog-reload refresh keeps the trial"
        );

        let (mut app, requests) = app_with(&studio(""), "theme = \"Studio\"\n");
        let before = app.installed_colors.scheme.clone();
        app.reload_theme_catalog(Arc::new(
            (*crate::app_config::ThemeCatalog::from_parsed([(
                "Studio".to_string(),
                aterm_types::scheme::parse_theme_str(&theme_text("#201418", "")).expect("parses"),
            )]))
            .clone(),
        ));
        assert!(requests.try_recv().is_err(), "no font work for colours");
        assert_ne!(app.installed_colors.scheme, before, "installed at once");
        assert!(app.held_theme_change.is_none());
    }

    #[test]
    fn os_flip_with_split_typography() {
        let split = |dark: &str, light: &str| work_catalog(("#101418", dark), ("#f4f4f0", light));
        let (mut app, requests) = app_with(&split("line_height = 1.2", ""), SPLIT);
        let before = app.installed_colors.scheme.clone();
        app.sync_app_theme_to_appearance(aterm_types::Appearance::Light);
        let request = requests.try_recv().expect("the light face is prepared");
        assert_eq!(request.appearance, aterm_types::Appearance::Light);
        assert_eq!(request.reason, crate::app_colorscheme::ThemeChange::OsFlip);
        assert_eq!(app.installed_colors.scheme, before);
        assert!(app.held_theme_change.is_some());

        let (mut app, requests) = app_with(&split("line_height = 1.2", ""), SPLIT);
        app.font_worker_refuses_for_test = true;
        app.sync_app_theme_to_appearance(aterm_types::Appearance::Light);
        assert!(requests.try_recv().is_err());
        assert_eq!(app.installed_colors.side, aterm_types::Appearance::Light);
        assert!(
            app.held_theme_change.is_none(),
            "a refused worker holds nothing"
        );

        let (mut app, requests) = app_with(&split("line_height = 1.2", "line_height = 1.2"), SPLIT);
        app.sync_app_theme_to_appearance(aterm_types::Appearance::Light);
        assert!(requests.try_recv().is_err(), "the same face on both sides");
        assert_eq!(app.installed_colors.side, aterm_types::Appearance::Light);
    }

    #[test]
    fn a_refused_font_request_does_not_hold_a_later_theme_swap() {
        let themes = work_catalog(("#101418", "line_height = 1.2"), ("#f4f4f0", ""));
        let (mut app, requests) = app_with(&themes, SPLIT);
        let before = app.installed_colors.scheme.clone();
        assert_eq!(
            app.finished_font_catalog_sequence, app.requested_font_catalog_sequence,
            "the setup generation actually finished"
        );
        app.reload_prepared_config_observation(request_with_text(71, SPLIT).prepared);
        let request = requests
            .try_recv()
            .expect("a live worker receives the reload");
        app.finish_font_catalog_generation(prepare(request));
        assert_eq!(
            app.finished_font_catalog_sequence,
            app.requested_font_catalog_sequence
        );
        assert_eq!(app.installed_colors.scheme, before);
        drop(requests);
        let completed = app.finished_font_catalog_sequence;
        app.reload_prepared_config_observation(request_with_text(72, SPLIT).prepared);
        let refused_ticket = app.requested_font_catalog_sequence;
        assert!(
            refused_ticket > completed,
            "the refused enqueue allocated a newer ticket"
        );
        let refused_ticket_settled = app.finished_font_catalog_sequence == refused_ticket;
        app.sync_app_theme_to_appearance(aterm_types::Appearance::Light);
        assert_eq!(
            app.os_appearance,
            aterm_types::Appearance::Light,
            "the appearance event reached the reducer"
        );
        assert_eq!(
            (
                app.installed_colors.side,
                app.held_theme_change.is_none(),
                refused_ticket_settled
            ),
            (aterm_types::Appearance::Light, true, true),
            "a refused request has no future completion to release a colour hold",
        );
    }

    #[test]
    fn font_size_scale_scales_the_derived_size() {
        let (app, _requests) = app_with(&studio(""), "font_px = 20.0\nfont_size_scale = 1.1\n");
        assert_eq!(app.default_font_size, 22.0);
        assert_eq!(app.font_size_scale_milli, 1100);
        let (app, _requests) = app_with(
            &studio("font_size_scale = 1.1"),
            "theme = \"Studio\"\nfont_px = 20.0\n",
        );
        assert_eq!(app.default_font_size, 22.0, "a theme's scale too");
        let (app, _requests) = app_with(&studio(""), "font_px = 20.0\nfont_size_scale = 1.0\n");
        assert_eq!(app.default_font_size, 20.0);
    }

    #[test]
    fn a_configured_size_draws_at_the_window_scale() {
        let at_2x = |text: &str| {
            let mut app = crate::App::headless_for_test();
            let wid = crate::WindowId(0);
            app.windows.get_mut(&wid).expect("window 0").scale = 2.0;
            app.refresh_all_window_metrics();
            let (lane, requests) = Lane::test_pair();
            app.native_font_catalog = Some(lane);
            app.reload_prepared_config_observation(request_with_text(70, text).prepared);
            if let Ok(setup) = requests.try_recv() {
                app.finish_font_catalog_generation(prepare(setup));
            }
            (app.font_size, app.windows[&wid].metrics.font_px)
        };
        let default = at_2x("");
        assert_eq!(default, (crate::FONT_PX, (crate::FONT_PX * 2.0).round()));
        assert_eq!(at_2x("font_px = 13.0\n"), (13.0, 26.0));
        assert!(
            at_2x(&format!("font_px = {:.1}\n", crate::FONT_PX + 1.0)).1 > default.1,
            "one step up grows the text"
        );
        assert_eq!(
            at_2x(&format!("font_px = {:.1}\n", crate::FONT_PX)).1,
            default.1,
            "the default size named is the default"
        );
    }

    #[test]
    fn unrelated_and_comment_only_generations_do_not_install_fonts() {
        let comment_only = prepare(request(10));
        assert!(
            comment_only.fonts.is_none(),
            "semantic no-op text must publish service state without replacing the renderer"
        );

        let unrelated = prepare(request_with_text(11, "trail_sounds = false\n"));
        assert!(
            unrelated.fonts.is_none(),
            "an unrelated effects toggle must not replace/regrid an identical font generation"
        );
    }

    #[test]
    fn prepared_catalog_carries_exact_admitted_toy_pack_consumers() {
        let pack = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../aterm-effects/toy-packs/community/tiny-triumphs/pack.toml");
        let request = request_with_text(
            12,
            &format!(
                "[sparkle_words]\ntoy_packs = [{:?}]\n",
                pack.to_string_lossy()
            ),
        );
        let completion = prepare(request);
        let consumers = *completion
            .assets
            .sparkle_spec_consumers
            .as_deref()
            .expect("prepared catalogs always carry an authoritative projection");
        assert!(consumers.sparkle_or_starburst_burst);
        assert!(consumers.rainbow_ink);
        assert!(consumers.twotone_ink);
        assert!(!consumers.nova_burst);
        assert!(!consumers.nova_twotone_ink);
        assert_eq!(
            consumers,
            completion.sparkle.consumer_capabilities(),
            "Settings catalog and installed runtime share one compiled spec generation"
        );
    }

    #[test]
    fn prepared_path_feeds_are_not_reopened_by_font_preparation() {
        let dir =
            std::env::temp_dir().join(format!("aterm-font-prepared-feeds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let lexicon = dir.join("lexicon.toml");
        let v1 = "# font-worker-exact\n";
        let v2 = "# font-worker-replacement\n";
        std::fs::write(&lexicon, v1).unwrap();
        let text = format!(
            "[sparkle_words]\nlexicon = {:?}\n",
            lexicon.to_string_lossy(),
        );

        let open_probe = aterm_effects::file_feed::probe_open_attempts(&lexicon);
        crate::app_config::reset_path_feed_read_count();
        let request = request_with_text(13, &text);
        assert_eq!(crate::app_config::path_feed_read_count(), 1);
        assert_eq!(
            open_probe.attempts(),
            1,
            "the genuine config worker opens the one active lexicon exactly once"
        );

        std::fs::write(&lexicon, v2).unwrap();
        std::fs::write(&lexicon, v1).unwrap();
        crate::app_config::reset_path_feed_read_count();
        let _completion = prepare(request);
        assert_eq!(
            crate::app_config::path_feed_read_count(),
            0,
            "font/theme work must consume the carried generation without reopening feeds"
        );
        assert_eq!(open_probe.attempts(), 1);

        let _ = aterm_effects::file_feed::fingerprint_bounded_regular_utf8(
            &lexicon,
            aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES,
        )
        .unwrap();
        assert_eq!(
            open_probe.attempts(),
            2,
            "negative control: a genuine independent fingerprint reopen reaches the OS seam"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prepared_catalog_distinguishes_observed_no_consumers_from_unobserved() {
        let pack = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../aterm-effects/toy-packs/community/tiny-triumphs/pack.toml");
        let request = request_with_text(
            14,
            &format!(
                "[sparkle_words]\ntoy_packs = [{:?}]\n\
                 [[sparkle_words.custom]]\n\
                 words = [\"shipit\", \"shipped\", \"ultrathink\", \"deepthink\"]\n\
                 graphic = {{ collection = \"cats\" }}\n",
                pack.to_string_lossy()
            ),
        );

        let completion = prepare(request);
        let none = aterm_effects::spec::SpecConsumerCapabilities::default();
        assert_eq!(completion.sparkle.consumer_capabilities(), none);
        assert_eq!(
            completion.assets.sparkle_spec_consumers.as_deref(),
            Some(&none),
            "Some(default) is an exact worker observation; None alone means unobserved"
        );
    }
}
