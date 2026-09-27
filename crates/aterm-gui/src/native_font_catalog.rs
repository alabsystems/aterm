// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Latest-wins off-thread preparation of immutable config font generations.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use winit::event_loop::EventLoopProxy;

use crate::app_config::{Config, ConfigAssetCatalog, FontConfig};
use crate::native_config_service::ConfigDiskObservation;
use crate::{Wake, effective_font_family};

#[derive(Clone)]
pub(crate) struct PrimarySeed {
    pub(crate) path: Option<String>,
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
    pub(crate) assets: Arc<ConfigAssetCatalog>,
    pub(crate) path_feed_fps: crate::app_config::PathFeedFps,
    pub(crate) sparkle: crate::app_config::PreparedSparkleRuntime,
    pub(crate) fonts: Option<PreparedFonts>,
    pub(crate) warnings: Vec<String>,
}

/// Complete immutable config generation consumed by the event-loop reducer.
/// Every host-backed input has already been bounded, read, parsed, and tied to
/// the exact config observation; publication performs no filesystem work.
pub(crate) struct PreparedConfigGeneration {
    pub(crate) observation: ConfigDiskObservation,
    pub(crate) config: Config,
    pub(crate) values: std::collections::BTreeMap<String, String>,
    pub(crate) assets: Arc<ConfigAssetCatalog>,
    pub(crate) path_feed_fps: crate::app_config::PathFeedFps,
    pub(crate) sparkle: crate::app_config::PreparedSparkleRuntime,
    pub(crate) fonts: Option<PreparedFonts>,
    pub(crate) warnings: Vec<String>,
}

impl Completion {
    pub(crate) fn into_generation(self) -> PreparedConfigGeneration {
        PreparedConfigGeneration {
            observation: self.observation,
            config: self.config,
            values: self.values,
            assets: self.assets,
            path_feed_fps: self.path_feed_fps,
            sparkle: self.sparkle,
            fonts: self.fonts,
            warnings: self.warnings,
        }
    }
}

impl std::fmt::Debug for Completion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontCatalogCompletion")
            .field("sequence", &self.sequence)
            .field("theme_generation", &self.theme_generation)
            .field("fonts_ready", &self.fonts.is_some())
            .field("warnings", &self.warnings)
            .finish_non_exhaustive()
    }
}

pub(crate) struct Lane {
    tx: SyncSender<Request>,
    pending: Option<Request>,
}

impl Lane {
    pub(crate) fn spawn(proxy: EventLoopProxy<Wake>) -> Result<Self, String> {
        let (tx, rx) = sync_channel(1);
        std::thread::Builder::new()
            .name("aterm-font-catalog".into())
            .spawn(move || {
                // QoS (port of 61a6c8b62): a font-picker catalogue scan; the
                // picker shows a spinner meanwhile, the terminal is unaffected.
                crate::qos::set_self(crate::qos::Role::Background);
                worker(rx, proxy);
            })
            .map_err(|error| format!("could not start font catalog worker: {error}"))?;
        Ok(Self { tx, pending: None })
    }

    pub(crate) fn request(&mut self, request: Request) {
        self.pending = Some(request);
        self.dispatch();
    }

    pub(crate) fn worker_drained(&mut self) {
        self.dispatch();
    }

    fn dispatch(&mut self) {
        let Some(request) = self.pending.take() else {
            return;
        };
        match self.tx.try_send(request) {
            Ok(()) => {}
            Err(TrySendError::Full(request)) => self.pending = Some(request),
            Err(TrySendError::Disconnected(_)) => self.pending = None,
        }
    }

    #[cfg(test)]
    fn test_pair() -> (Self, Receiver<Request>) {
        let (tx, rx) = sync_channel(1);
        (Self { tx, pending: None }, rx)
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
        assets,
        path_feeds: path_generation,
    } = request.prepared;
    let path_feed_fps = path_generation.fingerprints;
    let sparkle = path_generation.sparkle;
    // The required bundle was composed from this exact path generation by the
    // config worker. Font/theme preparation consumes it literally: no feed
    // reopen, no second lexicon projection, and no representable "assets from
    // A, runtime from B" fallback.
    debug_assert!(Arc::ptr_eq(
        &path_generation.trail_packs,
        &assets.trail_packs
    ));
    debug_assert_eq!(
        assets.sparkle_spec_consumers.as_deref(),
        Some(&sparkle.consumer_capabilities())
    );
    let theme = source_config.theme_for_with_assets(request.appearance, &assets.themes);
    let requested_family = effective_font_family(source_config.font_family_request().as_deref());
    let styles = [
        source_config.font_family_bold.as_deref(),
        source_config.font_family_italic.as_deref(),
        source_config.font_family_bold_italic.as_deref(),
    ];
    let fallbacks = source_config
        .fallback_fonts
        .as_ref()
        .map(|fonts| fonts.0.as_slice())
        .unwrap_or(&[]);

    let mut names = Vec::new();
    let mut push = |value: &str| {
        let index = names.len();
        names.push(value.to_string());
        index
    };
    let primary_i = requested_family.as_deref().map(&mut push);
    let style_i = styles.map(|value| {
        value
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(&mut push)
    });
    let fallback_i = fallbacks
        .iter()
        .map(|value| push(value))
        .collect::<Vec<_>>();
    let symbol_i = source_config
        .symbol_font
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(&mut push);
    let emoji_i = source_config
        .emoji_font
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(&mut push);
    let batch = aterm_render::font_catalog::resolve_and_admit(&names);
    let admitted = |index: usize| {
        batch
            .get(index)
            .and_then(|result| result.as_ref().ok())
            .cloned()
    };
    let mut warnings = Vec::new();

    let (primary_path, primary_bytes, family) = match primary_i.and_then(admitted) {
        Some(asset) => (
            Some(asset.path),
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
                "config font_family: {error}; keeping the current working face"
            ));
            (
                request.primary.path.clone(),
                Arc::clone(&request.primary.bytes),
                request.previous_family.clone(),
            )
        }
        None if request.previous_family.is_some() => {
            let candidates = aterm_render::primary_font_candidate_paths();
            let batch = aterm_render::font_catalog::resolve_and_admit(&candidates);
            match batch
                .entries
                .into_iter()
                .find_map(|entry| entry.result.ok())
            {
                Some(asset) => (
                    Some(asset.path),
                    Arc::<[u8]>::from(asset.bytes.as_slice()),
                    None,
                ),
                None => (
                    request.primary.path.clone(),
                    Arc::clone(&request.primary.bytes),
                    request.previous_family.clone(),
                ),
            }
        }
        None => (
            request.primary.path.clone(),
            Arc::clone(&request.primary.bytes),
            None,
        ),
    };

    let renderer = match primary_path.clone() {
        Some(path) => {
            aterm_render::Renderer::from_resolved_font_file(path, &primary_bytes, request.px, theme)
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
                assets,
                path_feed_fps,
                sparkle,
                fonts: None,
                warnings,
            };
        }
    };

    // FONT-DISPLAY MIX: a `display:<id>+<id>` family carries EXTRA faces beyond the
    // admitted primary (the mix's first face). Installed on the prepared
    // renderer here, so hot reload and the seal/rebuild path (which clones the
    // mix) both carry it; a rejected face degrades to the primary alone.
    if let Some(mix) = family
        .as_deref()
        .and_then(aterm_render::display_face_mix_for_family)
        && mix.len() > 1
        && let Err(error) = renderer.set_display_mix_faces(&mix[1..])
    {
        warnings.push(format!("display face mix rejected ({error})"));
    }

    let mut config = FontConfig::default();
    let mut preserve_current_generation = false;
    config.synthetic_style = source_config.font_synthetic_style.unwrap_or(true);
    renderer.set_synthetic_styles(config.synthetic_style);
    for (slot, index) in style_i.into_iter().enumerate() {
        let Some(index) = index else { continue };
        match admitted(index) {
            Some(asset) => match renderer.set_styled_font_bytes(slot, &asset.bytes) {
                Ok(()) => config.styled_paths[slot] = Some(asset.path),
                Err(error) => warnings.push(format!("config styled font rejected ({error})")),
            },
            None => warnings.push(format!(
                "config styled font {:?} was not admitted",
                names[index]
            )),
        }
        if config.styled_paths[slot].is_none()
            && request.previous_font_config.styled_paths[slot].is_some()
        {
            preserve_current_generation = true;
        }
    }
    let mut first_fallback = true;
    for index in fallback_i {
        let Some(asset) = admitted(index) else {
            warnings.push(format!(
                "config fallback font {:?} was not admitted",
                names[index]
            ));
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
    if !fallbacks.is_empty()
        && config.fallback_fonts.is_empty()
        && !request.previous_font_config.fallback_fonts.is_empty()
    {
        preserve_current_generation = true;
    }
    if let Some(asset) = symbol_i.and_then(admitted) {
        match renderer.set_symbol_fallback_bytes(&asset.bytes) {
            Ok(()) => config.symbol_font = Some(asset.path),
            Err(error) => warnings.push(format!("config symbol_font rejected ({error})")),
        }
    }
    if symbol_i.is_some()
        && config.symbol_font.is_none()
        && request.previous_font_config.symbol_font.is_some()
    {
        preserve_current_generation = true;
    }
    if let Some(asset) = emoji_i.and_then(admitted) {
        match renderer.set_color_font_arc(asset.bytes) {
            Ok(()) => config.emoji_font = Some(asset.path),
            Err(error) => warnings.push(format!("config emoji_font rejected ({error})")),
        }
    }
    if emoji_i.is_some()
        && config.emoji_font.is_none()
        && request.previous_font_config.emoji_font.is_some()
    {
        preserve_current_generation = true;
    }
    let (variations, _) = source_config.font_variation_requests();
    let dark_nudge = source_config.font_weight_dark_nudge_or_default();
    renderer.set_font_variations(&variations, dark_nudge);
    let sources = renderer.seal_admitted_font_sources();
    let install_required = family != request.previous_family
        || config != request.previous_font_config
        || sources != request.previous_sources
        || variations != request.previous_variations
        || (dark_nudge - request.previous_dark_nudge).abs() > f32::EPSILON;
    // Complete every chrome parse/discovery on this worker too. In particular
    // `chrome_bold_face` may read a primary sibling the first time it is called.
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
    }
}

/// Main-thread decision for one completed immutable font generation. A config
/// sequence mismatch is inert because a newer request already represents the
/// durable edit. A theme mismatch is different: the config is still current,
/// but must be re-prepared against the newer catalog before it may publish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionDisposition {
    Publish,
    RejectStaleConfig,
    ReprepareLatestTheme,
}

pub(crate) const fn completion_disposition(
    requested: u64,
    completed: u64,
    current_theme: u64,
    completed_theme: u64,
) -> CompletionDisposition {
    if requested != completed {
        CompletionDisposition::RejectStaleConfig
    } else if current_theme != completed_theme {
        CompletionDisposition::ReprepareLatestTheme
    } else {
        CompletionDisposition::Publish
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        Completion, CompletionDisposition, Lane, PrimarySeed, Request, completion_disposition,
        prepare,
    };

    fn request(sequence: u64) -> Request {
        request_with_text(sequence, "")
    }

    fn request_with_text(sequence: u64, text: &str) -> Request {
        let dir = std::env::temp_dir().join(format!(
            "aterm-font-lane-{}-{}",
            std::process::id(),
            sequence
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
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
                path: None,
                bytes: Arc::from(
                    include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf").as_slice(),
                ),
            },
        }
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
        let model = aterm_spec::derive::font_catalog_generation_model();
        let mut state = model.init_state();
        for action in ["RequestFirst", "RequestSecond", "CompleteFirst"] {
            assert!(model.fire(action, &mut state));
        }
        let requested = state[&"requested"] as u64;
        let stale = state[&"completed"] as u64;
        assert_eq!(
            completion_disposition(requested, stale, 0, 0),
            CompletionDisposition::RejectStaleConfig
        );
        assert!(model.fire("RejectStale", &mut state));
        assert!(model.fire("CompleteSecond", &mut state));
        let current = state[&"completed"] as u64;
        assert_eq!(
            completion_disposition(requested, current, 0, 0),
            CompletionDisposition::Publish
        );

        // Negative control: the mutant predicate reproduces stale publication.
        let buggy_accepts = |_: u64, completed: u64| completed > 0;
        assert!(buggy_accepts(requested, stale));
        assert_ne!(
            buggy_accepts(requested, stale),
            completion_disposition(requested, stale, 0, 0) == CompletionDisposition::Publish
        );
    }

    #[test]
    fn theme_overtaking_current_config_reprepares_instead_of_rolling_back() {
        // Watcher order: config A is sampled and queued against theme 0, then
        // theme 1 publishes before A's expensive font preparation completes.
        let requested_config = 1;
        let current_theme = 1;
        assert_eq!(
            completion_disposition(requested_config, 1, current_theme, 0),
            CompletionDisposition::ReprepareLatestTheme
        );

        // The shipping reducer assigns the re-preparation a fresh config ticket
        // while retaining the same immutable config observation. Its completion
        // is publishable only once both dimensions name current generations.
        let reprepare_ticket = 2;
        assert_eq!(
            completion_disposition(reprepare_ticket, reprepare_ticket, current_theme, 1),
            CompletionDisposition::Publish
        );

        // Negative control: the old sequence-only predicate accepted the first
        // completion and would replace theme 1 with assets resolved from theme 0.
        let old_guard = |requested, completed| requested == completed;
        assert!(old_guard(requested_config, 1));
    }

    /// The two theme catalogs the `FontThemeGeneration` trace admits, by
    /// generation, and the assets of every completion it has prepared, by
    /// config sequence — the identities its projection reads back.
    struct ThemeTrace {
        themes: [Arc<crate::app_config::ThemeCatalog>; 2],
        prepared: Vec<(i64, Arc<crate::app_config::ConfigAssetCatalog>)>,
        /// The theme generation of the last completion the worker produced.
        completed_theme: i64,
        /// Sticky: a publication was ever seen naming a superseded theme.
        stale_published: i64,
    }

    impl ThemeTrace {
        fn project(
            &mut self,
            app: &crate::App,
            in_hand: Option<&Completion>,
        ) -> Result<aterm_spec::interp::State, String> {
            let generation = |n: u64| i64::try_from(n).expect("a bounded generation");
            // WHICH completion the config service now holds: its assets Arc is
            // the one the published completion carried, and nothing else's.
            let held = app.native_config_service.snapshot().assets;
            let published = self
                .prepared
                .iter()
                .find(|(_, assets)| Arc::ptr_eq(assets, &held))
                .map_or(0, |(sequence, _)| *sequence);
            let published_theme = if published == 0 {
                0
            } else {
                self.themes
                    .iter()
                    .position(|themes| Arc::ptr_eq(themes, &held.themes))
                    .ok_or("the published assets carry a catalog this trace never admitted")?
                    as i64
            };
            let theme = generation(app.theme_catalog_generation);
            if published != 0 && published_theme != theme {
                self.stale_published = 1;
            }
            Ok(std::collections::BTreeMap::from([
                ("requested", generation(app.requested_font_catalog_sequence)),
                ("theme", theme),
                ("completed", in_hand.map_or(0, |c| generation(c.sequence))),
                ("completed_theme", self.completed_theme),
                ("published", published),
                ("published_theme", published_theme),
                ("stale_published", self.stale_published),
            ]))
        }

        /// The worker's half: prepare one request for real, remembering the
        /// assets it carries so a later publication can be named.
        fn complete(&mut self, request: Request) -> Completion {
            let completion = prepare(request);
            self.completed_theme = i64::try_from(completion.theme_generation).expect("bounded");
            self.prepared.push((
                i64::try_from(completion.sequence).expect("bounded"),
                Arc::clone(&completion.assets),
            ));
            completion
        }
    }

    type FinishFn = fn(&mut crate::App, Completion);

    fn shipped_finish(app: &mut crate::App, completion: Completion) {
        app.finish_font_catalog_generation(completion);
    }

    /// The guard from before theme generations existed: a completion naming the
    /// CURRENT config request is current, whatever catalog it was prepared
    /// against — the model's `Buggy = 1` publication of a theme-stale generation.
    fn sequence_only_finish(app: &mut crate::App, completion: Completion) {
        if completion.sequence == app.requested_font_catalog_sequence {
            app.apply_prepared_config_generation(completion.into_generation());
        }
    }

    /// Drive the overtaken-config trace on a REAL headless App and its real
    /// font-catalog worker function, checking every step against `model`.
    /// Returns every state visited, or the first real step the model refuses.
    fn font_theme_trace(
        model: &aterm_spec::derive::Model,
        finish: FinishFn,
    ) -> Result<std::collections::BTreeSet<aterm_spec::interp::State>, String> {
        let mut app = crate::App::headless_for_test();
        let (lane, requests) = Lane::test_pair();
        app.native_font_catalog = Some(lane);
        let scheme = aterm_types::scheme::builtin("Dracula").expect("a built-in scheme");
        let mut trace = ThemeTrace {
            themes: [
                Arc::clone(&app.config_assets.themes),
                crate::app_config::ThemeCatalog::from_schemes([("Work".to_string(), scheme)]),
            ],
            prepared: Vec::new(),
            completed_theme: 0,
            stale_published: 0,
        };
        let mut state = trace.project(&app, None)?;
        if state != model.init_state() {
            return Err(format!("the App starts at {state:?}, the model at Init"));
        }
        let mut visited = std::collections::BTreeSet::from([state.clone()]);
        let mut step = |action: &str,
                        state: &mut aterm_spec::interp::State,
                        app: &crate::App,
                        in_hand: Option<&Completion>,
                        trace: &mut ThemeTrace|
         -> Result<(), String> {
            let after = trace.project(app, in_hand)?;
            let expected = model.successors(action, state);
            if expected != vec![after.clone()] {
                return Err(format!(
                    "real {action} took {state:?} to {after:?}, the model's {action} to \
                     {expected:?}"
                ));
            }
            visited.insert(after.clone());
            *state = after;
            Ok(())
        };

        // The config watcher admits an edit; its font prepare is queued
        // against theme generation 0.
        app.reload_prepared_config_observation(
            request_with_text(90, "trail_sounds = false\n").prepared,
        );
        step("RequestConfig", &mut state, &app, None, &mut trace)?;
        // The theme directory publishes a new catalog before that prepare ends.
        app.reload_theme_catalog(Arc::clone(&trace.themes[1]));
        let admitted = &app.config_assets.themes;
        if !Arc::ptr_eq(admitted, &trace.themes[1]) {
            // The catalog the App actually admitted is generation 1.
            trace.themes[1] = Arc::clone(admitted);
        }
        step("ThemeChanged", &mut state, &app, None, &mut trace)?;
        // The worker finishes the OLD-theme prepare.
        let old = requests
            .try_recv()
            .map_err(|e| format!("no queued request: {e}"))?;
        let old = trace.complete(old);
        step("CompleteOldTheme", &mut state, &app, Some(&old), &mut trace)?;
        // The App decides: re-prepare the current config against the new theme.
        finish(&mut app, old);
        step("ReprepareLatestTheme", &mut state, &app, None, &mut trace)?;
        let latest = requests
            .try_recv()
            .map_err(|e| format!("the re-preparation was not queued: {e}"))?;
        // The worker could never deliver the same request twice, but the App's
        // decision must not depend on that: the model lets it, so drive it.
        for request in [latest.clone(), latest] {
            let completion = trace.complete(request);
            step(
                "CompleteLatestTheme",
                &mut state,
                &app,
                Some(&completion),
                &mut trace,
            )?;
            finish(&mut app, completion);
            step("PublishCurrent", &mut state, &app, None, &mut trace)?;
        }
        Ok(visited)
    }

    /// Tier-1 conformance for `FontThemeGeneration`
    /// (`aterm_spec::derive::font_theme_generation_model`): a theme catalog
    /// that overtakes an in-flight font prepare, driven on a REAL headless App —
    /// `reload_prepared_config_observation`, `reload_theme_catalog` and
    /// `finish_font_catalog_generation` — with the worker's real `prepare`
    /// between them. Every step is read back out of the App: its request and
    /// theme generations, the completion in hand, and WHICH completion (and so
    /// which theme catalog) the config service ended up holding. The trace
    /// reaches every state the model can. (The model's `PublishCurrent` at Init
    /// is a stutter with nothing prepared — no real counterpart, and no state
    /// of its own.)
    ///
    /// NEGATIVE CONTROL: the same trace with the pre-theme-generation guard
    /// (publish whatever completes for the current config request) is refused
    /// by the model at the re-preparation, where it publishes theme 0's
    /// generation over theme 1; and the shipped App is in turn refused by the
    /// `Buggy = 1` model.
    #[test]
    fn overtaking_theme_catalog_conforms_to_font_theme_generation_model() {
        let model = aterm_spec::derive::font_theme_generation_model();
        let visited =
            font_theme_trace(&model, shipped_finish).unwrap_or_else(|why| panic!("{why}"));
        assert_eq!(
            Ok(visited.len()),
            aterm_spec::interp::bmc(&model),
            "the real trace must visit every state the model reaches"
        );

        let stale = font_theme_trace(&model, sequence_only_finish)
            .expect_err("the model must refuse a theme-stale publication");
        assert!(
            stale.contains("real ReprepareLatestTheme"),
            "the refusal must land where the stale generation publishes: {stale}"
        );
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        font_theme_trace(&buggy, shipped_finish)
            .expect_err("the shipped App must not conform to the stale-publishing model");
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
        let trail = dir.join("trail.toml");
        let v1 = "pack = 1\nid = \"font-worker-exact\"\n";
        let v2 = "pack = 1\nid = \"font-worker-replacement\"\n";
        std::fs::write(&trail, v1).unwrap();
        let text = format!(
            "cursor_trail_packs = [{:?}]\n[sparkle_words]\nenabled = false\n",
            trail.to_string_lossy(),
        );

        let open_probe = aterm_effects::file_feed::probe_open_attempts(&trail);
        crate::app_config::reset_path_feed_read_count();
        let request = request_with_text(13, &text);
        assert_eq!(crate::app_config::path_feed_read_count(), 1);
        assert_eq!(
            open_probe.attempts(),
            1,
            "the genuine config worker opens the one active trail exactly once"
        );

        // ABA the pathname after preparation. The required immutable bundle
        // must remain v1 and native font/theme work must perform no new open.
        std::fs::write(&trail, v2).unwrap();
        std::fs::write(&trail, v1).unwrap();
        crate::app_config::reset_path_feed_read_count();
        let completion = prepare(request);
        assert_eq!(
            crate::app_config::path_feed_read_count(),
            0,
            "font/theme work must consume the carried generation without reopening feeds"
        );
        assert_eq!(open_probe.attempts(), 1);
        assert_eq!(completion.assets.trail_packs.ids, ["font-worker-exact"]);

        let _ = aterm_effects::file_feed::fingerprint_bounded_regular_utf8(
            &trail,
            aterm_effects::trail_pack::MAX_TRAIL_PACK_BYTES,
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
        // Shadow every Tiny Triumphs recipe that consumes a shared setting.
        // The remaining pack recipe is SelfGlow + graphic, so the exact
        // reachable table has an authoritative all-false projection even
        // though its arena still contains pack specs.
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
