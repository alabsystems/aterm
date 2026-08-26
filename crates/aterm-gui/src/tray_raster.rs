// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A small anti-aliased CPU rasterizer for the widget tray's [`DrawPrim`] list. It
//! renders the prims into an RGBA8 buffer with analytic SDF anti-aliasing (rounded
//! panel, concentric ring/arc gauges, capsule, dot) and real glyphs — the USER'S
//! terminal face (+ its real bold sibling), injected via [`set_chrome_fonts`] from
//! the renderer's resolved primary font, with the bundled DejaVu strictly as a
//! per-char COVERAGE fallback (symbols like ⌘⇧⌃✓ the terminal face lacks; see
//! [`select_chrome_face`]). It is the SHARED renderer core: the CPU/softbuffer
//! backend composites this buffer directly, and the GPU backend uploads it as one
//! overlay texture — so the tray looks IDENTICAL on both paths and stays WYSIWYG
//! for the `image` introspection. Pure pixels; no window/GPU state.
//!
//! TYPOGRAPHY (grid standard): text prims are positioned by BASELINE; row-centred
//! sites derive it from the cap-height centering rule ([`row_baseline`], backed by
//! the machine-checked `aterm_render::chrome_metrics::baseline_in_row_q`), and the
//! pen accumulates glyph advances in 26.6 fixed point with per-glyph rounding —
//! placement error ≤ 0.5 px, drift-free (proven in `chrome_metrics`).

use std::collections::HashMap;
use std::f32::consts::TAU;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use aterm_grapheme::GraphemeClusters;

/// Serialize the rare heavyweight Unicode-font warmups. Hot reload can replace
/// a pending semantic fork; its dropped receiver prevents stale installation,
/// while this gate prevents the old/new workers from concurrently parsing two
/// temporary 100–370 MB copies before the process-wide Arc interner converges.
static SEMANTIC_PREWARM_GATE: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, PartialEq, Eq)]
enum SemanticFontResolution {
    Host,
    Ready,
    Fallback(Vec<String>),
}

impl SemanticFontResolution {
    fn description(&self, candidate: &crate::widget::SemanticFontCandidate) -> String {
        let authored = candidate
            .authored_slots()
            .map(|(slot, family)| format!("{slot} {family:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut assets = Vec::new();
        if !candidate.fallback.is_empty() {
            assets.push(format!("fallback {:?}", candidate.fallback));
        }
        if let Some(symbol) = candidate.symbol.as_deref() {
            assets.push(format!("symbol {symbol:?}"));
        }
        if let Some(emoji) = candidate.emoji.as_deref() {
            assets.push(format!("emoji {emoji:?}"));
        }
        if !candidate.variations.is_empty() {
            assets.push(format!(
                "{} variation axis request(s)",
                candidate.variations.len()
            ));
        }
        let requested = [authored, assets.join(", ")]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        match self {
            Self::Host if candidate.is_host() => {
                "committed renderer font cascade ready".to_string()
            }
            Self::Host => format!("candidate renderer queued: {requested}"),
            Self::Ready => format!("candidate renderer ready: {requested}"),
            Self::Fallback(unresolved) => format!(
                "candidate fallback active: unresolved {}; requested {requested}",
                unresolved.join(", ")
            ),
        }
    }
}

#[derive(Clone)]
struct ResolvedCandidateFace {
    path: String,
    bytes: Arc<Vec<u8>>,
}

type CandidateFaceCache = HashMap<String, Result<ResolvedCandidateFace, String>>;

struct SemanticPrewarmJob {
    generation: u64,
    request: u64,
    candidate: crate::widget::SemanticFontCandidate,
    /// Present only when a renderer reload installs a new committed base in
    /// the parked worker. Subsequent candidates fork that retained base.
    base: Option<Renderer>,
}

struct SemanticPrewarmResult {
    generation: u64,
    request: u64,
    candidate: crate::widget::SemanticFontCandidate,
    renderer: Option<Renderer>,
    resolution: SemanticFontResolution,
    elapsed_ms: u64,
}

/// One parked worker per chrome-font context. The single pending slot is
/// replacement-based: a reload burst retains only its newest semantic fork,
/// while an already-running parse is generation-checked before installation.
struct SemanticPrewarmQueue {
    semantic_job: Mutex<Option<SemanticPrewarmJob>>,
    ready: Condvar,
}

struct SemanticPrewarmWorker {
    queue: Arc<SemanticPrewarmQueue>,
    results: std::sync::mpsc::Receiver<SemanticPrewarmResult>,
}

impl SemanticPrewarmWorker {
    fn spawn() -> Option<Self> {
        Self::spawn_serialized_by(&SEMANTIC_PREWARM_GATE)
    }

    /// `gate` is the serialization mutex for heavyweight warmups — the
    /// process-wide [`SEMANTIC_PREWARM_GATE`] in production. Worker-mechanics
    /// unit tests pass a private gate so their staged coalescing windows and
    /// bounded landing waits never queue behind another test's warmup (nor
    /// stall anyone else's): the worker protocol under test is identical, the
    /// serialization domain is per-test.
    fn spawn_serialized_by(gate: &'static Mutex<()>) -> Option<Self> {
        let queue = Arc::new(SemanticPrewarmQueue {
            semantic_job: Mutex::new(None),
            ready: Condvar::new(),
        });
        let worker_queue = Arc::clone(&queue);
        let (tx, results) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("aterm-semantic-font-prewarm".into())
            .spawn(move || {
                let mut base: Option<(u64, Renderer)> = None;
                let mut faces = CandidateFaceCache::new();
                loop {
                    let mut slot = worker_queue
                        .semantic_job
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    while slot.is_none() {
                        slot = worker_queue
                            .ready
                            .wait(slot)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                    let Some(job) = slot.take() else {
                        continue;
                    };
                    drop(slot);

                    let _serial = gate
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    // A newer reload may have arrived while this job waited behind a
                    // prior parse. Drop it before the heavyweight fallback discovery;
                    // the one-slot queue already contains the latest replacement.
                    if worker_queue
                        .semantic_job
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref()
                        .is_some_and(|newer| newer.generation != job.generation)
                    {
                        continue;
                    }
                    let started = std::time::Instant::now();
                    if let Some(mut renderer) = job.base {
                        renderer.prepare_semantic_typography(
                            "Input→present λ π √ ✓ 你好世界 日本語 한글 🚀 😀 🐈‍⬛ e\u{301}",
                        );
                        base = Some((job.generation, renderer));
                        // A renderer generation owns its configured cascade.
                        // Paths may still resolve to the same interned bytes,
                        // but a new base must never inherit an old candidate's
                        // success/failure verdict accidentally.
                        faces.clear();
                    }
                    let (renderer, resolution) = base
                        .as_ref()
                        .filter(|(generation, _)| *generation == job.generation)
                        .map_or_else(
                            || {
                                (
                                    None,
                                    SemanticFontResolution::Fallback(vec![
                                        "committed renderer unavailable".to_string(),
                                    ]),
                                )
                            },
                            |(_, base)| build_semantic_candidate(base, &job.candidate, &mut faces),
                        );
                    let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                    if tx
                        .send(SemanticPrewarmResult {
                            generation: job.generation,
                            request: job.request,
                            candidate: job.candidate,
                            renderer,
                            resolution,
                            elapsed_ms,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .ok()?;
        Some(Self { queue, results })
    }

    fn submit_latest(&self, mut job: SemanticPrewarmJob) {
        let mut pending = self
            .queue
            .semantic_job
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // If the worker has not taken the generation-install job yet, a rapid
        // candidate replacement must carry that one non-cloneable base forward.
        // If it has taken it, the worker already owns the base and `pending` is
        // empty. This closes the A→B-before-first-poll wedge without ever cloning
        // a Renderer or falling back to UI-thread construction.
        if let Some(previous) = pending.take() {
            let carries_base = crate::semantic_prewarm_replacement_carries_base(
                job.base.is_some(),
                previous.base.is_some(),
            );
            if job.base.is_none() && carries_base {
                job.base = previous.base;
            }
        }
        *pending = Some(job);
        drop(pending);
        self.queue.ready.notify_one();
    }

    fn cancel_queued(&self) {
        self.queue
            .semantic_job
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

fn resolve_candidate_face(
    family: &str,
    cache: &mut CandidateFaceCache,
) -> Result<ResolvedCandidateFace, String> {
    let key = family.trim().to_ascii_lowercase();
    if let Some(cached) = cache.get(&key) {
        return cached.clone();
    }
    // The `display:` scheme resolves to embedded bytes, never a file read — the
    // same interception every other resolution path performs.
    let resolved = if let Some(bytes) = aterm_render::display_face_for_family(family.trim()) {
        Ok(ResolvedCandidateFace {
            path: family.trim().to_string(),
            bytes: aterm_render::intern_font_bytes(bytes.to_vec()),
        })
    } else {
        aterm_render::resolve_config_font(family).and_then(|path| {
            aterm_render::font_file::read_font_file(std::path::Path::new(&path))
                .map(|bytes| ResolvedCandidateFace {
                    path,
                    bytes: aterm_render::intern_font_bytes(bytes),
                })
                .map_err(|error| format!("font {family:?} could not be read ({error})"))
        })
    };
    cache.insert(key, resolved.clone());
    resolved
}

/// Worker-only candidate construction. Family resolution, file reads, fontdue
/// parsing, styled-face injection, and specimen warmup all happen here; the UI
/// thread only swaps the finished renderer into its bounded cache.
fn build_semantic_candidate(
    base: &Renderer,
    candidate: &crate::widget::SemanticFontCandidate,
    faces: &mut CandidateFaceCache,
) -> (Option<Renderer>, SemanticFontResolution) {
    let Some(mut renderer) = base.fork_semantic_surface(14.0, aterm_render::Theme::default())
    else {
        return (
            None,
            SemanticFontResolution::Fallback(vec!["renderer fork".to_string()]),
        );
    };
    // Candidate resolution is complete on this parked worker. Once the fork
    // reaches semantic compile/paint it must be a closed immutable cascade:
    // no missing glyph may trigger system resolver or filesystem work.
    renderer.set_runtime_font_discovery(false);
    let mut unresolved = Vec::new();
    if let Some(family) = candidate.regular.as_deref() {
        match resolve_candidate_face(family, faces).and_then(|face| {
            renderer.set_primary_font_from_resolved_path(face.bytes.as_slice(), face.path)
        }) {
            Ok(()) => {}
            Err(error) => unresolved.push(format!("regular {family:?}: {error}")),
        }
    }
    for (slot, label, family) in [
        (0, "bold", candidate.bold.as_deref()),
        (1, "italic", candidate.italic.as_deref()),
        (2, "bold italic", candidate.bold_italic.as_deref()),
    ] {
        let Some(family) = family else {
            continue;
        };
        match resolve_candidate_face(family, faces)
            .and_then(|face| renderer.set_styled_font_bytes(slot, face.bytes.as_slice()))
        {
            Ok(()) => {}
            Err(error) => unresolved.push(format!("{label} {family:?}: {error}")),
        }
    }
    let mut installed_fallback = false;
    for family in &candidate.fallback {
        match resolve_candidate_face(family, faces).and_then(|face| {
            if installed_fallback {
                renderer.add_fallback_bytes(face.bytes.as_slice())
            } else {
                renderer.set_fallback_bytes(face.bytes.as_slice())
            }
        }) {
            Ok(()) => installed_fallback = true,
            Err(error) => unresolved.push(format!("fallback {family:?}: {error}")),
        }
    }
    if let Some(family) = candidate.symbol.as_deref() {
        match resolve_candidate_face(family, faces)
            .and_then(|face| renderer.set_symbol_fallback_bytes(face.bytes.as_slice()))
        {
            Ok(()) => {}
            Err(error) => unresolved.push(format!("symbol {family:?}: {error}")),
        }
    }
    if let Some(family) = candidate.emoji.as_deref() {
        match resolve_candidate_face(family, faces)
            .and_then(|face| renderer.set_color_font_arc(Arc::clone(&face.bytes)))
        {
            Ok(()) => {}
            Err(error) => unresolved.push(format!("emoji {family:?}: {error}")),
        }
    }
    renderer.set_synthetic_styles(candidate.synthetic_styles);
    let variations = candidate.variation_requests();
    let _ = renderer.set_font_variations(&variations, 0.0);
    renderer.prepare_semantic_typography(
        "Regular Aa 012 · Bold Aa · Italic Aa · Bold Italic Aa · != == => -> · 你好世界 日本語 한글 🚀 😀 🐈‍⬛ e\u{301}",
    );
    let resolution = if !unresolved.is_empty() {
        SemanticFontResolution::Fallback(unresolved)
    } else if candidate.is_host() {
        SemanticFontResolution::Host
    } else {
        SemanticFontResolution::Ready
    };
    (Some(renderer), resolution)
}

#[cfg(test)]
use std::ops::{Deref, DerefMut};

use aterm_render::chrome_metrics::{baseline_in_row_q, px_to_q, q_round_to_px, q_to_px};
use aterm_render::Renderer;

use crate::widget::{DrawPrim, SpecimenTextBlending, TerminalSpecimenSpec, TextFace, TextWeight};

/// One loaded chrome face: the parsed fontdue font plus its cap-height ratio
/// (OS/2 `sCapHeight`, measured-'H' fallback) and a per-char em-advance memo
/// for [`measure_text`].
struct ChromeFace {
    /// Shared with the TERMINAL renderer's parse of the same bytes: the chrome
    /// draws Settings/About/tabs in the user's terminal face
    /// (`Renderer::chrome_primary_face`) and falls back to the same bundled
    /// DejaVu the default renderer parses, so every chrome face here was a
    /// byte-identical second copy of a face the process already held
    /// (~6.9 MB of live heap each). See `aterm_render::shared_parsed_face`.
    font: std::sync::Arc<fontdue::Font>,
    /// Cap height as a fraction of the em (drives [`row_baseline`]).
    cap_ratio: f32,
    /// Memoized advance-per-em by char (advances scale linearly with px).
    advances: std::collections::HashMap<char, f32>,
}

/// Reference size for the em-advance memo (large enough that fontdue's f32
/// metric quantization is negligible).
const ADVANCE_REF_PX: f32 = 64.0;

impl ChromeFace {
    fn from_bytes(bytes: &[u8], index: u32) -> Option<Self> {
        let (_, font) = aterm_render::shared_parsed_face(bytes, index).ok()?;
        let cap_ratio = aterm_render::chrome_metrics::cap_height_ratio(bytes, index, &font);
        Some(Self {
            font,
            cap_ratio,
            advances: std::collections::HashMap::new(),
        })
    }

    /// Whether this face has a real (non-`.notdef`) glyph for `ch`.
    fn has(&self, ch: char) -> bool {
        self.font.lookup_glyph_index(ch) != 0
    }

    /// The advance of `ch` per em (memoized; multiply by the run's px).
    fn advance_em(&mut self, ch: char) -> f32 {
        if let Some(&a) = self.advances.get(&ch) {
            return a;
        }
        let a = self.font.metrics(ch, ADVANCE_REF_PX).advance_width / ADVANCE_REF_PX;
        self.advances.insert(ch, a);
        a
    }
}

/// The chrome font stack: the user's terminal face + its real bold sibling
/// (both injected by [`set_chrome_fonts`]) over the embedded DejaVu coverage
/// fallback. `primary`/`bold` are `None` until the renderer resolves them (and
/// in unit tests), leaving the deterministic DejaVu-only stack.
struct ChromeFonts {
    primary: Option<ChromeFace>,
    bold: Option<ChromeFace>,
    fallback: Option<ChromeFace>,
    /// Host-prepared proportional UI faces. These immutable parsed assets are
    /// installed by `set_chrome_fonts`; measure/compile/raster never probe a
    /// platform path or initialize a font lazily.
    ui_regular: Option<Arc<fontdue::Font>>,
    ui_semibold: Option<Arc<fontdue::Font>>,
    /// An exact owned fork of the live renderer's font engine. It is prepared
    /// off the UI/raster thread because a first CJK face can cost hundreds of
    /// milliseconds and hundreds of MB to parse; parsed fallbacks themselves
    /// are process-interned and shared with the live renderer.
    semantic: Option<Renderer>,
    semantic_identity: Option<crate::widget::SemanticFontCandidate>,
    semantic_resolution: SemanticFontResolution,
    semantic_requested: crate::widget::SemanticFontCandidate,
    semantic_generation: u64,
    semantic_request: u64,
    semantic_pending: Option<(u64, u64, crate::widget::SemanticFontCandidate)>,
    semantic_worker_generation: Option<u64>,
    semantic_worker: Option<SemanticPrewarmWorker>,
    semantic_cache:
        HashMap<crate::widget::SemanticFontCandidate, (Renderer, SemanticFontResolution, u64)>,
    semantic_ready_epoch: u64,
    /// Measured wall time of the last one-time background specimen warmup.
    /// Retained for Diagnostics/tests; it is not part of paint identity.
    semantic_prewarm_ms: Option<u64>,
}

fn default_chrome_fonts() -> ChromeFonts {
    // Keep the cold store identical in production and tests. Backend
    // construction is the one boundary that installs immutable UI assets via
    // `set_chrome_fonts`; direct-view tests must exercise that transition
    // explicitly instead of receiving a test-only prepared store.
    ChromeFonts {
        primary: None,
        bold: None,
        fallback: ChromeFace::from_bytes(aterm_render::embedded_font(), 0),
        ui_regular: None,
        ui_semibold: None,
        semantic: None,
        semantic_identity: None,
        semantic_resolution: SemanticFontResolution::Host,
        semantic_requested: crate::widget::SemanticFontCandidate::default(),
        semantic_generation: 0,
        semantic_request: 0,
        semantic_pending: None,
        semantic_worker_generation: None,
        semantic_worker: None,
        semantic_cache: HashMap::new(),
        semantic_ready_epoch: 0,
        semantic_prewarm_ms: None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SemanticFontSnapshot {
    pub(crate) status: String,
    pub(crate) ready_epoch: u64,
    pub(crate) pending: bool,
}

/// One host-prepared, immutable semantic renderer source captured for a single
/// view compilation. The worker/cache lock is consulted only while creating
/// this value; semantic tree construction and rasterization merely fork the
/// private renderer carried here. `Renderer` is deliberately thread-bound, so
/// snapshots share it with a non-atomic [`Rc`] and never imply cross-thread use.
#[derive(Clone)]
pub(crate) struct PreparedSemanticFont {
    pub(crate) candidate: crate::widget::SemanticFontCandidate,
    pub(crate) snapshot: SemanticFontSnapshot,
    renderer: Option<Rc<Renderer>>,
}

impl std::fmt::Debug for PreparedSemanticFont {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedSemanticFont")
            .field("candidate", &self.candidate)
            .field("snapshot", &self.snapshot)
            .field("renderer_ready", &self.renderer.is_some())
            .finish()
    }
}

impl PartialEq for PreparedSemanticFont {
    fn eq(&self, other: &Self) -> bool {
        self.candidate == other.candidate
            && self.snapshot == other.snapshot
            && self.renderer.is_some() == other.renderer.is_some()
    }
}

impl PreparedSemanticFont {
    pub(crate) fn unavailable(candidate: crate::widget::SemanticFontCandidate) -> Self {
        Self {
            candidate,
            snapshot: SemanticFontSnapshot {
                status: "host-prepared renderer snapshot unavailable".to_string(),
                ready_epoch: 0,
                pending: false,
            },
            renderer: None,
        }
    }

    pub(crate) fn matches(&self, candidate: &crate::widget::SemanticFontCandidate) -> bool {
        &self.candidate == candidate
    }

    /// Whether this immutable snapshot can paint the real terminal specimen.
    /// Callers use this only to decide whether a non-renderer fallback is
    /// needed; rasterization still forks the captured renderer below.
    pub(crate) fn is_ready(&self) -> bool {
        self.renderer.is_some()
    }

    #[cfg(test)]
    pub(crate) fn renderer_ready(&self) -> bool {
        self.renderer.is_some()
    }

    fn fork(&self, px: f32, theme: aterm_render::Theme) -> Option<Renderer> {
        self.renderer.as_ref()?.fork_semantic_surface(px, theme)
    }
}

#[cfg(not(test))]
fn chrome_fonts() -> &'static Mutex<ChromeFonts> {
    static FONTS: OnceLock<Mutex<ChromeFonts>> = OnceLock::new();
    FONTS.get_or_init(|| Mutex::new(default_chrome_fonts()))
}

/// Lock the chrome font stack, recovering from a poisoned mutex (a panicked
/// test thread must not take every later tray raster down with it).
#[cfg(not(test))]
fn lock_fonts() -> std::sync::MutexGuard<'static, ChromeFonts> {
    chrome_fonts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Unit tests construct independent `App`s in parallel. Each app may install a
/// different resolved terminal face, while layout assertions measure and paint
/// in separate calls. Production has one process-global renderer context, but a
/// process-global test font lets an unrelated worker swap the face between those
/// calls. Keep the production shape and isolate only libtest workers by thread.
#[cfg(test)]
struct TestChromeFontsGuard {
    fonts: std::sync::MutexGuard<
        'static,
        std::collections::HashMap<std::thread::ThreadId, ChromeFonts>,
    >,
    thread: std::thread::ThreadId,
}

#[cfg(test)]
impl Deref for TestChromeFontsGuard {
    type Target = ChromeFonts;

    fn deref(&self) -> &Self::Target {
        self.fonts
            .get(&self.thread)
            .expect("test thread font context was installed")
    }
}

#[cfg(test)]
impl DerefMut for TestChromeFontsGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.fonts
            .get_mut(&self.thread)
            .expect("test thread font context was installed")
    }
}

#[cfg(test)]
fn lock_fonts() -> TestChromeFontsGuard {
    static FONTS: OnceLock<Mutex<std::collections::HashMap<std::thread::ThreadId, ChromeFonts>>> =
        OnceLock::new();
    let mut fonts = FONTS
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let thread = std::thread::current().id();
    // Keep the per-thread test store production-cold. Tests that construct a
    // semantic view without backend startup cross the same explicit preparation
    // boundary via `prepare_ui_fonts_for_direct_view_test`.
    fonts.entry(thread).or_insert_with(default_chrome_fonts);
    TestChromeFontsGuard { fonts, thread }
}

/// Install the renderer-resolved chrome faces — `(bytes, collection index)` of
/// the PRIMARY terminal face and its real BOLD sibling (from
/// `Renderer::chrome_primary_face` / `chrome_bold_face`) — so the tray text
/// renders in the user's font. Called by `App::sync_chrome_fonts` after every
/// backend (re)build; unparseable bytes leave that slot on the fallback.
pub(crate) fn set_chrome_fonts(
    primary: Option<(std::sync::Arc<[u8]>, u32)>,
    bold: Option<(std::sync::Arc<[u8]>, u32)>,
    semantic: Option<Renderer>,
) {
    let parse = |slot: &Option<(std::sync::Arc<[u8]>, u32)>| {
        slot.as_ref()
            .and_then(|(bytes, index)| ChromeFace::from_bytes(bytes, *index))
    };
    let (p, b) = (parse(&primary), parse(&bold));
    let mut fonts = lock_fonts();
    install_chrome_faces_locked(&mut fonts, p, b, semantic);
    // The service is owned by backend initialization, never demand-spawned by a
    // semantic compile or paint. Candidate requests remain lazy, but the one
    // parked worker already exists before a Settings view can ask for one.
    if fonts.semantic_worker.is_none() {
        fonts.semantic_worker = SemanticPrewarmWorker::spawn();
    }
    if let Some(worker) = fonts.semantic_worker.as_ref() {
        worker.cancel_queued();
    }
}

/// Fully parsed chrome/semantic font generation. Constructed by the font
/// catalog worker; publishing it performs no path I/O or font parsing.
pub(crate) struct PreparedChromeFonts {
    primary: Option<ChromeFace>,
    bold: Option<ChromeFace>,
    semantic: Option<Renderer>,
}

pub(crate) fn prepare_chrome_fonts(
    primary: Option<(std::sync::Arc<[u8]>, u32)>,
    bold: Option<(std::sync::Arc<[u8]>, u32)>,
    semantic: Option<Renderer>,
) -> PreparedChromeFonts {
    let parse = |slot: &Option<(std::sync::Arc<[u8]>, u32)>| {
        slot.as_ref()
            .and_then(|(bytes, index)| ChromeFace::from_bytes(bytes, *index))
    };
    PreparedChromeFonts {
        primary: parse(&primary),
        bold: parse(&bold),
        semantic,
    }
}

pub(crate) fn set_prepared_chrome_fonts(prepared: PreparedChromeFonts) {
    let mut fonts = lock_fonts();
    install_chrome_faces_locked(
        &mut fonts,
        prepared.primary,
        prepared.bold,
        prepared.semantic,
    );
    if fonts.semantic_worker.is_none() {
        fonts.semantic_worker = SemanticPrewarmWorker::spawn();
    }
    if let Some(worker) = fonts.semantic_worker.as_ref() {
        worker.cancel_queued();
    }
}

/// The locked chrome-face installation shared by [`set_chrome_fonts`] and the
/// test fixture seam: swap in the parsed faces and the dormant committed
/// semantic seed, invalidating every retained candidate view.
fn install_chrome_faces_locked(
    fonts: &mut ChromeFonts,
    primary: Option<ChromeFace>,
    bold: Option<ChromeFace>,
    semantic: Option<Renderer>,
) {
    let ui = prepared_ui_font_assets();
    fonts.primary = primary;
    fonts.bold = bold;
    fonts.ui_regular = ui.regular.clone();
    fonts.ui_semibold = ui.semibold.clone();
    fonts.semantic_generation = fonts.semantic_generation.wrapping_add(1);
    fonts.semantic = semantic;
    fonts.semantic_identity = fonts
        .semantic
        .as_ref()
        .map(|_| crate::widget::SemanticFontCandidate::default());
    fonts.semantic_resolution = SemanticFontResolution::Host;
    fonts.semantic_requested = crate::widget::SemanticFontCandidate::default();
    fonts.semantic_pending = None;
    fonts.semantic_worker_generation = None;
    fonts.semantic_cache.clear();
    fonts.semantic_ready_epoch = fonts.semantic_ready_epoch.wrapping_add(1);
    fonts.semantic_prewarm_ms = None;
}

/// Test-only fixture seam: install `renderer` as this test thread's SETTLED
/// semantic chrome font.
///
/// Preview unit tests paint one spec several times and assert pixel identity.
/// Production installs a dormant seed and lets the parked worker's warmup
/// LAND asynchronously; in a parallel test run that landing can fall between
/// two of those paints, swapping the active cascade mid-test (the pass-
/// isolated/fail-parallel flake in `settings_preview::tests`). Here the
/// worker's exact install-generation warmup runs INLINE instead, and no
/// worker is parked on this thread's context, so nothing can land later:
/// every paint on this thread sees one settled cascade.
#[cfg(test)]
pub(crate) fn install_settled_chrome_fonts_for_test(mut renderer: Renderer) -> u64 {
    // The same warmup `SemanticPrewarmWorker` runs when a job installs a new
    // committed base, so the settled test cascade matches what production
    // paints after the landing.
    renderer.prepare_semantic_typography(
        "Input→present λ π √ ✓ 你好世界 日本語 한글 🚀 😀 🐈\u{200d}⬛ e\u{301}",
    );
    let mut fonts = lock_fonts();
    install_chrome_faces_locked(&mut fonts, None, None, Some(renderer));
    // SETTLED, not dormant: the warmup already ran inline above, and no
    // parked worker exists to replace the cascade asynchronously.
    fonts.semantic_prewarm_ms = Some(0);
    fonts.semantic_worker = None;
    fonts.semantic_ready_epoch
}

/// Test-only generation token for the current libtest worker's chrome-font
/// context. Libtest may reuse a worker thread for multiple tests; retaining a
/// boolean "installed" flag across that reuse is insufficient because another
/// test can replace the same thread-local cascade in between. Pixel fixtures
/// remember this token and reinstall their exact settled cascade whenever it
/// changes.
#[cfg(test)]
pub(crate) fn chrome_font_epoch_for_test() -> u64 {
    lock_fonts().semantic_ready_epoch
}

/// Direct semantic-view tests do not construct a renderer backend, so they
/// explicitly install the same immutable proportional faces that backend
/// startup would install. This keeps [`default_chrome_fonts`] production-cold
/// while letting view-layout fixtures opt into production typography without
/// replacing an already-installed semantic renderer.
#[cfg(test)]
pub(crate) fn prepare_ui_fonts_for_direct_view_test() {
    let ui = prepared_ui_font_assets();
    let mut fonts = lock_fonts();
    fonts.ui_regular = ui.regular.clone();
    fonts.ui_semibold = ui.semibold.clone();
}

/// The inverse seam: return THIS test thread's chrome stack to the cold,
/// UI-face-less store. Libtest reuses worker threads, so a test that asserts
/// the "no UI face installed" branch (the strip band's decline path) cannot
/// assume a cold thread — an earlier test on the same worker may have prepared
/// the faces.
#[cfg(test)]
pub(crate) fn clear_ui_fonts_for_test() {
    let mut fonts = lock_fonts();
    fonts.ui_regular = None;
    fonts.ui_semibold = None;
}

/// An owned, immutable semantic renderer input for deterministic direct-view
/// tests. The production host supplies the same kind of [`PreparedSemanticFont`]
/// through `ViewCx`; these tests inject that boundary value directly so an
/// asynchronous prewarm completion cannot change two phase-comparison frames.
#[cfg(test)]
pub(crate) fn prepared_semantic_font_for_direct_view_test(
    candidate: &crate::widget::SemanticFontCandidate,
) -> PreparedSemanticFont {
    if !candidate.is_host() {
        return PreparedSemanticFont::unavailable(candidate.clone());
    }

    thread_local! {
        static EMBEDDED: std::cell::RefCell<Option<Rc<Renderer>>> = const {
            std::cell::RefCell::new(None)
        };
    }
    let renderer = EMBEDDED.with(|slot| {
        let mut slot = slot.borrow_mut();
        slot.get_or_insert_with(|| {
            let mut renderer = Renderer::from_bytes(
                aterm_render::embedded_font(),
                14.0,
                aterm_render::Theme::default(),
            )
            .expect("embedded direct-view semantic renderer");
            renderer.set_runtime_font_discovery(false);
            renderer.prepare_semantic_typography("Bold Italic != => 你好 ∑✓♥ 😀 🚀 👩‍💻");
            Rc::new(renderer)
        })
        .clone()
    });
    PreparedSemanticFont {
        candidate: candidate.clone(),
        snapshot: SemanticFontSnapshot {
            status: "host-prepared embedded renderer ready".to_string(),
            ready_epoch: 1,
            pending: false,
        },
        renderer: Some(renderer),
    }
}

/// Which face of the chrome stack draws a glyph — the PURE policy (the proof
/// target; no font I/O). A bold run whose real bold face covers the char takes
/// the BOLD face; else a char the user's primary covers takes the PRIMARY; the
/// embedded DejaVu is STRICTLY a coverage fallback.
///
/// # Invariant (proven)
/// `Fallback` is returned ONLY when neither user face applies
/// (`EmbeddedOnlyAsCoverageFallback`), and a covered bold run always keeps its
/// weight (`BoldHonoredWhenCovered`). Two-tier: the `ChromeFaceGate` derived
/// model (`aterm_spec::derive::chrome_face_gate_model`) is checked by the real
/// Trust `ty` over the whole bounded space (proves at Buggy=0, catches the old
/// hardcoded-DejaVu chrome at Buggy=1); the exhaustive 2^3 enumeration in this
/// module's tests is the Tier-1 binding to this shipping fn.
pub(crate) fn select_chrome_face(
    bold_run: bool,
    bold_has: bool,
    primary_has: bool,
) -> ChromeFacePick {
    if bold_run && bold_has {
        ChromeFacePick::Bold
    } else if primary_has {
        ChromeFacePick::Primary
    } else {
        ChromeFacePick::Fallback
    }
}

/// The outcome of [`select_chrome_face`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ChromeFacePick {
    Bold,
    Primary,
    Fallback,
}

impl ChromeFonts {
    fn ui_font(&self, face: TextFace) -> Option<&Arc<fontdue::Font>> {
        if face == TextFace::UiBold {
            self.ui_semibold.as_ref().or(self.ui_regular.as_ref())
        } else {
            self.ui_regular.as_ref()
        }
    }

    /// Resolve `(weight, ch)` to the face that draws it, per
    /// [`select_chrome_face`]; `None` when even the fallback failed to parse.
    fn face_for(&mut self, weight: TextWeight, ch: char) -> Option<&mut ChromeFace> {
        let bold_run = weight == TextWeight::Bold;
        let bold_has = self.bold.as_ref().is_some_and(|f| f.has(ch));
        let primary_has = self.primary.as_ref().is_some_and(|f| f.has(ch));
        match select_chrome_face(bold_run, bold_has, primary_has) {
            ChromeFacePick::Bold => self.bold.as_mut(),
            ChromeFacePick::Primary => self.primary.as_mut(),
            ChromeFacePick::Fallback => self.fallback.as_mut().filter(|face| face.has(ch)),
        }
    }

    fn poll_semantic_renderer(&mut self) {
        let mut landed = Vec::new();
        if let Some(worker) = self.semantic_worker.as_ref() {
            while let Ok(result) = worker.results.try_recv() {
                landed.push(result);
            }
        }
        for result in landed {
            let candidate_matches = result.candidate == self.semantic_requested;
            let decision = crate::semantic_prewarm_result_decision(
                self.semantic_generation,
                self.semantic_request,
                result.generation,
                result.request,
                candidate_matches,
                result.renderer.is_some(),
            );
            match decision {
                crate::SemanticPrewarmResultDecision::IgnoreStaleGeneration
                | crate::SemanticPrewarmResultDecision::IgnoreFailedSuperseded => {}
                crate::SemanticPrewarmResultDecision::FailClosedCurrent => {
                    self.semantic = None;
                    self.semantic_identity = None;
                    self.semantic_resolution = result.resolution;
                    self.semantic_pending = None;
                    self.semantic_ready_epoch = self.semantic_ready_epoch.wrapping_add(1);
                }
                crate::SemanticPrewarmResultDecision::InstallCurrent => {
                    let renderer = result
                        .renderer
                        .expect("InstallCurrent requires a ready renderer");
                    self.cache_active_semantic();
                    self.semantic = Some(renderer);
                    self.semantic_identity = Some(result.candidate);
                    self.semantic_resolution = result.resolution;
                    self.semantic_prewarm_ms = Some(result.elapsed_ms);
                    self.semantic_pending = None;
                    self.semantic_ready_epoch = self.semantic_ready_epoch.wrapping_add(1);
                }
                crate::SemanticPrewarmResultDecision::CacheSuperseded => {
                    let renderer = result
                        .renderer
                        .expect("CacheSuperseded requires a ready renderer");
                    self.insert_semantic_cache(
                        result.candidate,
                        renderer,
                        result.resolution,
                        result.elapsed_ms,
                    );
                }
            }
        }
    }

    fn insert_semantic_cache(
        &mut self,
        candidate: crate::widget::SemanticFontCandidate,
        renderer: Renderer,
        resolution: SemanticFontResolution,
        elapsed_ms: u64,
    ) {
        const MAX_CANDIDATES: usize = 8;
        if self.semantic_cache.len() >= MAX_CANDIDATES
            && !self.semantic_cache.contains_key(&candidate)
            && let Some(oldest) = self.semantic_cache.keys().next().cloned()
        {
            self.semantic_cache.remove(&oldest);
        }
        self.semantic_cache
            .insert(candidate, (renderer, resolution, elapsed_ms));
    }

    fn cache_active_semantic(&mut self) {
        let Some(renderer) = self.semantic.take() else {
            return;
        };
        let Some(candidate) = self.semantic_identity.take() else {
            self.semantic = Some(renderer);
            return;
        };
        // `None` means this is the dormant committed seed which the worker has
        // not retained yet; it must travel in an Install job, never be cached
        // as though its broad fallback warmup had completed.
        let Some(elapsed_ms) = self.semantic_prewarm_ms else {
            self.semantic = Some(renderer);
            self.semantic_identity = Some(candidate);
            return;
        };
        self.insert_semantic_cache(
            candidate,
            renderer,
            self.semantic_resolution.clone(),
            elapsed_ms,
        );
        // Parking the active renderer IS a change of what a fork would contain:
        // `semantic` and `semantic_identity` both just went to `None`. The
        // install/cache-hit callers bump the epoch immediately afterwards, but
        // the pre-request park in `ensure_semantic_candidate` did not — which
        // left `semantic_ready_epoch` briefly unable to witness the removal.
        // Bumping here makes the epoch a TOTAL key over the fork's inputs.
        self.semantic_ready_epoch = self.semantic_ready_epoch.wrapping_add(1);
    }

    /// Ensure the exact candidate authored by one preview is active. Family
    /// resolution and renderer construction happen on the parked worker; the
    /// UI/raster thread only performs bounded cache swaps.
    fn ensure_semantic_candidate(
        &mut self,
        candidate: &crate::widget::SemanticFontCandidate,
    ) -> bool {
        self.poll_semantic_renderer();
        if self.semantic.as_ref().is_some()
            && self.semantic_identity.as_ref() == Some(candidate)
            && self.semantic_prewarm_ms.is_some()
        {
            self.semantic_requested = candidate.clone();
            return false;
        }
        if self
            .semantic_pending
            .as_ref()
            .is_some_and(|(generation, _, pending)| {
                *generation == self.semantic_generation && pending == candidate
            })
        {
            return true;
        }

        if let Some((renderer, resolution, elapsed_ms)) = self.semantic_cache.remove(candidate) {
            self.cache_active_semantic();
            self.semantic = Some(renderer);
            self.semantic_identity = Some(candidate.clone());
            self.semantic_resolution = resolution;
            self.semantic_requested = candidate.clone();
            self.semantic_prewarm_ms = Some(elapsed_ms);
            self.semantic_pending = None;
            self.semantic_ready_epoch = self.semantic_ready_epoch.wrapping_add(1);
            return false;
        }

        if self.semantic_worker.is_none() {
            return false;
        }

        self.semantic_request = self.semantic_request.wrapping_add(1);
        self.semantic_requested = candidate.clone();
        let install_base = self.semantic_worker_generation != Some(self.semantic_generation);
        let base = if install_base {
            self.semantic_cache.clear();
            self.semantic.as_ref().and_then(|renderer| {
                renderer.fork_semantic_surface(14.0, aterm_render::Theme::default())
            })
        } else {
            None
        };
        if install_base && base.is_none() {
            self.semantic_resolution = SemanticFontResolution::Fallback(vec![
                "committed renderer unavailable".to_string(),
            ]);
            return false;
        }
        if crate::semantic_prewarm_cache_active_before_request(
            self.semantic.is_some(),
            self.semantic_prewarm_ms.is_some(),
            self.semantic_identity.as_ref() == Some(candidate),
        ) {
            // A completed non-host candidate is exact only for its own preview.
            // Move it into the bounded cache before another uncached candidate
            // starts resolving, so the old face cannot remain the active paint
            // source under the new request identity. The committed host seed has
            // no prewarm time and `cache_active_semantic` deliberately retains it
            // as the permitted broad fallback while the first request resolves.
            self.cache_active_semantic();
        }
        let job = SemanticPrewarmJob {
            generation: self.semantic_generation,
            request: self.semantic_request,
            candidate: candidate.clone(),
            base,
        };
        // The worker borrow begins only after all cache/base mutations above.
        self.semantic_worker
            .as_ref()
            .expect("semantic worker checked above")
            .submit_latest(job);
        if install_base {
            self.semantic_worker_generation = Some(self.semantic_generation);
        }
        self.semantic_pending = Some((
            self.semantic_generation,
            self.semantic_request,
            candidate.clone(),
        ));
        true
    }

    /// Host-preparation helper: capture one isolated renderer fork after the
    /// request/poll transition has settled as far as it can this frame.
    fn semantic_renderer_fork_for(
        &self,
        candidate: &crate::widget::SemanticFontCandidate,
        px: f32,
        theme: aterm_render::Theme,
    ) -> Option<Renderer> {
        let renderer = self.semantic.as_ref()?;
        let exact = self.semantic_identity.as_ref() == Some(candidate);
        let committed_fallback = self
            .semantic_identity
            .as_ref()
            .is_some_and(crate::widget::SemanticFontCandidate::is_host);
        (exact || committed_fallback)
            .then(|| renderer.fork_semantic_surface(px, theme))
            .flatten()
    }
}

fn semantic_font_snapshot_locked(
    fonts: &ChromeFonts,
    candidate: &crate::widget::SemanticFontCandidate,
) -> SemanticFontSnapshot {
    let pending = fonts
        .semantic_pending
        .as_ref()
        .is_some_and(|(_, _, pending)| pending == candidate);
    let status = if pending {
        let authored = candidate
            .authored_slots()
            .map(|(slot, family)| format!("{slot} {family:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        if authored.is_empty() {
            "committed renderer fallback and emoji loading".to_string()
        } else {
            format!("candidate renderer loading: {authored}")
        }
    } else if fonts.semantic_identity.as_ref() == Some(candidate)
        && fonts.semantic.is_some()
        && fonts.semantic_prewarm_ms.is_some()
    {
        fonts.semantic_resolution.description(candidate)
    } else if let Some((_, resolution, _)) = fonts.semantic_cache.get(candidate) {
        resolution.description(candidate)
    } else if fonts.semantic.is_some() && fonts.semantic_prewarm_ms.is_none() {
        "renderer fallback and emoji ready for host preparation".to_string()
    } else {
        SemanticFontResolution::Host.description(candidate)
    };
    SemanticFontSnapshot {
        status,
        ready_epoch: fonts.semantic_ready_epoch
            ^ u64::from(fonts.semantic_identity.as_ref() == Some(candidate)).rotate_left(17)
            ^ u64::from(pending).rotate_left(31),
        pending,
    }
}

/// One captured semantic fork: `(candidate, ready_epoch, fork)` — the memo key
/// followed by the renderer it produced (`None` when that key resolved to "no
/// semantic font"). Named because the inline spelling trips
/// `clippy::type_complexity` under the workspace's `-D warnings`.
type SemanticForkMemo = (
    crate::widget::SemanticFontCandidate,
    u64,
    Option<Rc<Renderer>>,
);

thread_local! {
    /// The last semantic fork this thread captured, keyed by
    /// `(candidate, snapshot.ready_epoch)`.
    ///
    /// A fork is NOT cheap: `Renderer::fork_semantic_surface` re-parses the
    /// whole primary face (`fontdue::Font::from_bytes`, again for the real bold
    /// sibling) plus its metric/feature tables. [`prepare_semantic_font`] runs
    /// on EVERY native compile and every preview tick while a Settings page is
    /// front, so an animating preview paid two full font parses per frame for a
    /// value that never differs between them.
    ///
    /// Reusing the `Rc` is byte-identical to handing each compilation its own
    /// copy: the captured renderer is immutable by construction —
    /// [`PreparedSemanticFont::fork`] takes `&self` and every consumer forks its
    /// own private mutable renderer off it — so nothing can observe the sharing
    /// (identity is never compared; `PartialEq`/`Debug` see only
    /// `renderer.is_some()`).
    ///
    /// `snapshot.ready_epoch` folds `semantic_ready_epoch` with the
    /// identity-match and pending bits, so it moves on every transition that can
    /// change what a fork contains; the candidate is part of the key because
    /// those two bits are computed against it. Thread-local because `Renderer`
    /// is deliberately thread-bound (`Rc`, `!Send`) — and because it then
    /// matches the per-thread test font store's isolation exactly.
    static SEMANTIC_FORK_MEMO: std::cell::RefCell<Option<SemanticForkMemo>> =
        const { std::cell::RefCell::new(None) };
}

/// Host-only request/poll/capture seam. The returned snapshot owns an isolated
/// fork and is the sole semantic-font input allowed past `ViewCx`.
pub(crate) fn prepare_semantic_font(
    candidate: &crate::widget::SemanticFontCandidate,
) -> PreparedSemanticFont {
    let mut fonts = lock_fonts();
    // The request/poll transition and the snapshot run on EVERY call, memo hit
    // or not: the former drives the worker convergence and the latter is paint
    // identity. Only the fork itself is memoized.
    let _ = fonts.ensure_semantic_candidate(candidate);
    let snapshot = semantic_font_snapshot_locked(&fonts, candidate);
    let memoized = SEMANTIC_FORK_MEMO.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|(memo_candidate, memo_epoch, renderer)| {
                (memo_candidate == candidate && *memo_epoch == snapshot.ready_epoch)
                    .then(|| renderer.clone())
            })
    });
    let renderer = match memoized {
        Some(renderer) => renderer,
        None => {
            let renderer = fonts
                .semantic_renderer_fork_for(candidate, 14.0, aterm_render::Theme::default())
                .map(Rc::new);
            // Release the chrome-font mutex before the thread-local write: the
            // memo needs nothing from it, and the lock scope stays minimal.
            drop(fonts);
            let memo = renderer.clone();
            SEMANTIC_FORK_MEMO.with(|slot| {
                // A `None` fork is cached too — a candidate with no installable
                // renderer must not re-attempt the fork every frame, and the
                // epoch moves the moment one lands.
                *slot.borrow_mut() = Some((candidate.clone(), snapshot.ready_epoch, memo));
            });
            renderer
        }
    };
    PreparedSemanticFont {
        candidate: candidate.clone(),
        snapshot,
        renderer,
    }
}

/// The width of `s` at `px` in the chrome stack — REAL advances (per-char face
/// pick + fixed-point accumulation, the same arithmetic the raster pen uses),
/// replacing the old `0.6 em × chars` estimate.
pub(crate) fn measure_text(s: &str, px: f32, weight: TextWeight) -> f32 {
    let mut fonts = lock_fonts();
    let mut pen_q: i64 = 0;
    for ch in s.chars() {
        if let Some(face) = fonts.face_for(weight, ch) {
            pen_q += px_to_q(face.advance_em(ch) * px);
        }
    }
    q_to_px(pen_q)
}

/// The cap-height ratio of the chrome's dominant text face (the user's primary,
/// else the fallback; 0.7 — the Latin norm — if no face parsed).
fn chrome_cap_ratio() -> f32 {
    let fonts = lock_fonts();
    fonts
        .primary
        .as_ref()
        .or(fonts.fallback.as_ref())
        .map_or(0.7, |f| f.cap_ratio)
}

/// The cap-height-centred BASELINE for a `size`-px run in the row
/// `[y0, y0 + row_h)`: `row_center + cap_height/2` (the leading-trim rule),
/// via the machine-checked fixed-point law
/// [`aterm_render::chrome_metrics::baseline_in_row_q`] — top and bottom gaps
/// balance within 1px for any `(row_h, cap)` with `cap <= row_h`. Mixed-size
/// runs on one visual row share the baseline computed from the DOMINANT size.
pub(crate) fn row_baseline(y0: f32, row_h: f32, size: f32) -> f32 {
    let cap_q = px_to_q(size * chrome_cap_ratio());
    q_to_px(baseline_in_row_q(px_to_q(y0), px_to_q(row_h), cap_q))
}

/// The baseline that centres a `size`-px run's CAP BOX on `cy` (a point
/// anchor — ring centres, dots): the `row_h = 0` degenerate of
/// [`row_baseline`].
pub(crate) fn baseline_centered_at(cy: f32, size: f32) -> f32 {
    row_baseline(cy, 0.0, size)
}

/// Real regular/semibold system UI faces.  Native chrome must never synthesize a
/// weight by re-striking pixels: it selects an installed face or honestly falls
/// back to the closest real face in the terminal stack.
#[derive(Clone, Default)]
struct UiFontAssets {
    regular: Option<Arc<fontdue::Font>>,
    semibold: Option<Arc<fontdue::Font>>,
    /// T3, the FULL cut for one caller: when the regular is a VARIABLE face
    /// with a `wght` axis (Segoe UI Variable on Win11), its raw bytes and the
    /// resolved semibold coords, so a pixel-space painter can instance the
    /// real wght-600 cut through `aterm_render::variation::varied_glyph_raster`
    /// instead of the static sibling. `None` for a static regular — and the
    /// bytes are retained ONLY in the variable case (macOS's Helvetica Neue
    /// collection is ~9 MB that nobody would read).
    variable_semibold: Option<UiVariableSemibold>,
}

/// The variable UI face instanced at SEMIBOLD, for the one painter that draws
/// outside fontdue (the Windows strip band's active label —
/// [`crate::tab_bar::pixel_band`]). fontdue has no variation API, so
/// `ChromeFonts::ui_semibold` above stays the static `seguisb.ttf`; this is the
/// honest way to the Win11 "Segoe UI Variable Semibold" without pairing the
/// variable file with itself (two wght-400 faces and no contrast).
#[derive(Clone)]
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct UiVariableSemibold {
    /// The whole font file (ttf-parser borrows it per raster).
    pub(crate) bytes: Arc<[u8]>,
    /// Collection index the regular was parsed at.
    pub(crate) index: u32,
    /// The `(tag, value)` instance coords — `wght` pulled to the Fluent
    /// semibold (600), clamped onto the face's own axis; every other axis
    /// (`opsz`) stays at its default, the Text optical size.
    pub(crate) coords: Vec<(u32, f32)>,
    /// The SAME regular face parsed from `bytes`: its cmap resolves glyph ids
    /// for the varied raster (identical glyph numbering — one file), and its
    /// `kern` pairs are the static kerning a weight instance shares.
    pub(crate) cmap: Arc<fontdue::Font>,
}

/// The CSS/OpenType weight the chrome's `UiBold` stands for (Fluent's
/// "Semibold" — `seguisb.ttf` is 600 too, so the static and variable paths
/// name the same weight).
const UI_SEMIBOLD_WGHT: f32 = 600.0;

/// Resolve [`UiVariableSemibold`] for a just-parsed regular, or `None` when the
/// face is static, has no `wght` axis, or its axis cannot reach a weight that
/// is visibly heavier than the regular (a face whose `wght` tops out near 400
/// would hand `UiBold` a look-alike — the exact contrast loss the "never pair
/// SegUIVar with itself" rule guards against, in another coat).
fn variable_semibold_of(
    bytes: Vec<u8>,
    index: u32,
    regular: &Arc<fontdue::Font>,
) -> Option<UiVariableSemibold> {
    use aterm_render::variation::{REGULAR_WGHT, WGHT_TAG, clamp_axis, probe};
    let probe = probe(&bytes, index)?;
    let wght = probe.axes.iter().find(|axis| axis.tag == WGHT_TAG)?;
    let weight = clamp_axis(wght, UI_SEMIBOLD_WGHT);
    if weight < REGULAR_WGHT + 100.0 {
        return None;
    }
    Some(UiVariableSemibold {
        bytes: bytes.into(),
        index,
        coords: vec![(WGHT_TAG, weight)],
        cmap: Arc::clone(regular),
    })
}

struct UiFontCandidate {
    regular_path: std::path::PathBuf,
    regular_index: u32,
    semibold_path: std::path::PathBuf,
    semibold_index: u32,
}

#[cfg(target_os = "macos")]
fn ui_font_candidates() -> Vec<UiFontCandidate> {
    vec![
        // NO SF ENTRY, and the reason is a property of the file rather than a
        // preference. `/System/Library/Fonts/SFNS.ttf` is NOT a TrueType
        // collection — its magic is `0001 0000` (a plain sfnt), not `ttcf` — so
        // it has exactly ONE face and no face index above 0 can ever resolve.
        // SF's weights live on an `fvar` WEIGHT AXIS (the file carries fvar/gvar/
        // avar/HVAR/MVAR/STAT), i.e. they are variable-font instances, and
        // fontdue parses only the default instance. There is no static SF
        // semibold on disk to point at either.
        //
        // This list previously led with `SFNS.ttf` face 0 + face 6 described as
        // "a TrueType collection whose normal-width semibold is face 6". Both
        // clauses were false, so `resolve_ui_font_assets` parsed face 0 (~81 ms),
        // failed face 6 (~26 ms), discarded BOTH because it requires a candidate
        // to supply regular AND semibold, and fell through to Helvetica Neue —
        // which is therefore what has always shipped. Removing the entry changes
        // no pixel; it stops paying ~107 ms per launch for a result that was
        // always thrown away.
        //
        // Restoring the documented intent (docs/INTROSPECTABLE_SURFACES_DESIGN.md
        // §9 says "SF Pro on macOS") needs variable-instance support in the UI
        // face path, not another candidate entry — that is a deliberate design
        // choice about the chrome's appearance, so it is left to the owner rather
        // than smuggled in as a perf fix.
        UiFontCandidate {
            regular_path: "/System/Library/Fonts/HelveticaNeue.ttc".into(),
            regular_index: 0,
            semibold_path: "/System/Library/Fonts/HelveticaNeue.ttc".into(),
            semibold_index: 10,
        },
    ]
}

#[cfg(windows)]
fn ui_font_candidates() -> Vec<UiFontCandidate> {
    let root = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
        .join("Fonts");
    vec![
        // T3 — the Win11 UI cut. `SegUIVar.ttf` is a variable font whose `fvar`
        // DEFAULT instance (wght 400, opsz 10.5) carries name id 17 "Regular" and
        // IS "Segoe UI Variable Text" — the exact face Win11 sets its own chrome
        // in at 12–23 px. fontdue parses only a variable font's default-instance
        // outlines, which here is precisely the instance we want, so pointing
        // `regular_path` at the file lands the Fluent regular with no variation
        // machinery at all.
        //
        // The fontdue SEMIBOLD deliberately stays the STATIC `seguisb.ttf`.
        // Pairing SegUIVar with itself would silently install TWO wght-400 faces
        // (`resolve_ui_font_assets` accepts any pair that parses — it cannot see
        // weights), and every `UiBold` heading would lose its contrast with no
        // error anywhere. fontdue has no variation API, so instancing wght 600
        // out of SegUIVar happens OUTSIDE it: the resolver also keeps the
        // variable file's bytes + the semibold coords (`UiVariableSemibold`),
        // and the one painter that draws through the portable varied rasterizer
        // — the strip band's active label — takes the real "Segoe UI Variable
        // Semibold". Every fontdue-drawn `UiBold` (Settings/About headings)
        // keeps the static Win10 semibold: the same weight, a sibling cut.
        UiFontCandidate {
            regular_path: root.join("SegUIVar.ttf"),
            regular_index: 0,
            semibold_path: root.join("seguisb.ttf"),
            semibold_index: 0,
        },
        // Win10 (no SegUIVar on disk): the static Segoe UI pair, exactly the
        // faces this platform's own chrome uses there.
        UiFontCandidate {
            regular_path: root.join("segoeui.ttf"),
            regular_index: 0,
            semibold_path: root.join("seguisb.ttf"),
            semibold_index: 0,
        },
    ]
}

#[cfg(target_os = "linux")]
fn ui_font_candidates() -> Vec<UiFontCandidate> {
    [
        // `resolve_ui_font_assets` accepts a candidate only when BOTH faces
        // parse, so each Noto pairing must name a file the distro actually
        // ships. fonts-noto-core on Debian/Ubuntu installs exactly the four
        // Regular/Bold (+Italics) cuts — there is NO NotoSans-SemiBold.ttf
        // there (that cut arrives only with the separate -extra package). The
        // SemiBold pair stays first for the distros that do carry it; the
        // Regular+Bold pair right behind it is what keeps stock Debian on
        // Noto at all. Without it every chrome surface silently fell through
        // to DejaVu Sans, ~8% wider, and each width the pages authored
        // against Noto turned into systemic truncation (2026-08 settings
        // audit: theme card, wallpaper, font credits, badges, search
        // placeholder).
        (
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-SemiBold.ttf",
        ),
        (
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
        ),
        // Arch/Fedora keep Noto under /usr/share/fonts/noto.
        (
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/noto/NotoSans-Bold.ttf",
        ),
        (
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        ),
        (
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
        ),
    ]
    .into_iter()
    .map(|(regular, semibold)| UiFontCandidate {
        regular_path: regular.into(),
        regular_index: 0,
        semibold_path: semibold.into(),
        semibold_index: 0,
    })
    .collect()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn ui_font_candidates() -> Vec<UiFontCandidate> {
    Vec::new()
}

/// Parse one UI face; the file bytes ride back beside it so the resolver can
/// keep them for a variable face ([`variable_semibold_of`]) — fontdue copies
/// what it parses and does not retain the buffer, so this is the only handle.
fn parse_ui_font(path: &std::path::Path, index: u32) -> Option<(Arc<fontdue::Font>, Vec<u8>)> {
    let bytes = std::fs::read(path).ok()?;
    let font = fontdue::Font::from_bytes(
        &bytes[..],
        fontdue::FontSettings {
            collection_index: index,
            ..fontdue::FontSettings::default()
        },
    )
    .ok()?;
    Some((Arc::new(font), bytes))
}

#[cfg(test)]
static UI_FONT_RESOLVE_ATTEMPTS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Cold host-preparation path. This is called only from `set_chrome_fonts`
/// after backend construction/reload, never from semantic compile, measure, or
/// raster. The parsed faces then remain immutable and Arc-shared.
fn resolve_ui_font_assets() -> UiFontAssets {
    #[cfg(test)]
    UI_FONT_RESOLVE_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut first_regular: Option<(Arc<fontdue::Font>, Vec<u8>, u32)> = None;
    for candidate in ui_font_candidates() {
        let regular = parse_ui_font(&candidate.regular_path, candidate.regular_index);
        let semibold = parse_ui_font(&candidate.semibold_path, candidate.semibold_index);
        match (regular, semibold) {
            (Some((regular, bytes)), Some((semibold, _))) => {
                let variable_semibold =
                    variable_semibold_of(bytes, candidate.regular_index, &regular);
                return UiFontAssets {
                    regular: Some(regular),
                    semibold: Some(semibold),
                    variable_semibold,
                };
            }
            (Some(regular), None) if first_regular.is_none() => {
                first_regular = Some((regular.0, regular.1, candidate.regular_index));
            }
            _ => {}
        }
    }
    match first_regular {
        Some((regular, bytes, index)) => {
            // No static semibold anywhere: a variable regular can still
            // instance its own — the band's active label keeps its weight even
            // where `UiBold`'s fontdue slot falls back to the regular.
            let variable_semibold = variable_semibold_of(bytes, index, &regular);
            UiFontAssets {
                regular: Some(regular),
                semibold: None,
                variable_semibold,
            }
        }
        None => UiFontAssets::default(),
    }
}

fn prepared_ui_font_assets() -> &'static UiFontAssets {
    static ASSETS: OnceLock<UiFontAssets> = OnceLock::new();
    ASSETS.get_or_init(resolve_ui_font_assets)
}

/// Populate the two process-global font `OnceLock`s that [`set_chrome_fonts`]
/// would otherwise initialise on whichever thread calls it first.
///
/// Both are pure, self-contained and idempotent — the system UI face parse
/// ([`prepared_ui_font_assets`]) and the embedded default chrome generation
/// ([`chrome_fonts`]) — so warming them costs the caller only its own time and
/// changes nothing about what is installed. Called on a dedicated startup thread
/// so the first `sync_chrome_fonts` (which runs on the first-present hook, on the
/// event loop) pays only the per-backend face handoff instead of a measured
/// ~300 ms debug / ~30 ms release stall with the event loop blocked.
#[cfg(not(test))]
pub(crate) fn warm_chrome_font_assets() {
    let _ = prepared_ui_font_assets();
    drop(lock_fonts());
}

/// Tests keep chrome fonts per-thread (see [`TestChromeFontsGuard`]), so a
/// process-global warm would be meaningless there — and the production warm is a
/// pure startup optimisation with no observable effect to test.
#[cfg(test)]
pub(crate) fn warm_chrome_font_assets() {}

/// The pixel tab strip's font-readiness fingerprint (Windows band —
/// [`crate::tab_bar::pixel_band`]): the chrome-face install epoch with the UI
/// regular's presence folded in. The strip band raster is cached on the GUI side
/// keyed on everything the pixels are a function of; the fonts are one of those
/// inputs, and they LAND ASYNCHRONOUSLY (backend construction installs them via
/// [`set_chrome_fonts`] after the first frames may already have painted). Folding
/// this value into the band's cache key makes the landing a cache miss, so the
/// frame that replaces the tofu-free mono fallback with real Segoe is the same
/// frame every other chrome surface re-rasters on — no bespoke invalidation hook.
///
/// `semantic_ready_epoch` moves on every [`install_chrome_faces_locked`] (and on
/// semantic-cascade landings, which merely cost one harmless band rebuild); the
/// presence bit covers the test seam (`prepare_ui_fonts_for_direct_view_test`)
/// which installs UI faces without bumping the epoch.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn strip_band_font_epoch() -> u64 {
    let fonts = lock_fonts();
    fonts
        .semantic_ready_epoch
        .wrapping_shl(1)
        .wrapping_add(u64::from(fonts.ui_regular.is_some()))
}

/// Is a real proportional UI face installed at all? The Windows strip band
/// declines wholesale ([`crate::tab_bar::pixel_band::raster_band`] → `None`)
/// until it is — the first frames before `set_chrome_fonts` lands (and every
/// unit test that never installs faces) keep the byte-identical cell strip
/// instead of a half-pixel band whose labels would be mono anyway.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn strip_band_ui_ready() -> bool {
    lock_fonts().ui_regular.is_some()
}

/// The variable semibold instance for the strip band's ACTIVE label, or `None`
/// when the host's UI regular is static (Win10 Segoe UI, Helvetica, Noto) —
/// the caller then draws `TextFace::UiBold` through the fontdue path (the
/// static `seguisb.ttf` sibling, or the regular where even that is absent).
///
/// Guarded on IDENTITY: handed out only while the INSTALLED regular is the very
/// face these bytes were parsed from (`Arc::ptr_eq` against the prepared
/// asset), so a test seam or a future per-window face swap can never pair
/// one file's cmap with another file's outlines.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn strip_band_variable_semibold() -> Option<UiVariableSemibold> {
    let installed = lock_fonts().ui_regular.clone()?;
    prepared_ui_font_assets()
        .variable_semibold
        .clone()
        .filter(|variable| Arc::ptr_eq(&variable.cmap, &installed))
}

/// Can the chrome stack draw EVERY char of `s` as real ink — the UI face first,
/// then the terminal cascade ([`select_chrome_face`]: real bold sibling / user
/// primary / embedded DejaVu)? This is exactly the per-char routing
/// [`Canvas::text`]'s proportional arm performs, asked ahead of time: a char that
/// fails BOTH is one the pen would silently skip (advance-only), which for a tab
/// TITLE means a hole where a glyph should be. The Windows strip band asks this
/// per label and honestly falls back to the cell-grid painter for any segment
/// that fails — a colour emoji or CJK title then renders through the terminal
/// renderer's full fallback/emoji machinery (mono-quantised, but REAL), instead
/// of vanishing from a proportional run.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn strip_band_run_coverable(s: &str) -> bool {
    let mut fonts = lock_fonts();
    if fonts.ui_regular.is_none() {
        return false;
    }
    let ui = fonts.ui_regular.clone();
    s.chars().all(|ch| {
        ch == ' '
            || ui
                .as_deref()
                .is_some_and(|font| font.lookup_glyph_index(ch) != 0)
            || fonts.face_for(TextWeight::Regular, ch).is_some()
    })
}

/// Slight negative tracking (em) applied per glyph advance of the UI face — SF reads a
/// touch loose at panel sizes when advanced naively. Applied IDENTICALLY by the raster
/// pen ([`Canvas::text`]) and the measure ([`ui_text_width`]), so they cannot drift —
/// and by the strip band's variable-instance pen (`tab_bar::pixel_band`), so an
/// active label's tracking matches its inactive neighbours'.
pub(crate) const UI_TRACKING_EM: f32 = -0.008;

/// Width of `s` at size `px` in the UI face: real per-glyph advances plus
/// `horizontal_kern` plus the same [`UI_TRACKING_EM`] the raster pen applies. Falls
/// back to the mono approximation (0.6 em per char — [`crate::settings::text_w`]) when
/// the UI face is absent, exactly the face [`Canvas::text`] falls back to — the
/// settings painter measures with THIS function for every UI-face string, so painter
/// and rasterizer can never disagree about proportional widths.
pub(crate) fn ui_text_width(s: &str, px: f32) -> f32 {
    ui_text_width_for(TextFace::Ui, s, px)
}

fn ui_text_wrap_ranges_impl(
    text: &str,
    px: f32,
    maximum_width: f32,
) -> (Vec<std::ops::Range<usize>>, usize) {
    let maximum_width = maximum_width.max(1.0);
    let graphemes = text
        .grapheme_indices()
        .map(|(byte, grapheme)| (byte, grapheme, grapheme.chars().all(char::is_whitespace)))
        .collect::<Vec<_>>();
    if graphemes.is_empty() {
        return (std::iter::once(0..0).collect(), 0);
    }

    let fonts = lock_fonts();
    let font = fonts.ui_font(TextFace::Ui).map(Arc::as_ref);
    let piece_width = |piece: &str, previous: Option<char>| {
        let Some(font) = font else {
            return (
                piece.chars().count() as f32 * px * 0.6,
                piece.chars().last().or(previous),
            );
        };
        let mut width = 0.0;
        let mut previous = previous;
        for character in piece.chars() {
            if let Some(previous) = previous {
                width += font.horizontal_kern(previous, character, px).unwrap_or(0.0);
            }
            width += font.metrics(character, px).advance_width + UI_TRACKING_EM * px;
            previous = Some(character);
        }
        (width, previous)
    };
    let exact_width = |piece: &str| {
        let Some(font) = font else {
            return piece.chars().count() as f32 * px * 0.6;
        };
        let mut width = 0.0;
        let mut previous: Option<char> = None;
        for character in piece.chars() {
            if let Some(previous) = previous {
                width += font.horizontal_kern(previous, character, px).unwrap_or(0.0);
            }
            width += font.metrics(character, px).advance_width + UI_TRACKING_EM * px;
            previous = Some(character);
        }
        width
    };

    let byte_at = |index: usize| {
        graphemes
            .get(index)
            .map_or(text.len(), |(byte, _, _)| *byte)
    };
    let mut ranges = Vec::new();
    let mut line_start = 0usize;
    let mut index = 0usize;
    let mut width = 0.0;
    let mut previous = None;
    let mut whitespace_break = None;
    let mut scans = 0usize;
    while index < graphemes.len() {
        let (_, grapheme, whitespace) = graphemes[index];
        let (advance, next) = piece_width(grapheme, previous);
        scans += 1;
        if index > line_start && width + advance > maximum_width {
            let split = whitespace_break
                .filter(|split| *split > line_start)
                .unwrap_or(index);
            let range = byte_at(line_start)..byte_at(split);
            debug_assert!(
                exact_width(&text[range.clone()]) <= maximum_width + 0.5 || split == line_start + 1
            );
            ranges.push(range);
            line_start = split;
            index = split;
            width = 0.0;
            previous = None;
            whitespace_break = None;
            continue;
        }
        width += advance;
        previous = next;
        index += 1;
        if whitespace {
            whitespace_break = Some(index);
        }
    }
    let range = byte_at(line_start)..text.len();
    debug_assert!(
        exact_width(&text[range.clone()]) <= maximum_width + 0.5
            || line_start + 1 == graphemes.len()
    );
    ranges.push(range);
    (ranges, scans)
}

/// Exact, whitespace-preserving UI-face line breaks. The returned ranges are
/// contiguous and cover `text` byte-for-byte; callers add visual newlines but
/// never normalize the source. A single font lock and bounded suffix rescan
/// keep the operation count linear for a near-cap unbroken token.
pub(crate) fn ui_text_wrap_ranges(
    text: &str,
    px: f32,
    maximum_width: f32,
) -> Vec<std::ops::Range<usize>> {
    ui_text_wrap_ranges_impl(text, px, maximum_width).0
}

/// Face-aware proportional measure.  Centered semibold labels use this when their
/// installed face has different advances from regular.
pub(crate) fn ui_text_width_for(face: TextFace, s: &str, px: f32) -> f32 {
    let fonts = lock_fonts();
    let Some(f) = fonts.ui_font(face).map(Arc::as_ref) else {
        return s.chars().count() as f32 * px * 0.6;
    };
    let mut w = 0.0;
    let mut prev: Option<char> = None;
    for ch in s.chars() {
        if let Some(p) = prev {
            w += f.horizontal_kern(p, ch, px).unwrap_or(0.0);
        }
        w += f.metrics(ch, px).advance_width + UI_TRACKING_EM * px;
        prev = Some(ch);
    }
    w
}

/// sRGB EOTF lookup: a gamma-encoded byte (0..=255) → linear-light (0..1), built
/// once. Mirrors the main renderer's `srgb_to_linear` (lib.rs) so the tray's
/// anti-aliasing composites in the SAME space as the terminal glyph path.
fn srgb_to_linear_lut() -> &'static [f32; 256] {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        std::array::from_fn(|i| {
            let c = i as f32 / 255.0;
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        })
    })
}

fn linear_to_srgb_u8_direct(l: f32) -> u8 {
    let l = l.clamp(0.0, 1.0);
    let encoded = if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Inverse sRGB OETF: linear-light (0..1) → a gamma-encoded byte (0..=255).
///
/// Raster fills call this for every channel of every covered pixel. Evaluating
/// `powf` there made a large translucent palette card take seconds in debug
/// builds. Quantization to an 8-bit result is exactly a monotonic threshold
/// search: output `n + 1` once linear light crosses the EOTF of the halfway
/// sRGB value `(n + 0.5) / 255`. Build those 255 thresholds once. A coarse
/// linear-light index supplies the answer or its immediate neighbour, then the
/// exact thresholds correct it; this preserves the rounded byte without doing
/// either a transcendental operation or an eight-step binary search per pixel.
fn linear_to_srgb_u8(l: f32) -> u8 {
    const BUCKETS: usize = 8_192;
    struct Quantizer {
        thresholds: [f32; 255],
        coarse: [u8; BUCKETS + 1],
    }

    let l = l.clamp(0.0, 1.0);
    static QUANTIZER: OnceLock<Quantizer> = OnceLock::new();
    let quantizer = QUANTIZER.get_or_init(|| {
        // Find the first representable non-negative f32 that the original OETF
        // rounds upward at each byte boundary. Searching float bit patterns is
        // monotonic on [0, 1] and preserves even the last-bit behaviour around
        // a halfway value; a closed-form inverse can differ there by one ulp.
        let thresholds = std::array::from_fn(|n| {
            let mut low = 0.0_f32.to_bits();
            let mut high = 1.0_f32.to_bits();
            while low < high {
                let mid = low + (high - low) / 2;
                if linear_to_srgb_u8_direct(f32::from_bits(mid)) > n as u8 {
                    high = mid;
                } else {
                    low = mid + 1;
                }
            }
            f32::from_bits(low)
        });
        let coarse = std::array::from_fn(|bucket| {
            let linear = bucket as f32 / BUCKETS as f32;
            thresholds.partition_point(|&threshold| linear >= threshold) as u8
        });
        Quantizer { thresholds, coarse }
    });
    let bucket = ((l * BUCKETS as f32).floor() as usize).min(BUCKETS);
    let mut encoded = usize::from(quantizer.coarse[bucket]);
    while encoded < quantizer.thresholds.len() && l >= quantizer.thresholds[encoded] {
        encoded += 1;
    }
    while encoded > 0 && l < quantizer.thresholds[encoded - 1] {
        encoded -= 1;
    }
    encoded as u8
}

/// A straight-alpha RGBA8 canvas with src-over compositing in LINEAR LIGHT (the
/// gamma-correct blend the main glyph path uses), plus a device-px clip stack.
type GlyphCacheKey = (usize, u32, char);
type GlyphCacheEntry = (fontdue::Metrics, std::sync::Arc<[u8]>);

struct Canvas {
    /// Device-pixel origin of this retained tile in the full tray. Raster math
    /// stays in full-surface coordinates so a regional pass makes the exact
    /// same AA/snapping decisions as a fresh full raster.
    origin_x: i32,
    origin_y: i32,
    w: u32,
    h: u32,
    px: Vec<u8>,
    /// Per-raster glyph memo. A native page repeats a small alphabet across many
    /// semantic text runs; fontdue intentionally does not cache rasterization.
    /// Keeping the bitmap for this one retained surface avoids re-walking the
    /// font's character map and outline for every repeated letter, while the
    /// cache drops with the canvas (no cross-font or unbounded process state).
    glyphs: std::collections::HashMap<GlyphCacheKey, GlyphCacheEntry>,
    /// Device-px clip stack; each entry is the running INTERSECTION as `(x0, y0,
    /// x1, y1)` exclusive. `blend` rejects pixels outside the top entry. Empty ⇒
    /// unclipped. Driven by `DrawPrim::ClipPush`/`ClipPop`.
    clip: Vec<(i32, i32, i32, i32)>,
}

impl Canvas {
    fn new(w: u32, h: u32, backdrop: [u8; 4]) -> Self {
        Self::with_origin(0, 0, w, h, backdrop)
    }

    fn with_origin(origin_x: i32, origin_y: i32, w: u32, h: u32, backdrop: [u8; 4]) -> Self {
        let mut px = vec![0u8; (w * h * 4) as usize];
        // Native retained surfaces deliberately start transparent. `vec![0]`
        // already produced that exact canvas; walking every pixel again was a
        // pure O(width × height) tax before the first primitive and dominated
        // debug WYSIWYG captures of a full-window native app.
        if backdrop != [0, 0, 0, 0] {
            for chunk in px.as_chunks_mut::<4>().0 {
                chunk.copy_from_slice(&backdrop);
            }
        }
        Self {
            origin_x,
            origin_y,
            w,
            h,
            px,
            glyphs: std::collections::HashMap::new(),
            clip: Vec::new(),
        }
    }

    fn device_bounds(&self) -> (i32, i32, i32, i32) {
        (
            self.origin_x,
            self.origin_y,
            self.origin_x
                .saturating_add(self.w.min(i32::MAX as u32) as i32),
            self.origin_y
                .saturating_add(self.h.min(i32::MAX as u32) as i32),
        )
    }

    fn clipped_bounds(
        &self,
        mut x0: i32,
        mut y0: i32,
        mut x1: i32,
        mut y1: i32,
    ) -> (i32, i32, i32, i32) {
        let (canvas_x0, canvas_y0, canvas_x1, canvas_y1) = self.device_bounds();
        x0 = x0.max(canvas_x0);
        y0 = y0.max(canvas_y0);
        x1 = x1.min(canvas_x1);
        y1 = y1.min(canvas_y1);
        if let Some(&(clip_x0, clip_y0, clip_x1, clip_y1)) = self.clip.last() {
            x0 = x0.max(clip_x0);
            y0 = y0.max(clip_y0);
            x1 = x1.min(clip_x1);
            y1 = y1.min(clip_y1);
        }
        (x0, y0, x1.max(x0), y1.max(y0))
    }

    fn raster_glyph(
        &mut self,
        font: &fontdue::Font,
        character: char,
        px: f32,
    ) -> (fontdue::Metrics, std::sync::Arc<[u8]>) {
        let key = (
            font as *const fontdue::Font as usize,
            px.to_bits(),
            character,
        );
        if let Some((metrics, bitmap)) = self.glyphs.get(&key) {
            return (*metrics, std::sync::Arc::clone(bitmap));
        }
        let (metrics, bitmap) = font.rasterize(character, px);
        let bitmap: std::sync::Arc<[u8]> = bitmap.into();
        self.glyphs
            .insert(key, (metrics, std::sync::Arc::clone(&bitmap)));
        (metrics, bitmap)
    }

    /// Intersect device-px rect `(x, y, w, h)` with the current clip and push it.
    fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let mut r = (
            x.floor() as i32,
            y.floor() as i32,
            (x + w).ceil() as i32,
            (y + h).ceil() as i32,
        );
        if let Some(&(cx0, cy0, cx1, cy1)) = self.clip.last() {
            r.0 = r.0.max(cx0);
            r.1 = r.1.max(cy0);
            r.2 = r.2.min(cx1);
            r.3 = r.3.min(cy1);
        }
        self.clip.push(r);
    }

    /// Pop the most recent clip. An unbalanced pop is a harmless no-op.
    fn pop_clip(&mut self) {
        self.clip.pop();
    }

    fn active_clip_intersects_tile(&self) -> bool {
        let Some(&(clip_x0, clip_y0, clip_x1, clip_y1)) = self.clip.last() else {
            return true;
        };
        let (tile_x0, tile_y0, tile_x1, tile_y1) = self.device_bounds();
        clip_x1 > tile_x0 && clip_y1 > tile_y0 && clip_x0 < tile_x1 && clip_y0 < tile_y1
    }

    /// Composite `c` (straight alpha) over pixel `(x, y)` with extra `cov` (0..1) AA,
    /// in LINEAR-LIGHT space (the gamma-correct blend the main glyph path uses), and
    /// honoring the clip stack.
    fn blend(&mut self, x: i32, y: i32, c: [u8; 4], cov: f32) {
        let local_x = x.saturating_sub(self.origin_x);
        let local_y = y.saturating_sub(self.origin_y);
        if local_x < 0 || local_y < 0 || local_x as u32 >= self.w || local_y as u32 >= self.h {
            return;
        }
        // Clip: reject pixels outside the current intersection rect (exclusive).
        if let Some(&(cx0, cy0, cx1, cy1)) = self.clip.last()
            && (x < cx0 || y < cy0 || x >= cx1 || y >= cy1)
        {
            return;
        }
        let a = (f32::from(c[3]) / 255.0) * cov.clamp(0.0, 1.0);
        if a <= 0.0 {
            return;
        }
        let i = ((local_y as u32 * self.w + local_x as u32) * 4) as usize;
        // A fully covered opaque source replaces the destination exactly. The
        // general linear-light source-over equation reduces to this copy, so do
        // not pay three lookup/searches for the large opaque surfaces used by
        // native views and modal cards.
        if c[3] == u8::MAX && cov >= 1.0 {
            self.px[i..i + 4].copy_from_slice(&c);
            return;
        }
        let da = f32::from(self.px[i + 3]) / 255.0;
        // Equal straight-alpha colours remain that colour under source-over;
        // only alpha changes. This is the common full-coverage shadow case
        // (black over transparent/black) and avoids needless colour transfer.
        if cov >= 1.0 && self.px[i..i + 3] == c[..3] {
            let out_a = a + da * (1.0 - a);
            self.px[i + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
            return;
        }
        // STRAIGHT-alpha src-over: out_a = a + da·(1−a); the RGB mix is done in
        // LINEAR LIGHT — out_lin = (s_lin·a + d_lin·da·(1−a))/out_a — then re-encoded
        // to sRGB. This both fixes the premultiplied-over-sub-opaque-backdrop darkening
        // (the straight-alpha out_a) AND matches the main renderer (which composites
        // coverage in linear light, lib.rs srgb_to_linear/linear_to_srgb), so a
        // 50%-covered black glyph edge over white lands at sRGB ~0xBC, not the
        // gamma-space 0x80 that smears thin text. Opaque output stays consistent.
        let out_a = a + da * (1.0 - a);
        if out_a > 0.0 {
            let lut = srgb_to_linear_lut();
            for (k, &sc) in c.iter().take(3).enumerate() {
                let s_lin = lut[usize::from(sc)];
                let d_lin = lut[usize::from(self.px[i + k])];
                let out_lin = s_lin.mul_add(a, d_lin * da * (1.0 - a)) / out_a;
                self.px[i + k] = linear_to_srgb_u8(out_lin);
            }
        }
        self.px[i + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
    }

    /// Apply a premultiplied-light pixel with the renderer's exact saturating
    /// RGB operation. Settings live previews paint an opaque terminal canvas
    /// before any effect primitive, so alpha remains the canvas alpha while
    /// the RGB result matches `aterm_render::add_sat` byte for byte.
    fn add_premul(&mut self, x: i32, y: i32, premul: u32) {
        let local_x = x.saturating_sub(self.origin_x);
        let local_y = y.saturating_sub(self.origin_y);
        if local_x < 0 || local_y < 0 || local_x as u32 >= self.w || local_y as u32 >= self.h {
            return;
        }
        if let Some(&(cx0, cy0, cx1, cy1)) = self.clip.last()
            && (x < cx0 || y < cy0 || x >= cx1 || y >= cy1)
        {
            return;
        }
        let i = ((local_y as u32 * self.w + local_x as u32) * 4) as usize;
        let dst = (u32::from(self.px[i]) << 16)
            | (u32::from(self.px[i + 1]) << 8)
            | u32::from(self.px[i + 2]);
        let out = aterm_render::add_sat(dst, premul);
        self.px[i] = ((out >> 16) & 0xff) as u8;
        self.px[i + 1] = ((out >> 8) & 0xff) as u8;
        self.px[i + 2] = (out & 0xff) as u8;
    }

    fn over_packed(&mut self, x: i32, y: i32, rgb: u32, alpha: u8) {
        let local_x = x.saturating_sub(self.origin_x);
        let local_y = y.saturating_sub(self.origin_y);
        if local_x < 0 || local_y < 0 || local_x as u32 >= self.w || local_y as u32 >= self.h {
            return;
        }
        if let Some(&(cx0, cy0, cx1, cy1)) = self.clip.last()
            && (x < cx0 || y < cy0 || x >= cx1 || y >= cy1)
        {
            return;
        }
        let i = ((local_y as u32 * self.w + local_x as u32) * 4) as usize;
        let dst = (u32::from(self.px[i]) << 16)
            | (u32::from(self.px[i + 1]) << 8)
            | u32::from(self.px[i + 2]);
        let out = aterm_render::over_rgb(dst, rgb, alpha);
        self.px[i] = ((out >> 16) & 0xff) as u8;
        self.px[i + 1] = ((out >> 8) & 0xff) as u8;
        self.px[i + 2] = (out & 0xff) as u8;
    }

    fn additive_rect(&mut self, x: f32, y: f32, w: f32, h: f32, premul: u32) {
        let (x0, y0, x1, y1) = self.clipped_bounds(
            x.floor() as i32,
            y.floor() as i32,
            (x + w).ceil() as i32,
            (y + h).ceil() as i32,
        );
        for py in y0..y1 {
            for px in x0..x1 {
                self.add_premul(px, py, premul);
            }
        }
    }

    fn effect_halo(
        &mut self,
        halo: aterm_render::RainHalo,
        offset_x: f32,
        offset_y: f32,
        scale: f32,
    ) {
        use aterm_render::HaloMode;

        let x = ((f32::from(halo.x) + offset_x) * scale).round() as i32;
        let y = ((f32::from(halo.y) + offset_y) * scale).round() as i32;
        let w = (f32::from(halo.w) * scale).round().max(1.0) as i32;
        let h = (f32::from(halo.h) * scale).round().max(1.0) as i32;
        let cx = ((f32::from(halo.cx) + offset_x) * scale).round() as i32;
        let cy = ((f32::from(halo.cy) + offset_y) * scale).round() as i32;
        let rx = (f32::from(halo.rx) * scale).round().max(1.0) as i32;
        let ry = (f32::from(halo.ry) * scale).round().max(1.0) as i32;
        let (x0, y0, x1, y1) = self.clipped_bounds(x, y, x + w, y + h);
        let (rx2, ry2) = (rx * rx, ry * ry);
        for py in y0..y1 {
            let ny = aterm_render::halo_row_ny(py - cy, ry2);
            if ny >= 256 {
                continue;
            }
            for px in x0..x1 {
                let weight = aterm_render::halo_weight(px - cx, ny, rx2).clamp(0, 255) as u8;
                if weight == 0 {
                    continue;
                }
                match halo.mode {
                    HaloMode::Add => {
                        self.add_premul(px, py, aterm_render::premul_rgb(halo.color, weight));
                    }
                    HaloMode::Over => self.over_packed(px, py, halo.color, weight),
                }
            }
        }
    }

    fn effect_fire(
        &mut self,
        patch: aterm_render::FirePatch,
        offset_x: f32,
        offset_y: f32,
        scale: f32,
    ) {
        use aterm_render::FireMode;
        use aterm_render::fire_field::{self, FireFieldParams};

        let x = ((f32::from(patch.x) + offset_x) * scale).round() as i32;
        let y = ((f32::from(patch.y) + offset_y) * scale).round() as i32;
        let w = (f32::from(patch.w) * scale).round().max(1.0) as i32;
        let h = (f32::from(patch.h) * scale).round().max(1.0) as i32;
        let params = FireFieldParams {
            base_y: ((f32::from(patch.base_y) + offset_y) * scale).round() as i32,
            peak_h: (f32::from(patch.peak_h) * scale).round().max(1.0) as i32,
            phase: patch.phase,
            temp: i32::from(patch.temp),
            strength: i32::from(patch.strength),
            lean: (f32::from(patch.lean) * scale).round() as i32,
            cov_cap: i32::from(patch.cov_cap),
            cell_h: (f32::from(patch.cell_h) * scale).round().max(2.0) as i32,
            top_fade_y: (offset_y * scale).round() as i32,
        };
        let precomputed = fire_field::fire_precomp(&params);
        let (x0, y0, x1, y1) = self.clipped_bounds(x, y, x + w, y + h);
        for py in y0..y1 {
            let top_fade = fire_field::fire_top_fade(py, &params);
            let mut row = fire_field::FireRow::new(py, x0, &params, &precomputed);
            for px in x0..x1 {
                let core = row.core(px);
                match patch.mode {
                    FireMode::Add => {
                        let premul = fire_field::fire_shade_add(&core, &params, top_fade);
                        if premul != 0 {
                            self.add_premul(px, py, premul);
                        }
                    }
                    FireMode::Over => {
                        let (rgb, alpha) = fire_field::fire_shade_over(&core, &params, top_fade);
                        if alpha != 0 {
                            self.over_packed(px, py, rgb, alpha);
                        }
                    }
                }
            }
        }
    }

    /// PNG bytes of this canvas. TEST-ONLY: the two preview escape hatches
    /// (`ATERM_TRAY_PREVIEW`, `ATERM_TRAY_THEMES`) are the only callers, so the
    /// shipping binary does not carry the encoder path. Gated rather than
    /// `#[allow]`-ed, so it goes dead loudly if those previews are removed.
    #[cfg(test)]
    fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, self.w, self.h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            if let Ok(mut wr) = enc.write_header() {
                let _ = wr.write_image_data(&self.px);
            }
        }
        out
    }
}

/// Signed distance to a rounded box centered at `(cx, cy)` with half-extents `(hx, hy)`
/// and corner radius `r`. Negative inside.
fn sd_round_box(px: f32, py: f32, cx: f32, cy: f32, hx: f32, hy: f32, r: f32) -> f32 {
    let qx = (px - cx).abs() - (hx - r);
    let qy = (py - cy).abs() - (hy - r);
    let ax = qx.max(0.0);
    let ay = qy.max(0.0);
    (ax * ax + ay * ay).sqrt() + qx.max(qy).min(0.0) - r
}

/// Coverage (0..1) from a signed distance in pixels: 1 inside, AA over ~1px at the edge.
fn cov(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

/// How many rendered terminal specimens one thread retains. A Settings page
/// paints one specimen per live preview, so two entries make the animating
/// steady state a pure hit (and still absorb a second window) while keeping the
/// retained pixel buffers bounded.
const SPECIMEN_FRAME_CACHE_ENTRIES: usize = 2;

/// One retained specimen render.
struct SpecimenFrameEntry {
    /// Fingerprint of every renderer input except the fork source.
    key: u64,
    /// The exact `Rc` the frame was forked from, held WEAKLY on purpose: a weak
    /// handle keeps the `Rc` allocation reserved — so no later fork can be
    /// handed the same address and silently alias this entry — without keeping
    /// the forked `Renderer` (and its parsed faces) alive.
    source: std::rc::Weak<Renderer>,
    frame: Rc<aterm_render::Frame>,
}

thread_local! {
    /// Most-recently-used first. Thread-local because `Renderer`/`Frame` travel
    /// by `Rc` here, and because it then matches the per-thread test font
    /// store: a libtest worker can never observe another worker's entry.
    static SPECIMEN_FRAMES: std::cell::RefCell<Vec<SpecimenFrameEntry>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A TOTAL fingerprint of everything [`Canvas::terminal_specimen`] feeds the
/// renderer, except the two identities matched separately: the fork source
/// (compared by pointer beside this key) and the engine snapshot (represented
/// by `input_fingerprint`, which `settings_preview::build_terminal_specimen_input`
/// builds as a total function of the snapshot it returns).
///
/// Every field must appear here. Omitting one — a variation axis, `stem_gamma`,
/// the device scale — would paint stale pixels after a settings tweak, which is
/// exactly the WYSIWYG contract this specimen exists to honour.
fn terminal_specimen_frame_key(spec: &TerminalSpecimenSpec, scale: f32) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hash = std::collections::hash_map::DefaultHasher::new();
    spec.input_fingerprint.hash(&mut hash);
    // The fork source's generation. Pointer identity proves it is the same
    // `Rc`; the epoch additionally moves on every host transition that changes
    // what a fork of that generation contains.
    spec.prepared_font.snapshot.ready_epoch.hash(&mut hash);
    // `Theme` is not `Hash`; these four colours are the whole value.
    spec.theme.fg.hash(&mut hash);
    spec.theme.bg.hash(&mut hash);
    spec.theme.cursor.hash(&mut hash);
    spec.theme.selection.hash(&mut hash);
    // `scale` is keyed alongside `font_px` because it reaches `activate_px` and
    // the three rounded `* scale` metric adjustments below it: a fractional or
    // Retina scale change re-rasterizes even at an identical logical size.
    scale.to_bits().hash(&mut hash);
    spec.font_px.to_bits().hash(&mut hash);
    spec.line_height.to_bits().hash(&mut hash);
    spec.baseline_adjust.hash(&mut hash);
    spec.underline_position.hash(&mut hash);
    spec.underline_thickness.hash(&mut hash);
    spec.underline_skip_descenders.hash(&mut hash);
    spec.synthetic_styles.hash(&mut hash);
    spec.text_blending.hash(&mut hash);
    spec.font_thicken.hash(&mut hash);
    spec.stem_gamma.to_bits().hash(&mut hash);
    // `SemanticVariation` stores its value as bits, so the whole request hashes
    // exactly.
    spec.variations.hash(&mut hash);
    spec.ligatures.hash(&mut hash);
    spec.merged_ligatures.hash(&mut hash);
    spec.cursor_break_ligatures.hash(&mut hash);
    spec.minimum_contrast.to_bits().hash(&mut hash);
    spec.selection_foreground.hash(&mut hash);
    spec.selection_inactive.hash(&mut hash);
    hash.finish()
}

/// The retained frame for `key` forked from exactly `source`, promoted to
/// most-recently-used.
fn cached_specimen_frame(
    key: u64,
    source: Option<&Rc<Renderer>>,
) -> Option<Rc<aterm_render::Frame>> {
    let source = source?;
    SPECIMEN_FRAMES.with(|slot| {
        let mut entries = slot.borrow_mut();
        let hit = entries.iter().position(|entry| {
            entry.key == key && std::ptr::eq(entry.source.as_ptr(), Rc::as_ptr(source))
        })?;
        let entry = entries.remove(hit);
        let frame = Rc::clone(&entry.frame);
        entries.insert(0, entry);
        Some(frame)
    })
}

fn retain_specimen_frame(key: u64, source: &Rc<Renderer>, frame: &Rc<aterm_render::Frame>) {
    SPECIMEN_FRAMES.with(|slot| {
        let mut entries = slot.borrow_mut();
        entries.insert(
            0,
            SpecimenFrameEntry {
                key,
                source: Rc::downgrade(source),
                frame: Rc::clone(frame),
            },
        );
        entries.truncate(SPECIMEN_FRAME_CACHE_ENTRIES);
    });
}

impl Canvas {
    fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: [u8; 4]) {
        let (cx, cy, hx, hy) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
        let r = r.min(hx).min(hy);
        let mut x0 = (x - 1.0).floor() as i32;
        let mut y0 = (y - 1.0).floor() as i32;
        let mut x1 = (x + w + 1.0).ceil() as i32;
        let mut y1 = (y + h + 1.0).ceil() as i32;
        (x0, y0, x1, y1) = self.clipped_bounds(x0, y0, x1, y1);

        // A rounded box inset by one pixel is conservatively inside the region
        // whose analytic coverage is exactly 1. Fill that wide scanline span
        // without evaluating an SDF/sqrt for every interior pixel; retain the
        // original SDF on the complete edge and corner fringe, so antialiasing
        // and byte output stay unchanged.
        let inset = 1.0_f32;
        let inner_w = w - inset * 2.0;
        let inner_h = h - inset * 2.0;
        let inner_r = (r - inset).max(0.0).min(inner_w / 2.0).min(inner_h / 2.0);
        let inner_x = x + inset;
        let inner_y = y + inset;
        for py in y0..y1 {
            let py_center = py as f32 + 0.5;
            let full_span = if inner_w > 0.0
                && inner_h > 0.0
                && py_center >= inner_y
                && py_center <= inner_y + inner_h
            {
                let (left, right) = if inner_r <= 0.0
                    || (py_center >= inner_y + inner_r && py_center <= inner_y + inner_h - inner_r)
                {
                    (inner_x, inner_x + inner_w)
                } else {
                    let corner_y = if py_center < inner_y + inner_r {
                        inner_y + inner_r
                    } else {
                        inner_y + inner_h - inner_r
                    };
                    let dy = (py_center - corner_y).abs();
                    let extent = (inner_r * inner_r - dy * dy).max(0.0).sqrt();
                    (
                        inner_x + inner_r - extent,
                        inner_x + inner_w - inner_r + extent,
                    )
                };
                let start = ((left - 0.5).ceil() as i32).clamp(x0, x1);
                let end = (((right - 0.5).floor() as i32).saturating_add(1)).clamp(x0, x1);
                (start < end).then_some((start, end))
            } else {
                None
            };

            let (left_end, right_start) = full_span.unwrap_or((x1, x1));
            for px in x0..left_end {
                let d = sd_round_box(px as f32 + 0.5, py as f32 + 0.5, cx, cy, hx, hy, r);
                self.blend(px, py, c, cov(d));
            }
            if let Some((start, end)) = full_span {
                for px in start..end {
                    self.blend(px, py, c, 1.0);
                }
            }
            for px in right_start..x1 {
                let d = sd_round_box(px as f32 + 0.5, py as f32 + 0.5, cx, cy, hx, hy, r);
                self.blend(px, py, c, cov(d));
            }
        }
    }

    fn disc(&mut self, cx: f32, cy: f32, r: f32, c: [u8; 4]) {
        let (x0, y0, x1, y1) = self.clipped_bounds(
            (cx - r - 1.0) as i32,
            (cy - r - 1.0) as i32,
            (cx + r + 1.0) as i32,
            (cy + r + 1.0) as i32,
        );
        for py in y0..y1 {
            for px in x0..x1 {
                let dx = px as f32 + 0.5 - cx;
                let dy = py as f32 + 0.5 - cy;
                let d = (dx * dx + dy * dy).sqrt() - r;
                self.blend(px, py, c, cov(d));
            }
        }
    }

    /// Raster a round-capped anti-aliased segment from the signed distance to
    /// its finite center line. This is deliberately a small shared primitive,
    /// not an icon bitmap: regional and full native rasters therefore produce
    /// identical arrows at every display scale.
    fn line_segment(&mut self, start: (f32, f32), end: (f32, f32), width: f32, c: [u8; 4]) {
        let radius = width.max(0.0) / 2.0;
        if radius <= 0.0 {
            return;
        }
        let (x1, y1) = start;
        let (x2, y2) = end;
        let vx = x2 - x1;
        let vy = y2 - y1;
        let length_squared = vx.mul_add(vx, vy * vy);
        if length_squared <= f32::EPSILON {
            self.disc(x1, y1, radius, c);
            return;
        }
        let (x0, y0, x3, y3) = self.clipped_bounds(
            (x1.min(x2) - radius - 1.0).floor() as i32,
            (y1.min(y2) - radius - 1.0).floor() as i32,
            (x1.max(x2) + radius + 1.0).ceil() as i32,
            (y1.max(y2) + radius + 1.0).ceil() as i32,
        );
        for py in y0..y3 {
            for px in x0..x3 {
                let sample_x = px as f32 + 0.5;
                let sample_y = py as f32 + 0.5;
                let projection = ((sample_x - x1).mul_add(vx, (sample_y - y1) * vy)
                    / length_squared)
                    .clamp(0.0, 1.0);
                let nearest_x = x1 + projection * vx;
                let nearest_y = y1 + projection * vy;
                let dx = sample_x - nearest_x;
                let dy = sample_y - nearest_y;
                self.blend(px, py, c, cov(dx.mul_add(dx, dy * dy).sqrt() - radius));
            }
        }
    }

    /// The HSV colour disk ([`DrawPrim::HsvDisk`]): per pixel, angle → hue (0 at
    /// 12 o'clock, clockwise — the same angular convention as `arc`) and radius →
    /// saturation, both through the shared [`crate::widget::hsv_to_rgb`] scaled by
    /// `value`, with the same ~1px anti-aliased rim as `disc`. Pixels past the rim
    /// are untouched.
    fn hsv_disk(&mut self, cx: f32, cy: f32, r: f32, value: f32) {
        if r <= 0.0 {
            return;
        }
        let (x0, y0, x1, y1) = self.clipped_bounds(
            (cx - r - 1.0) as i32,
            (cy - r - 1.0) as i32,
            (cx + r + 1.0) as i32,
            (cy + r + 1.0) as i32,
        );
        for py in y0..y1 {
            for px in x0..x1 {
                let dx = px as f32 + 0.5 - cx;
                let dy = py as f32 + 0.5 - cy;
                let rr = (dx * dx + dy * dy).sqrt();
                let a = cov(rr - r);
                if a <= 0.0 {
                    continue;
                }
                // Clockwise turns from 12 o'clock (matches `arc`); centre = grey axis.
                let h = dx.atan2(-dy).rem_euclid(TAU) / TAU;
                let s = (rr / r).min(1.0);
                let rgb = crate::widget::hsv_to_rgb(h, s, value);
                self.blend(px, py, [rgb[0], rgb[1], rgb[2], 255], a);
            }
        }
    }

    /// An anti-aliased arc band: radial distance to the `r_mid` circle within
    /// `thickness`, masked to the angular span `[0, frac]` clockwise from 12 o'clock.
    /// `frac >= 1.0` draws the full ring (capacity track).
    fn arc(&mut self, cx: f32, cy: f32, r_mid: f32, thickness: f32, frac: f32, c: [u8; 4]) {
        let frac = frac.clamp(0.0, 1.0);
        if frac <= 0.0 {
            return;
        }
        let r_out = r_mid + thickness / 2.0;
        let (x0, y0, x1, y1) = self.clipped_bounds(
            (cx - r_out - 1.0) as i32,
            (cy - r_out - 1.0) as i32,
            (cx + r_out + 1.0) as i32,
            (cy + r_out + 1.0) as i32,
        );
        for py in y0..y1 {
            for px in x0..x1 {
                let dx = px as f32 + 0.5 - cx;
                let dy = py as f32 + 0.5 - cy;
                let rr = (dx * dx + dy * dy).sqrt();
                let radial = (rr - r_mid).abs() - thickness / 2.0;
                let cov_r = cov(radial);
                if cov_r <= 0.0 {
                    continue;
                }
                let mut cov_a = 1.0;
                if frac < 1.0 {
                    // clockwise fraction from 12 o'clock
                    let a = dx.atan2(-dy).rem_euclid(TAU) / TAU;
                    let d_frac = if a <= frac {
                        -(a.min(frac - a)) // inside (negative)
                    } else {
                        (a - frac).min(1.0 - a) // outside (positive)
                    };
                    let d_px = d_frac * TAU * rr.max(1.0);
                    cov_a = cov(d_px);
                }
                self.blend(px, py, c, cov_r * cov_a);
            }
        }
    }

    /// Draw `s` with its BASELINE at `baseline` (device px). `face` selects the
    /// render font. `Mono` draws in the chrome terminal stack: per-char face pick
    /// (bold sibling / user primary / DejaVu coverage fallback —
    /// [`select_chrome_face`]), a 26.6 fixed-point pen with per-glyph rounding
    /// (placement error ≤ 0.5 px, drift-free — `chrome_metrics` proofs). `Ui`/
    /// `UiBold` draw in real regular/semibold native system faces (SF Pro, Segoe UI,
    /// Noto Sans, or DejaVu Sans; the terminal stack is the final fallback) with
    /// real kerning + [`UI_TRACKING_EM`] tracking. Both paths place glyphs by their
    /// bearings relative to the caller-supplied baseline.
    #[allow(
        clippy::too_many_arguments,
        reason = "primitive text blit: font stack + pen origin/baseline + run + size + \
                  weight + face + color are all irreducible inputs; bundling them into a \
                  struct would only move the argument count to the one call site (the Text arm)"
    )]
    fn text(
        &mut self,
        fonts: &mut ChromeFonts,
        x: f32,
        baseline: f32,
        s: &str,
        px: f32,
        weight: TextWeight,
        face: TextFace,
        c: [u8; 4],
    ) {
        if face == TextFace::Mono {
            // Chrome terminal stack: 26.6 fixed-point pen, per-glyph rounding,
            // glyphs placed by their bearings under the snapped baseline.
            let mut pen_q = px_to_q(x);
            let baseline_px = q_round_to_px(px_to_q(baseline)) as i32;
            for ch in s.chars() {
                let Some(cface) = fonts.face_for(weight, ch) else {
                    continue;
                };
                let (m, bitmap) = self.raster_glyph(&cface.font, ch, px);
                if m.width > 0 && m.height > 0 {
                    let gx = q_round_to_px(pen_q) as i32 + m.xmin;
                    let gy = baseline_px - (m.height as i32 + m.ymin);
                    for gyy in 0..m.height {
                        for gxx in 0..m.width {
                            let a = f32::from(bitmap[gyy * m.width + gxx]) / 255.0;
                            if a > 0.0 {
                                self.blend(gx + gxx as i32, gy + gyy as i32, c, a);
                            }
                        }
                    }
                }
                pen_q += px_to_q(m.advance_width);
            }
            return;
        }
        // Native proportional UI face with PER-CHARACTER coverage fallback.
        // Fontdue will happily rasterize glyph 0 as a visible tofu box, so a
        // run-level face choice is not sufficient: keep the real UI face for
        // every covered character, and route only each miss through the terminal
        // cascade. This is what keeps labels proportional while arrows/CJK/etc.
        // remain genuine glyphs rather than `.notdef`.
        let ui = fonts.ui_font(face).cloned();
        let baseline_i = q_round_to_px(px_to_q(baseline)) as i32 as f32;
        let mut pen = x;
        let mut prev_ui: Option<char> = None;
        for ch in s.chars() {
            if let Some(font) = ui
                .as_deref()
                .filter(|font| font.lookup_glyph_index(ch) != 0)
            {
                if let Some(previous) = prev_ui {
                    pen += font.horizontal_kern(previous, ch, px).unwrap_or(0.0);
                }
                let (m, bitmap) = self.raster_glyph(font, ch, px);
                self.blit_fontdue_glyph(pen, baseline_i, &m, &bitmap, c);
                pen += m.advance_width + UI_TRACKING_EM * px;
                prev_ui = Some(ch);
                continue;
            }
            prev_ui = None;

            if let Some(cface) = fonts.face_for(weight, ch) {
                let (m, bitmap) = self.raster_glyph(&cface.font, ch, px);
                self.blit_fontdue_glyph(pen, baseline_i, &m, &bitmap, c);
                pen += m.advance_width;
                continue;
            }

            // Preserve following-label placement without ever drawing `.notdef`.
            pen += px * 0.6;
        }
    }

    /// Render a complete bounded terminal snapshot with the shipping CPU
    /// renderer, then composite its exact opaque frame into the semantic card.
    fn terminal_specimen(&mut self, x: f32, y: f32, spec: &TerminalSpecimenSpec, scale: f32) {
        // This is by far the most expensive prim in the tray: the fork below is
        // a full `fontdue` re-parse of the primary face (and a second one for
        // its real bold sibling) inside `fork_semantic_surface`, and
        // `render_input` then renders a whole mini terminal frame from cold
        // glyph/layout caches. An animating Settings preview re-rasterizes at
        // frame rate with every one of those inputs unchanged, so the finished
        // frame is memoized instead of recomputed.
        //
        // A hit is the same computation replayed: the key is total over what
        // reaches the renderer, the fork source is matched by POINTER identity
        // rather than by value, and the frame itself does not depend on `x`/`y`
        // (the blit below is a pure read that places it).
        let key = terminal_specimen_frame_key(spec, scale);
        let source = spec.prepared_font.renderer.as_ref();
        if let Some(frame) = cached_specimen_frame(key, source) {
            self.blit_terminal_frame(
                &frame,
                (x * scale).round() as i32,
                (y * scale).round() as i32,
            );
            return;
        }
        let Some(mut renderer) = spec.prepared_font.fork(spec.font_px * scale, spec.theme) else {
            return;
        };
        renderer.set_pad(0);
        renderer.set_head(0);
        renderer.set_line_height(spec.line_height);
        renderer.set_adjust_baseline((spec.baseline_adjust as f32 * scale).round() as i32);
        renderer.set_adjust_underline(
            (spec.underline_position as f32 * scale).round() as i32,
            (spec.underline_thickness as f32 * scale).round() as i32,
        );
        renderer.set_underline_skip_descenders(spec.underline_skip_descenders);
        renderer.set_synthetic_styles(spec.synthetic_styles);
        renderer.set_text_blending(match spec.text_blending {
            SpecimenTextBlending::Linear => aterm_render::TextBlending::Linear,
            SpecimenTextBlending::LinearCorrected => aterm_render::TextBlending::LinearCorrected,
        });
        renderer.set_font_thicken(spec.font_thicken);
        renderer.set_stem_gamma(spec.stem_gamma);
        let variations = spec
            .variations
            .iter()
            .map(|variation| (variation.tag, variation.value()))
            .collect::<Vec<_>>();
        let _ = renderer.set_font_variations(&variations, 0.0);
        let ligature_mode = if !spec.ligatures {
            aterm_types::text_shaping::LigatureMode::Disabled
        } else if spec.cursor_break_ligatures {
            aterm_types::text_shaping::LigatureMode::CursorDisabled
        } else {
            aterm_types::text_shaping::LigatureMode::Enabled
        };
        renderer.set_text_shaping(aterm_types::text_shaping::TextShapingConfig {
            ligature_mode,
            admit_collapsed: spec.merged_ligatures,
            ..aterm_types::text_shaping::TextShapingConfig::default()
        });
        renderer.set_minimum_contrast(spec.minimum_contrast);
        renderer.set_selection_fg(spec.selection_foreground);
        renderer.set_selection_inactive(spec.selection_inactive);
        renderer.activate_px(spec.font_px * scale);
        let frame = Rc::new(renderer.render_input(&spec.input));
        if let Some(source) = source {
            retain_specimen_frame(key, source, &frame);
        }
        self.blit_terminal_frame(
            &frame,
            (x * scale).round() as i32,
            (y * scale).round() as i32,
        );
    }

    fn blit_terminal_frame(&mut self, frame: &aterm_render::Frame, x: i32, y: i32) {
        if frame.width == 0 {
            return;
        }
        for (row, pixels) in frame.pixels.chunks(frame.width).enumerate() {
            for (column, pixel) in pixels.iter().copied().enumerate() {
                self.blend(
                    x.saturating_add(column as i32),
                    y.saturating_add(row as i32),
                    [
                        ((pixel >> 16) & 0xff) as u8,
                        ((pixel >> 8) & 0xff) as u8,
                        (pixel & 0xff) as u8,
                        255,
                    ],
                    1.0,
                );
            }
        }
    }

    fn blit_fontdue_glyph(
        &mut self,
        pen: f32,
        baseline: f32,
        metrics: &fontdue::Metrics,
        bitmap: &[u8],
        color: [u8; 4],
    ) {
        let gx = pen + metrics.xmin as f32;
        let gy = baseline - (metrics.height as f32 + metrics.ymin as f32);
        for y in 0..metrics.height {
            for x in 0..metrics.width {
                let alpha = f32::from(bitmap[y * metrics.width + x]) / 255.0;
                if alpha > 0.0 {
                    self.blend((gx + x as f32) as i32, (gy + y as f32) as i32, color, alpha);
                }
            }
        }
    }

    /// A crisp, fully-covered axis-aligned device-px rectangle, SNAPPED to the
    /// integer grid — the building block for hairline rules and the text caret,
    /// which must NOT route through the rounded SDF (it would smear them across two
    /// device rows).
    fn fill_px_rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: [u8; 4]) {
        let (x0, y0, x1, y1) = self.clipped_bounds(
            x.round() as i32,
            y.round() as i32,
            (x + w).round() as i32,
            (y + h).round() as i32,
        );
        for py in y0..y1 {
            for px in x0..x1 {
                self.blend(px, py, c, 1.0);
            }
        }
    }

    /// Stroke (outline) a rounded rect with line `width`, centered on the rect edge.
    /// A near-square-cornered outline (`radius <= 0.5`, or one too small/thin to
    /// round) is drawn as four DEVICE-PX-SNAPPED spans so 1px frames / carets /
    /// hairlines stay crisp; a genuinely rounded outline uses the analytic SDF
    /// (`cov(|sd| − width/2)`), whose corner AA is wanted. All inputs are device px.
    #[allow(
        clippy::too_many_arguments,
        reason = "primitive rect-stroke blit: rect (x,y,w,h) + radius + line width + \
                  color are all irreducible geometry inputs; bundling them into a struct \
                  would only move the argument count to the call sites"
    )]
    fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, width: f32, c: [u8; 4]) {
        let sw = width.max(1.0);
        let rounded = r > 0.5 && w > sw * 2.0 + 1.0 && h > sw * 2.0 + 1.0;
        if !rounded {
            let t = sw.round().max(1.0);
            // Degenerate (a thin line / caret): one snapped span fills it.
            if w <= t || h <= t {
                self.fill_px_rect(x, y, w.max(t), h.max(t), c);
                return;
            }
            // A crisp frame: top + bottom spans, then the left/right sides BETWEEN
            // them (so translucent strokes never double-blend the corners).
            self.fill_px_rect(x, y, w, t, c);
            self.fill_px_rect(x, y + h - t, w, t, c);
            let mid_h = (h - 2.0 * t).max(0.0);
            if mid_h > 0.0 {
                self.fill_px_rect(x, y + t, t, mid_h, c);
                self.fill_px_rect(x + w - t, y + t, t, mid_h, c);
            }
            return;
        }
        // Rounded outline via the analytic SDF.
        let (cx, cy, hx, hy) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
        let r = r.min(hx).min(hy);
        let hw = sw / 2.0;
        let (x0, y0, x1, y1) = self.clipped_bounds(
            (x - sw - 1.0).floor() as i32,
            (y - sw - 1.0).floor() as i32,
            (x + w + sw + 1.0).ceil() as i32,
            (y + h + sw + 1.0).ceil() as i32,
        );
        for py in y0..y1 {
            for px in x0..x1 {
                let d =
                    sd_round_box(px as f32 + 0.5, py as f32 + 0.5, cx, cy, hx, hy, r).abs() - hw;
                self.blend(px, py, c, cov(d));
            }
        }
    }
}

/// A half-open device-pixel rectangle in a retained tray raster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RasterRect {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl RasterRect {
    pub(crate) fn pixels(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// Map an outward-rounded logical damage rectangle into a clipped device
    /// tile. Outward rounding is essential: every partially covered AA pixel
    /// touched by the logical region belongs to the patch.
    pub(crate) fn from_logical(
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        scale: f32,
        full_width: u32,
        full_height: u32,
    ) -> Option<Self> {
        if width == 0
            || height == 0
            || !scale.is_finite()
            || scale <= 0.0
            || full_width == 0
            || full_height == 0
        {
            return None;
        }
        let scale = f64::from(scale);
        let x0 = (f64::from(x) * scale).floor().max(0.0) as u64;
        let y0 = (f64::from(y) * scale).floor().max(0.0) as u64;
        let x1 = (f64::from(x.saturating_add(width)) * scale).ceil() as u64;
        let y1 = (f64::from(y.saturating_add(height)) * scale).ceil() as u64;
        let x0 = x0.min(u64::from(full_width)) as u32;
        let y0 = y0.min(u64::from(full_height)) as u32;
        let x1 = x1.min(u64::from(full_width)) as u32;
        let y1 = y1.min(u64::from(full_height)) as u32;
        (x1 > x0 && y1 > y0).then_some(Self {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        })
    }
}

/// Rasterize the tray `prims` into an RGBA8 buffer of `w*scale × h*scale` over
/// `backdrop`. Every coordinate + glyph size is multiplied by `scale` (for Retina).
/// Returns `(rgba_bytes, pixel_w, pixel_h)`.
pub(crate) fn rasterize_tray(
    prims: &[DrawPrim],
    w: u32,
    h: u32,
    scale: f32,
    backdrop: [u8; 4],
) -> (Vec<u8>, u32, u32) {
    let pw = (w as f32 * scale).round() as u32;
    let ph = (h as f32 * scale).round() as u32;
    let cv = Canvas::new(pw, ph, backdrop);
    (rasterize_tray_on_canvas(prims, scale, cv), pw, ph)
}

/// Rasterize into an exact device-pixel extent. Split-leaf geometry is already
/// device snapped, so routing it through integer logical dimensions would
/// round twice at fractional/Retina scales and defeat retained-cache identity.
pub(crate) fn rasterize_tray_pixels(
    prims: &[DrawPrim],
    pixel_width: u32,
    pixel_height: u32,
    scale: f32,
    backdrop: [u8; 4],
) -> Vec<u8> {
    rasterize_tray_on_canvas(
        prims,
        scale,
        Canvas::new(pixel_width, pixel_height, backdrop),
    )
}

/// Composite one retained straight-alpha RGBA surface over another in the same
/// linear-light source-over space as tray primitive rasterization.
///
/// Origins are device-pixel coordinates in one shared window space. The
/// destination storage is reused in place; malformed dimensions fail closed
/// without changing it.
pub(crate) fn composite_rgba_surface(
    destination: &mut Vec<u8>,
    destination_size: (u32, u32),
    destination_origin: (u32, u32),
    source: &[u8],
    source_size: (u32, u32),
    source_origin: (u32, u32),
) -> bool {
    let byte_len = |(width, height): (u32, u32)| {
        usize::try_from(
            u64::from(width)
                .saturating_mul(u64::from(height))
                .saturating_mul(4),
        )
        .ok()
    };
    if byte_len(destination_size) != Some(destination.len())
        || byte_len(source_size) != Some(source.len())
    {
        return false;
    }

    let (destination_width, destination_height) = destination_size;
    let mut canvas = Canvas {
        origin_x: destination_origin.0.min(i32::MAX as u32) as i32,
        origin_y: destination_origin.1.min(i32::MAX as u32) as i32,
        w: destination_width,
        h: destination_height,
        px: std::mem::take(destination),
        glyphs: std::collections::HashMap::new(),
        clip: Vec::new(),
    };
    let (source_width, source_height) = source_size;
    let source_x = source_origin.0.min(i32::MAX as u32) as i32;
    let source_y = source_origin.1.min(i32::MAX as u32) as i32;
    for row in 0..source_height {
        for col in 0..source_width {
            let index =
                usize::try_from((u64::from(row) * u64::from(source_width) + u64::from(col)) * 4)
                    .unwrap_or(usize::MAX);
            let Some(pixel) = source.get(index..index.saturating_add(4)) else {
                *destination = canvas.px;
                return false;
            };
            canvas.blend(
                source_x.saturating_add(col.min(i32::MAX as u32) as i32),
                source_y.saturating_add(row.min(i32::MAX as u32) as i32),
                [pixel[0], pixel[1], pixel[2], pixel[3]],
                1.0,
            );
        }
    }
    *destination = canvas.px;
    true
}

/// Rasterize only `region` of a full retained tray. Coordinates passed to every
/// primitive remain global device coordinates; only storage and scan bounds are
/// tiled. The returned bytes are tightly packed `region.width × region.height`.
pub(crate) fn rasterize_tray_region(
    prims: &[DrawPrim],
    scale: f32,
    backdrop: [u8; 4],
    region: RasterRect,
) -> Vec<u8> {
    let cv = Canvas::with_origin(
        region.x.min(i32::MAX as u32) as i32,
        region.y.min(i32::MAX as u32) as i32,
        region.width,
        region.height,
        backdrop,
    );
    rasterize_tray_on_canvas(prims, scale, cv)
}

fn rasterize_tray_on_canvas(prims: &[DrawPrim], scale: f32, mut cv: Canvas) -> Vec<u8> {
    // One lock for the whole raster: the chrome font stack (user primary +
    // bold sibling over the DejaVu coverage fallback).
    let mut fonts = lock_fonts();
    let s = scale;
    let mut skipped_clip_depth = 0_u32;
    for p in prims {
        match p {
            DrawPrim::ClipPush { .. } if skipped_clip_depth > 0 => {
                skipped_clip_depth = skipped_clip_depth.saturating_add(1);
                continue;
            }
            DrawPrim::ClipPush { x, y, w, h } => {
                cv.push_clip(x * s, y * s, w * s, h * s);
                if !cv.active_clip_intersects_tile() {
                    skipped_clip_depth = 1;
                }
                continue;
            }
            DrawPrim::ClipPop if skipped_clip_depth > 1 => {
                skipped_clip_depth -= 1;
                continue;
            }
            DrawPrim::ClipPop if skipped_clip_depth == 1 => {
                skipped_clip_depth = 0;
                cv.pop_clip();
                continue;
            }
            DrawPrim::ClipPop => {
                cv.pop_clip();
                continue;
            }
            _ if skipped_clip_depth > 0 => continue,
            _ => {}
        }
        match p {
            DrawPrim::Panel {
                x,
                y,
                w,
                h,
                radius,
                fill,
                ..
            } => cv.round_rect(x * s, y * s, w * s, h * s, radius * s, *fill),
            DrawPrim::Ring {
                cx,
                cy,
                r_outer,
                thickness,
                track,
                sys_frac,
                sys_color,
                tab_frac,
                tab_color,
                dashed_tab,
            } => {
                let r_mid = (r_outer - thickness / 2.0) * s;
                let th = thickness * s;
                // capacity track (full circle)
                cv.arc(cx * s, cy * s, r_mid, th, 1.0, *track);
                // system usage arc
                cv.arc(cx * s, cy * s, r_mid, th, *sys_frac, *sys_color);
                // this-tab inner arc (nested), or a dashed faint placeholder
                let r_in = r_mid - th * 0.95;
                if *dashed_tab {
                    // honest "—": a faint dotted inner track (no real value)
                    for k in 0..12 {
                        let f0 = k as f32 / 12.0;
                        cv.arc(cx * s, cy * s, r_in, th * 0.5, f0 + 0.02, *track);
                    }
                } else if let Some(tf) = tab_frac {
                    cv.arc(cx * s, cy * s, r_in, th * 0.55, *tf, *tab_color);
                }
            }
            DrawPrim::Capsule {
                x,
                y,
                w,
                h,
                frac,
                fill,
                track,
            } => {
                let r = h * s / 2.0;
                cv.round_rect(x * s, y * s, w * s, h * s, r, *track);
                let fw = (w * frac).max(if *frac > 0.0 { *h } else { 0.0 }) * s;
                if fw > 0.0 {
                    cv.round_rect(x * s, y * s, fw, h * s, r, *fill);
                }
            }
            DrawPrim::Dot {
                cx, cy, r, color, ..
            } => cv.disc(cx * s, cy * s, r * s, *color),
            DrawPrim::HsvDisk { cx, cy, r, value } => {
                cv.hsv_disk(cx * s, cy * s, r * s, *value);
            }
            DrawPrim::Sparkline {
                x,
                y,
                w,
                h,
                samples,
                color,
            } => {
                let n = samples.len().max(1);
                let bw = (w * s) / n as f32;
                for (i, v) in samples.iter().enumerate() {
                    let bh = (v.clamp(0.0, 1.0)) * h * s;
                    cv.round_rect(
                        x * s + i as f32 * bw,
                        y * s + h * s - bh,
                        (bw - 1.0).max(1.0),
                        bh,
                        1.0,
                        *color,
                    );
                }
            }
            DrawPrim::Text {
                x,
                baseline,
                s: txt,
                px,
                color,
                weight,
                face,
            } => cv.text(
                &mut fonts,
                x * scale,
                baseline * scale,
                txt,
                px * scale,
                *weight,
                *face,
                *color,
            ),
            DrawPrim::TerminalSpecimen { x, y, spec } => {
                cv.terminal_specimen(*x, *y, spec, scale);
            }
            DrawPrim::AdditiveRect { x, y, w, h, premul } => {
                cv.additive_rect(x * s, y * s, w * s, h * s, *premul)
            }
            DrawPrim::EffectHalo {
                halo,
                offset_x,
                offset_y,
            } => cv.effect_halo(*halo, *offset_x, *offset_y, scale),
            DrawPrim::EffectFire {
                patch,
                offset_x,
                offset_y,
            } => cv.effect_fire(*patch, *offset_x, *offset_y, scale),
            DrawPrim::Stroke {
                x,
                y,
                w,
                h,
                radius,
                width,
                color,
            } => cv.stroke_rect(x * s, y * s, w * s, h * s, radius * s, width * s, *color),
            DrawPrim::Line {
                x1,
                y1,
                x2,
                y2,
                width,
                color,
            } => cv.line_segment((x1 * s, y1 * s), (x2 * s, y2 * s), width * s, *color),
            DrawPrim::ClipPush { .. } | DrawPrim::ClipPop => unreachable!("handled above"),
        }
    }
    cv.px
}

/// Copy a tightly packed regional raster into an existing full retained RGBA8
/// surface. Invalid dimensions fail closed without mutating `destination`.
pub(crate) fn apply_raster_patch(
    destination: &mut [u8],
    destination_width: u32,
    destination_height: u32,
    region: RasterRect,
    patch: &[u8],
) -> bool {
    let Some(destination_len) = usize::try_from(destination_width)
        .ok()
        .and_then(|width| width.checked_mul(usize::try_from(destination_height).ok()?))
        .and_then(|pixels| pixels.checked_mul(4))
    else {
        return false;
    };
    let Some(patch_len) = usize::try_from(region.width)
        .ok()
        .and_then(|width| width.checked_mul(usize::try_from(region.height).ok()?))
        .and_then(|pixels| pixels.checked_mul(4))
    else {
        return false;
    };
    if destination.len() != destination_len
        || patch.len() != patch_len
        || region.x.saturating_add(region.width) > destination_width
        || region.y.saturating_add(region.height) > destination_height
    {
        return false;
    }
    let destination_stride = destination_width as usize * 4;
    let patch_stride = region.width as usize * 4;
    for row in 0..region.height as usize {
        let destination_start =
            (region.y as usize + row) * destination_stride + region.x as usize * 4;
        let patch_start = row * patch_stride;
        destination[destination_start..destination_start + patch_stride]
            .copy_from_slice(&patch[patch_start..patch_start + patch_stride]);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::DrawPrim;
    use aterm_render::Theme;

    #[test]
    fn chrome_font_store_is_cold_until_host_preparation_installs_ui_faces() {
        std::thread::spawn(|| {
            {
                let fonts = lock_fonts();
                assert!(fonts.ui_regular.is_none());
                assert!(fonts.ui_semibold.is_none());
            }

            set_chrome_fonts(None, None, None);
            let expected = prepared_ui_font_assets();
            let fonts = lock_fonts();
            match (&expected.regular, &fonts.ui_regular) {
                (Some(expected), Some(installed)) => assert!(Arc::ptr_eq(expected, installed)),
                (None, None) => {}
                _ => panic!("regular UI face did not match the prepared host asset"),
            }
            match (&expected.semibold, &fonts.ui_semibold) {
                (Some(expected), Some(installed)) => assert!(Arc::ptr_eq(expected, installed)),
                (None, None) => {}
                _ => panic!("semibold UI face did not match the prepared host asset"),
            }
        })
        .join()
        .expect("cold-to-prepared font transition exits cleanly");
    }

    /// T3: the Win11 variable cut LEADS the Windows UI-face candidates and is
    /// paired with the STATIC semibold — never with itself (two wght-400 faces
    /// would erase every `UiBold` heading's contrast with no error anywhere).
    /// The static Segoe UI pair stays behind it for Win10 hosts.
    #[cfg(windows)]
    #[test]
    fn windows_ui_face_candidates_lead_with_segoe_ui_variable_over_a_static_semibold() {
        let candidates = ui_font_candidates();
        let file = |path: &std::path::Path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_ascii_lowercase)
                .unwrap_or_default()
        };
        assert_eq!(candidates.len(), 2);
        assert_eq!(file(&candidates[0].regular_path), "seguivar.ttf");
        assert_eq!(file(&candidates[0].semibold_path), "seguisb.ttf");
        assert_eq!(file(&candidates[1].regular_path), "segoeui.ttf");
        assert_eq!(file(&candidates[1].semibold_path), "seguisb.ttf");
        for candidate in &candidates {
            assert_ne!(
                candidate.regular_path, candidate.semibold_path,
                "a candidate must never pair a face with itself"
            );
        }
    }

    /// The Linux ladder must reach Noto Sans on a host that ships only the
    /// fonts-noto-core cuts (Regular/Bold — Debian and Ubuntu never install a
    /// NotoSans-SemiBold.ttf). Pairing Regular exclusively with SemiBold made
    /// every stock Debian box fall through to DejaVu Sans, ~8% wider, which the
    /// 2026-08 settings audit measured as systemic truncation across the
    /// chrome. SemiBold keeps priority where a distro does carry it.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_ui_face_ladder_reaches_noto_on_bold_only_hosts() {
        let candidates = ui_font_candidates();
        let file = |path: &std::path::Path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_ascii_lowercase)
                .unwrap_or_default()
        };
        let noto_pairs: Vec<(String, String)> = candidates
            .iter()
            .filter(|candidate| file(&candidate.regular_path).contains("noto"))
            .map(|candidate| {
                (
                    file(&candidate.regular_path),
                    file(&candidate.semibold_path),
                )
            })
            .collect();
        assert!(
            noto_pairs
                .iter()
                .any(|(regular, semibold)| regular == "notosans-regular.ttf"
                    && semibold == "notosans-bold.ttf"),
            "a Regular+Bold pairing must back up the SemiBold cut Debian never ships: {noto_pairs:?}"
        );
        assert_eq!(
            noto_pairs.first().map(|(_, semibold)| semibold.as_str()),
            Some("notosans-semibold.ttf"),
            "the true SemiBold keeps priority where a distro ships it"
        );
        let first_dejavu = candidates
            .iter()
            .position(|candidate| file(&candidate.regular_path).contains("dejavu"))
            .expect("DejaVu remains the last-resort pair");
        let last_noto = candidates
            .iter()
            .rposition(|candidate| file(&candidate.regular_path).contains("noto"))
            .expect("Noto pairs exist");
        assert!(
            last_noto < first_dejavu,
            "every Noto pairing outranks the wider DejaVu fallback"
        );
        for candidate in &candidates {
            assert_ne!(
                candidate.regular_path, candidate.semibold_path,
                "a candidate must never pair a face with itself"
            );
        }
        // Host-conditional leg: with the Debian core cuts on disk the resolver
        // must land Noto Sans — regular AND a real bold companion — never the
        // DejaVu fallback.
        let debian_regular =
            std::path::Path::new("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf");
        let debian_bold = std::path::Path::new("/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf");
        if debian_regular.exists() && debian_bold.exists() {
            let assets = resolve_ui_font_assets();
            let regular = assets.regular.expect("Noto regular resolves");
            assert!(
                regular.name().is_some_and(|name| name.contains("Noto")),
                "regular face is Noto: {:?}",
                regular.name()
            );
            let semibold = assets
                .semibold
                .expect("a heavier companion resolves beside the regular");
            assert!(
                semibold.name().is_some_and(|name| name.contains("Noto")),
                "the UiBold face stays in the Noto family: {:?}",
                semibold.name()
            );
        }
    }

    /// The strip band's readiness/coverage seams follow the installed UI face:
    /// cold store ⇒ not ready and nothing coverable; prepared store ⇒ ready, a
    /// Latin label coverable, and a char the embedded fallback stack lacks not.
    #[test]
    fn strip_band_seams_follow_the_installed_ui_face() {
        clear_ui_fonts_for_test();
        assert!(!strip_band_ui_ready());
        assert!(!strip_band_run_coverable("Settings"));
        let cold = strip_band_font_epoch();
        prepare_ui_fonts_for_direct_view_test();
        if !strip_band_ui_ready() {
            // Host without a resolvable system UI face (CI): the decline
            // branch above is the whole contract.
            return;
        }
        assert_ne!(
            strip_band_font_epoch(),
            cold,
            "the UI face landing must move the band's font fingerprint"
        );
        assert!(strip_band_run_coverable("Settings · pwsh 7"));
        assert!(
            !strip_band_run_coverable("\u{1F680}"),
            "a colour emoji is covered by neither the UI face nor the embedded cascade"
        );
        clear_ui_fonts_for_test();
    }

    #[test]
    fn ui_font_assets_are_host_prepared_and_compile_raster_without_resolver_io() {
        use crate::native_ui::{
            GroupSpec, Insets, Layout, Length, LogicalRect, SemanticRole, StyleRef, TextSpec,
            UiContent, UiNode, UiTree,
        };

        // The only resolver entry is backend/chrome installation. Everything
        // below is the exact semantic compile → tray → image path.
        set_chrome_fonts(None, None, None);
        let attempts = UI_FONT_RESOLVE_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed);
        assert!(attempts >= 1, "host preparation exercised the resolver");
        let installed = lock_fonts().ui_regular.clone();

        let tree = UiTree::new(
            UiNode::new(
                "root",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(Layout::column().padding(Insets::all(8.0)))
            .children(vec![
                UiNode::new(
                    "label",
                    UiContent::Text(TextSpec {
                        text: "Settings · Aa 012 ✓".to_string(),
                        role: SemanticRole::Heading,
                        style: StyleRef::Primary,
                    }),
                )
                .layout(Layout::default().height(Length::Fixed(32.0))),
            ]),
        );
        let viewport = LogicalRect::new(0.0, 0.0, 260.0, 56.0);
        let compiled = tree.compile(viewport).expect("semantic UI compiles");
        let measured = ui_text_width_for(TextFace::Ui, "Settings · Aa 012 ✓", 13.0);
        assert!(measured > 0.0);
        let prims = compiled.tray(Theme::default(), 13.0).prims;
        let (image, _, _) = rasterize_tray(&prims, 260, 56, 1.0, [0, 0, 0, 0]);
        let (pixels, remainder) = image.as_chunks::<4>();
        assert!(remainder.is_empty(), "tray raster has complete pixels");
        assert!(pixels.iter().any(|pixel| pixel[3] != 0));

        assert_eq!(
            UI_FONT_RESOLVE_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed),
            attempts,
            "compile, measure, tray paint, and image raster perform no font resolution",
        );
        let after = lock_fonts().ui_regular.clone();
        match (installed, after) {
            (Some(before), Some(after)) => assert!(Arc::ptr_eq(&before, &after)),
            (None, None) => {}
            _ => panic!("immutable UI asset identity changed during compile/raster"),
        }

        // Line-ending agnostic: a Windows checkout (`core.autocrlf`) hands
        // `include_str!` CRLF text, and a `\n`-only marker then never splits —
        // the scan silently widened to this whole test module.
        let source = include_str!("tray_raster.rs").replace("\r\n", "\n");
        let canvas = source
            .split("struct Canvas")
            .nth(1)
            .expect("Canvas source")
            .split("#[cfg(test)]\nmod tests")
            .next()
            .expect("production raster source");
        assert!(
            !canvas.contains("std::fs::") && !canvas.contains("ui_font_candidates()"),
            "paint/raster source must remain resolver and filesystem free",
        );
    }

    /// A theme-derived spread of prims — three gauge rings, a capacity capsule, a
    /// status dot, a throughput sparkline, and one funnelled text run — enough to
    /// drive every core arm of the rasterizer without any live metrics source.
    fn demo_prims(cw: f32, ch: f32, theme: Theme) -> Vec<DrawPrim> {
        use crate::type_scale::TypeStep;
        use crate::widget::{TextFace, TextWeight, rgba, text_prim};
        let c = crate::chrome_band::band_colors(theme);
        let label = rgba(c.label, 0xFF);
        let track = rgba(c.label, 0x33);
        let good = rgba(c.value, 0xFF);
        let cap = TypeStep::Caption.px(12.0);
        let slot = (cw - 32.0) / 3.0;
        let mut prims = vec![text_prim(
            16.0,
            24.0,
            "SYSTEM".to_string(),
            cap,
            TextWeight::Bold,
            TextFace::Mono,
            label,
        )];
        for i in 0..3u32 {
            prims.push(DrawPrim::Ring {
                cx: 16.0 + slot * (i as f32 + 0.5),
                cy: 70.0,
                r_outer: 26.0,
                thickness: 7.0,
                track,
                sys_frac: 0.3 + i as f32 * 0.2,
                sys_color: good,
                tab_frac: Some(0.1),
                tab_color: good,
                dashed_tab: i == 2,
            });
        }
        prims.push(DrawPrim::Capsule {
            x: 16.0,
            y: ch - 48.0,
            w: cw - 32.0,
            h: 8.0,
            frac: 0.4,
            fill: good,
            track,
        });
        prims.push(DrawPrim::Dot {
            cx: cw - 20.0,
            cy: 20.0,
            r: 4.0,
            color: good,
            breathe: false,
        });
        prims.push(DrawPrim::Sparkline {
            x: 16.0,
            y: ch - 28.0,
            w: 80.0,
            h: 12.0,
            samples: vec![0.1, 0.5, 0.3, 0.9, 0.4],
            color: good,
        });
        prims
    }

    /// Tier-1 conformance for the CHROME FACE GATE: the SHIPPING
    /// [`select_chrome_face`] policy, enumerated over its ENTIRE 2^3 boolean
    /// domain — a complete proof of the same invariants the `ChromeFaceGate`
    /// derived model carries (`aterm_spec::derive::chrome_face_gate_model`),
    /// which the real Trust `ty` proves at Buggy=0 and catches at Buggy=1
    /// (`derived_chrome_face_gate_proves_and_catches_dejavu_hardcode`).
    #[test]
    fn chrome_face_gate_exhaustive() {
        let (mut fallback_seen, mut bold_seen, mut primary_seen) = (false, false, false);
        for bits in 0u32..8 {
            let b = |i: u32| (bits >> i) & 1 == 1;
            let (bold_run, bold_has, primary_has) = (b(0), b(1), b(2));
            let pick = select_chrome_face(bold_run, bold_has, primary_has);
            // EmbeddedOnlyAsCoverageFallback: the embedded DejaVu is chosen
            // ONLY when neither user face applies.
            if pick == ChromeFacePick::Fallback {
                fallback_seen = true;
                assert!(
                    !(primary_has || (bold_run && bold_has)),
                    "DejaVu chosen although a user face covers the glyph \
                     (bold_run={bold_run}, bold_has={bold_has}, primary_has={primary_has})"
                );
            }
            // BoldHonoredWhenCovered: a covered bold run keeps its weight.
            if bold_run && bold_has {
                bold_seen = true;
                assert_eq!(
                    pick,
                    ChromeFacePick::Bold,
                    "covered bold run lost its weight"
                );
            }
            // The primary is only ever chosen when it covers the glyph.
            if pick == ChromeFacePick::Primary {
                primary_seen = true;
                assert!(primary_has, "primary chosen without coverage");
            }
        }
        // NON-VACUOUS: every face of the stack is reachable.
        assert!(
            fallback_seen && bold_seen && primary_seen,
            "gate must reach all three faces (fallback={fallback_seen}, \
             bold={bold_seen}, primary={primary_seen})"
        );
    }

    /// The cap-height centering law bound to the SHIPPING [`row_baseline`]
    /// (the fn every chrome painter calls): over a lattice of `(row, size)`,
    /// the gap above the cap box and the gap below the baseline balance within
    /// 1px + one 26.6 subunit (the `chrome_metrics` proofs' device bound).
    #[test]
    fn row_baseline_balances_gaps_on_the_shipping_path() {
        let ratio = chrome_cap_ratio();
        assert!((0.4..1.0).contains(&ratio), "sane cap ratio: {ratio}");
        for row_h in [12.0_f32, 17.0, 20.0, 21.5, 34.0, 64.0] {
            for size in [8.0_f32, 9.6, 12.0, 13.8, 14.0, 26.0] {
                let cap = size * ratio;
                if cap > row_h {
                    continue;
                }
                for y0 in [0.0_f32, 7.5, 100.0] {
                    let b = row_baseline(y0, row_h, size);
                    let top_gap = (b - cap) - y0;
                    let bottom_gap = (y0 + row_h) - b;
                    assert!(
                        (top_gap - bottom_gap).abs() <= 1.0 + 1.0 / 64.0 + 1e-3,
                        "row_h={row_h} size={size} y0={y0}: gaps {top_gap} vs {bottom_gap}"
                    );
                }
            }
        }
        // And the old em-bottom placement (baseline = y + px) fails the law for
        // the same inputs — the pre-fix ~0.135em-low sit (negative control).
        let (row_h, size) = (20.0_f32, 14.0_f32);
        let old_b = (row_h - size) * 0.5 + size;
        let cap = size * ratio;
        assert!(
            ((old_b - cap) - (row_h - old_b)).abs() > 1.5,
            "the pre-fix baseline must be detectably unbalanced"
        );
    }

    /// Draw and measurement agree: the pixels of a run never extend past the
    /// fixed-point measured width (+1px of rounding), and the run genuinely
    /// paints — the pen/measure seam cannot drift apart silently.
    #[test]
    fn draw_stays_within_measured_width() {
        let s = "MMMMMMMMMMMMMMMMMMMMMMMMMMMMMMMM"; // 32 advances
        let px = 13.0_f32;
        let measured = measure_text(s, px, TextWeight::Regular);
        assert!(measured > 0.0);
        let w = (measured.ceil() as u32) + 4;
        let prims = vec![crate::widget::text_prim(
            1.0,
            14.0,
            s.to_string(),
            crate::type_scale::TypeStep::Body.px(px),
            TextWeight::Regular,
            TextFace::Mono,
            [255, 255, 255, 255],
        )];
        let (buf, pw, ph) = rasterize_tray(&prims, w, 20, 1.0, [0, 0, 0, 255]);
        let mut ink_right = 0u32;
        for yy in 0..ph {
            for xx in 0..pw {
                if buf[((yy * pw + xx) * 4) as usize] > 0 {
                    ink_right = ink_right.max(xx + 1);
                }
            }
        }
        assert!(ink_right > 0, "the run painted");
        assert!(
            (ink_right as f32) <= 1.0 + measured + 1.0,
            "ink right edge {ink_right} exceeds measured width {measured} (+1px rounding)"
        );
    }

    /// [`set_chrome_fonts`] robustness: unparseable bytes leave the slot on the
    /// DejaVu fallback (text still renders), and clearing restores the default
    /// stack. Serialized with the other font-injecting assertions by running in
    /// one test (the store is process-global).
    #[test]
    fn set_chrome_fonts_bad_bytes_keep_fallback_rendering() {
        let garbage: std::sync::Arc<[u8]> = std::sync::Arc::from(&b"not a font"[..]);
        set_chrome_fonts(Some((garbage.clone(), 0)), Some((garbage, 3)), None);
        let w = measure_text("abc", 12.0, TextWeight::Regular);
        assert!(w > 0.0, "fallback still measures after a bad injection");
        // And an injected REAL face (the embedded bytes stand in for the user's
        // font) takes the primary slot: bold runs downgrade honestly (no bold
        // face), regular runs measure identically to the same face as fallback.
        let bytes: std::sync::Arc<[u8]> = std::sync::Arc::from(aterm_render::embedded_font());
        set_chrome_fonts(Some((bytes, 0)), None, None);
        let w2 = measure_text("abc", 12.0, TextWeight::Bold);
        assert!(
            (w2 - w).abs() < 1e-3,
            "bold with no bold face downgrades to primary"
        );
        set_chrome_fonts(None, None, None);
    }

    /// One real engine frame for the shipping specimen path: `source` runs
    /// through the actual `Terminal`, exactly like
    /// `settings_preview::build_terminal_specimen_input` builds its snapshot.
    fn specimen_input(source: &str, rows: usize, cols: usize) -> aterm_render::RenderInput {
        let mut terminal = aterm_core::terminal::Terminal::new(rows as u16, cols as u16);
        terminal.process(source.as_bytes());
        let mut input = terminal.cell_frame(rows, cols);
        input.cursor_visible = false;
        input
    }

    /// Bind one exact renderer + engine frame into the spec
    /// [`Canvas::terminal_specimen`] consumes, mirroring the migrated
    /// `settings_preview` call sites (production defaults for every knob a
    /// test does not exercise).
    fn specimen_spec(
        renderer: Renderer,
        identity: crate::widget::SemanticFontCandidate,
        input: aterm_render::RenderInput,
    ) -> crate::widget::TerminalSpecimenSpec {
        crate::widget::TerminalSpecimenSpec {
            input: Arc::new(input),
            input_fingerprint: 1,
            prepared_font: PreparedSemanticFont {
                candidate: identity,
                snapshot: SemanticFontSnapshot {
                    status: "test renderer snapshot".to_string(),
                    ready_epoch: 1,
                    pending: false,
                },
                renderer: Some(Rc::new(renderer)),
            },
            theme: aterm_render::Theme::default(),
            font_px: 14.0,
            line_height: 1.0,
            baseline_adjust: 0,
            ligatures: true,
            merged_ligatures: false,
            cursor_break_ligatures: true,
            synthetic_styles: true,
            underline_position: 0,
            underline_thickness: 0,
            underline_skip_descenders: true,
            text_blending: SpecimenTextBlending::default(),
            font_thicken: false,
            stem_gamma: 1.0,
            variations: Vec::new(),
            minimum_contrast: 1.0,
            selection_foreground: None,
            selection_inactive: false,
        }
    }

    /// An unsupported scalar on the shipping specimen path occupies exactly
    /// one terminal cell and cannot collapse the grid: every pixel outside
    /// that cell — including the following glyph's placement — is
    /// byte-identical to the same frame with a space there. (The specimen IS
    /// the shipping terminal renderer, so the honest in-cell miss signal —
    /// `.notdef`, plus its `missing_font_classes` report — replaced the
    /// retired `Canvas::terminal_text` per-glyph skip; confinement to the one
    /// column is the invariant that survives the migration.)
    #[test]
    fn semantic_terminal_text_skips_notdef_without_collapsing_the_grid() {
        let raster = |unsupported: bool| -> Vec<u8> {
            let mut renderer = Renderer::from_bytes(
                aterm_render::embedded_font(),
                14.0,
                aterm_render::Theme::default(),
            )
            .expect("embedded renderer");
            renderer.set_runtime_font_discovery(false);
            // Both frames are the exact same engine snapshot of "A B"; the
            // probe swaps the middle cell for a scalar no face can cover, so
            // it occupies that one terminal column BY CONSTRUCTION.
            let mut input = specimen_input("A B", 1, 8);
            if unsupported {
                input.cells[0][1].ch = '\u{10FFFF}';
            }
            let spec = specimen_spec(
                renderer,
                crate::widget::SemanticFontCandidate::default(),
                input,
            );
            let mut canvas = Canvas::new(96, 28, [0, 0, 0, 255]);
            canvas.terminal_specimen(2.0, 4.0, &spec, 1.0);
            canvas.px
        };

        let (with_unsupported, with_space) = (raster(true), raster(false));
        // The probed cell's device-px x-band: blit origin 2 + one cell width.
        let cell_w = Renderer::from_bytes(
            aterm_render::embedded_font(),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("embedded renderer")
        .cell_size()
        .0 as usize;
        let band = (2 + cell_w)..(2 + 2 * cell_w);
        for (index, (probe, space)) in with_unsupported.iter().zip(&with_space).enumerate() {
            let x = (index / 4) % 96;
            if !band.contains(&x) {
                assert_eq!(
                    probe, space,
                    "unsupported-scalar ink leaked outside its cell at x={x}"
                );
            }
        }
        // Non-vacuity: the probe genuinely exercised the uncovered-miss path
        // (all difference therefore sits inside the one probed cell).
        assert_ne!(
            with_unsupported, with_space,
            "the probe scalar must reach the renderer's miss path"
        );
    }

    /// Installing a renderer fork is deliberately dormant: ordinary terminal
    /// startup must not spawn a worker or discover broad Unicode fallbacks before
    /// a visible semantic preview asks for them.
    #[test]
    fn semantic_renderer_install_is_lazy_until_preview_demand() {
        let mut renderer = Renderer::from_bytes(
            aterm_render::embedded_font(),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("embedded renderer");
        renderer.set_runtime_font_discovery(false);
        set_chrome_fonts(None, None, Some(renderer));
        let fonts = lock_fonts();
        assert!(fonts.semantic.is_some(), "exact fork is retained dormant");
        assert!(
            fonts.semantic_worker.is_some(),
            "backend startup parks the worker before Settings can request a candidate"
        );
        assert!(fonts.semantic_pending.is_none());
        assert!(fonts.semantic_prewarm_ms.is_none());
    }

    /// A reload burst is bounded to one worker plus its latest one-slot job. A
    /// superseded generation cannot install even when it was already waiting to
    /// parse, and the current result disarms after landing.
    #[test]
    fn semantic_prewarm_coalesces_and_installs_only_latest_generation() {
        let renderer = || {
            let mut renderer = Renderer::from_bytes(
                aterm_render::embedded_font(),
                14.0,
                aterm_render::Theme::default(),
            )
            .expect("embedded renderer");
            renderer.set_runtime_font_discovery(false);
            renderer
        };
        // A PRIVATE serialization gate: the staged coalescing window and the
        // bounded landing wait below must never queue behind (or stall)
        // another test's warmup on the process-wide gate. The worker protocol
        // is byte-identical to production's `spawn`.
        let gate: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
        let serial = gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut fonts = default_chrome_fonts();
        // Backend startup parks the one worker (`set_chrome_fonts`); candidate
        // requests never demand-spawn it.
        fonts.semantic_worker = SemanticPrewarmWorker::spawn_serialized_by(gate);
        fonts.semantic_generation = 1;
        fonts.semantic = Some(renderer());
        fonts.semantic_identity = Some(crate::widget::SemanticFontCandidate::default());
        let candidate = crate::widget::SemanticFontCandidate::default();
        assert!(fonts.ensure_semantic_candidate(&candidate));
        let worker_ptr = fonts
            .semantic_worker
            .as_ref()
            .map(|worker| Arc::as_ptr(&worker.queue))
            .expect("the parked worker serves the request");

        fonts.semantic_generation = 2;
        fonts.semantic_pending = None;
        fonts.semantic_worker_generation = None;
        fonts.semantic = Some(renderer());
        fonts.semantic_identity = Some(candidate.clone());
        fonts.semantic_prewarm_ms = None;
        assert!(fonts.ensure_semantic_candidate(&candidate));
        assert_eq!(
            fonts
                .semantic_worker
                .as_ref()
                .map(|worker| Arc::as_ptr(&worker.queue)),
            Some(worker_ptr),
            "reload reuses the same worker"
        );
        drop(serial);

        // Debug builds can spend several seconds settling the complete Unicode
        // specimen on a cold font cache. This is a bounded completion wait, not
        // a timing assertion; correctness is the landed generation below.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while fonts.semantic_pending.is_some() && std::time::Instant::now() < deadline {
            fonts.poll_semantic_renderer();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        fonts.poll_semantic_renderer();
        assert_eq!(fonts.semantic_generation, 2);
        assert!(fonts.semantic_pending.is_none(), "land disarms");
        assert!(fonts.semantic.is_some(), "latest exact fork installs");
        assert!(fonts.semantic_prewarm_ms.is_some());
    }

    #[test]
    fn rapid_candidate_replacement_carries_base_and_cached_views_do_not_cross_contaminate() {
        let mut renderer = Renderer::from_bytes(
            aterm_render::embedded_font(),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("embedded renderer");
        renderer.set_runtime_font_discovery(false);
        // A PRIVATE serialization gate — see
        // `semantic_prewarm_coalesces_and_installs_only_latest_generation`.
        let gate: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
        let mut fonts = default_chrome_fonts();
        // Backend startup parks the one worker (`set_chrome_fonts`); candidate
        // requests never demand-spawn it.
        fonts.semantic_worker = SemanticPrewarmWorker::spawn_serialized_by(gate);
        fonts.semantic_generation = 7;
        fonts.semantic = Some(renderer);
        fonts.semantic_identity = Some(crate::widget::SemanticFontCandidate::default());
        let first = crate::widget::SemanticFontCandidate {
            regular: Some("No Such Candidate A 7F31".to_string()),
            ..crate::widget::SemanticFontCandidate::default()
        };
        let second = crate::widget::SemanticFontCandidate {
            regular: Some("No Such Candidate B 9C42".to_string()),
            ..crate::widget::SemanticFontCandidate::default()
        };

        let serial = gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let initial_ready_epoch = fonts.semantic_ready_epoch;
        assert!(fonts.ensure_semantic_candidate(&first));
        assert!(fonts.ensure_semantic_candidate(&second));
        drop(serial);

        let wait = |fonts: &mut ChromeFonts| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while fonts.semantic_pending.is_some() && std::time::Instant::now() < deadline {
                fonts.poll_semantic_renderer();
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            fonts.poll_semantic_renderer();
        };
        wait(&mut fonts);
        assert_eq!(fonts.semantic_identity.as_ref(), Some(&second));
        assert_ne!(
            fonts.semantic_ready_epoch, initial_ready_epoch,
            "worker landing invalidates retained preview fingerprints"
        );
        assert!(
            fonts.semantic.is_some(),
            "replacement retained the only base"
        );
        assert!(matches!(
            fonts.semantic_resolution,
            SemanticFontResolution::Fallback(_)
        ));

        // Whether the worker dequeued A or B won the replacement slot above,
        // force the same uncached B -> A transition. This makes the cache/paint
        // isolation regression deterministic instead of scheduler-dependent.
        fonts.semantic_cache.remove(&first);
        assert!(fonts.ensure_semantic_candidate(&first));
        assert!(
            fonts.semantic.is_none(),
            "B cannot paint A while A resolves"
        );
        wait(&mut fonts);
        assert_eq!(fonts.semantic_identity.as_ref(), Some(&first));
        assert!(
            !fonts.ensure_semantic_candidate(&second),
            "returning to B is a bounded renderer-cache swap"
        );
        assert_eq!(fonts.semantic_identity.as_ref(), Some(&second));
    }

    #[test]
    fn four_face_candidate_changes_real_pixels_fingerprint_and_ready_semantics() {
        let fixture = include_bytes!("../../aterm-render/tests/fixtures/jetbrains-mono.ttf");
        let family = "aterm deterministic JetBrains fixture";
        let mut faces = CandidateFaceCache::new();
        faces.insert(
            family.to_ascii_lowercase(),
            Ok(ResolvedCandidateFace {
                path: "jetbrains-mono.ttf".to_string(),
                bytes: Arc::new(fixture.to_vec()),
            }),
        );
        let candidate = crate::widget::SemanticFontCandidate {
            regular: Some(family.to_string()),
            bold: Some(family.to_string()),
            italic: Some(family.to_string()),
            bold_italic: Some(family.to_string()),
            fallback: Vec::new(),
            symbol: None,
            emoji: None,
            variations: Vec::new(),
            synthetic_styles: true,
        };
        let mut base = Renderer::from_bytes(
            aterm_render::embedded_font(),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("embedded renderer");
        base.set_runtime_font_discovery(false);
        base.prepare_semantic_typography("Regular Aa Bold Aa Italic Aa Bold Italic Aa != =>");
        let (candidate_renderer, resolution) =
            build_semantic_candidate(&base, &candidate, &mut faces);
        let candidate_renderer = candidate_renderer.expect("fixture candidate renderer");
        assert_eq!(
            candidate_renderer.debug_styled_face_indices(),
            [Some(0), Some(0), Some(0)],
            "all four authored family slots reach the renderer"
        );
        assert_eq!(resolution, SemanticFontResolution::Ready);
        assert!(
            resolution
                .description(&candidate)
                .contains("candidate renderer ready")
        );

        let raster =
            |renderer: Renderer, identity: crate::widget::SemanticFontCandidate| -> Vec<u8> {
                // One engine frame drives all four authored family slots: the
                // rows carry real SGR bold/italic renditions, so the specimen
                // exercises the same styled-face routing a terminal does.
                let input = specimen_input(
                    concat!(
                        "Regular Aa 012 != =>\r\n",
                        "\u{1b}[1mBold Aa 012 != =>\u{1b}[0m\r\n",
                        "\u{1b}[3mItalic Aa 012 != =>\u{1b}[0m\r\n",
                        "\u{1b}[1;3mBold Italic Aa 012 != =>\u{1b}[0m",
                    ),
                    4,
                    26,
                );
                let spec = specimen_spec(renderer, identity, input);
                let mut canvas = Canvas::new(420, 96, [18, 20, 26, 255]);
                canvas.terminal_specimen(4.0, 4.0, &spec, 1.0);
                canvas.px
            };
        let host = crate::widget::SemanticFontCandidate::default();
        let host_pixels = raster(base, host.clone());
        let candidate_pixels = raster(candidate_renderer, candidate.clone());
        assert!(
            host_pixels
                .iter()
                .zip(&candidate_pixels)
                .filter(|(left, right)| left != right)
                .count()
                > 500,
            "resolved candidate must alter genuine terminal glyph pixels"
        );
        assert_ne!(
            crate::settings_preview::SettingsPreviewSpec::typography(14.0).paint_fingerprint(),
            crate::settings_preview::SettingsPreviewSpec::typography(14.0)
                .with_font_candidate(candidate)
                .paint_fingerprint(),
            "candidate identity participates in retained paint"
        );
    }

    #[test]
    fn invalid_candidate_falls_back_honestly_without_losing_the_renderer() {
        let mut base = Renderer::from_bytes(
            aterm_render::embedded_font(),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("embedded renderer");
        base.set_runtime_font_discovery(false);
        let candidate = crate::widget::SemanticFontCandidate {
            regular: Some("No Such Font Family E18D 77A2".to_string()),
            ..crate::widget::SemanticFontCandidate::default()
        };
        let (renderer, resolution) =
            build_semantic_candidate(&base, &candidate, &mut CandidateFaceCache::new());
        assert!(
            renderer.is_some(),
            "invalid draft retains committed fallback renderer"
        );
        let SemanticFontResolution::Fallback(unresolved) = &resolution else {
            panic!("invalid family must expose fallback semantics: {resolution:?}")
        };
        assert!(unresolved.iter().any(|slot| slot.contains("regular")));
        let status = resolution.description(&candidate);
        assert!(status.contains("candidate fallback active"));
        assert!(status.contains("No Such Font Family"));
    }

    /// Libtest workers must not exchange renderer-resolved font state. A font
    /// installed by one concurrently live worker remains visible there, while a
    /// peer starts from the deterministic fallback context.
    #[test]
    fn test_font_context_is_isolated_per_worker_thread() {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let installer_barrier = barrier.clone();
        let installer = std::thread::spawn(move || {
            let bytes: std::sync::Arc<[u8]> = std::sync::Arc::from(aterm_render::embedded_font());
            set_chrome_fonts(Some((bytes, 0)), None, None);
            installer_barrier.wait();
            installer_barrier.wait();
            assert!(lock_fonts().primary.is_some());
        });
        let observer = std::thread::spawn(move || {
            barrier.wait();
            assert!(
                lock_fonts().primary.is_none(),
                "another worker's installed face must not leak into this context"
            );
            barrier.wait();
        });
        installer.join().expect("installer exits");
        observer.join().expect("observer exits");
    }

    /// `ui_text_width` is a usable layout metric with OR WITHOUT the system face:
    /// positive for nonempty text, empty == 0, and MONOTONIC — appending a
    /// character never shrinks the measure (the tracking is a fraction of any
    /// glyph advance). Runs identically on the mono fallback (headless CI that
    /// sandboxes /System reads), so the fallback path is covered by construction.
    #[test]
    fn ui_text_width_is_monotonic_and_falls_back_cleanly() {
        assert_eq!(ui_text_width("", 16.0), 0.0);
        let mut prev = 0.0;
        let mut s = String::new();
        for ch in "Appearance Wavy 17".chars() {
            s.push(ch);
            let w = ui_text_width(&s, 16.0);
            assert!(w > prev, "appending {ch:?} grew the measure: {w} vs {prev}");
            prev = w;
        }
        // The metric scales with px (a size ramp can never invert widths).
        assert!(ui_text_width("Cursor", 20.0) > ui_text_width("Cursor", 10.0));
    }

    #[test]
    fn ui_text_wrap_is_lossless_width_bounded_and_linear_at_update_cap() {
        let source = "x".repeat(256 * 1024);
        let (ranges, scans) = ui_text_wrap_ranges_impl(&source, 13.0, 420.0);
        assert!(!ranges.is_empty());
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges.last().map(|range| range.end), Some(source.len()));
        assert!(
            ranges.windows(2).all(|pair| pair[0].end == pair[1].start),
            "responsive wrapping must cover the authored source contiguously"
        );
        assert!(
            scans <= source.len() * 2,
            "near-cap unbroken input must remain linear: {scans} scans for {} graphemes",
            source.len()
        );
        assert!(
            ranges
                .iter()
                .all(|range| { ui_text_width(&source[range.clone()], 13.0) <= 420.5 })
        );
        assert_eq!(
            ranges
                .iter()
                .map(|range| &source[range.clone()])
                .collect::<String>(),
            source
        );
    }

    #[test]
    fn ui_text_wrap_preserves_repeated_and_unicode_whitespace_exactly() {
        let source = "\t  alpha\u{2003}\u{2003}beta  終\t tail";
        let ranges = ui_text_wrap_ranges(source, 13.0, 54.0);
        assert!(ranges.len() > 1, "fixture must exercise a real wrap");
        assert_eq!(
            ranges
                .iter()
                .map(|range| &source[range.clone()])
                .collect::<String>(),
            source
        );
    }

    #[test]
    fn ui_text_wrap_allows_only_indivisible_oversized_graphemes_to_overflow() {
        let oversized = "👨‍👩‍👧‍👦";
        let source = format!("{oversized}xx");
        assert_eq!(source.graphemes().count(), 3);
        let maximum_width = ui_text_width("x", 13.0) + 0.5;
        assert!(ui_text_width(oversized, 13.0) > maximum_width);

        let ranges = ui_text_wrap_ranges(&source, 13.0, maximum_width);
        assert_eq!(ranges.first().map(|range| range.start), Some(0));
        assert_eq!(ranges.last().map(|range| range.end), Some(source.len()));
        assert!(ranges.windows(2).all(|pair| pair[0].end == pair[1].start));
        assert_eq!(
            ranges
                .iter()
                .map(|range| &source[range.clone()])
                .collect::<String>(),
            source
        );
        let overflowing = ranges
            .iter()
            .filter(|range| {
                ui_text_width(&source[range.start..range.end], 13.0) > maximum_width + 0.5
            })
            .collect::<Vec<_>>();
        assert_eq!(overflowing.len(), 1);
        let overflow = overflowing[0];
        assert_eq!(&source[overflow.start..overflow.end], oversized);
        assert_eq!(source[overflow.start..overflow.end].graphemes().count(), 1);
    }

    /// The UI faces rasterize SOMETHING even when the system stack is absent, and a
    /// real semibold face covers more ink than regular on supported desktops.
    #[test]
    fn ui_faces_rasterize_and_bold_is_no_lighter() {
        // UI faces are HOST-PREPARED state now: `set_chrome_fonts` installs the
        // prepared assets into this context exactly like backend startup does
        // (raster itself never resolves fonts).
        set_chrome_fonts(None, None, None);
        let text = |face: TextFace| {
            // Route through the funnel (text_prim), baseline-positioned in a 28px
            // row, so this stays the sanctioned single construction site.
            let prims = vec![crate::widget::text_prim(
                2.0,
                row_baseline(0.0, 28.0, 16.0),
                "Appearance".to_string(),
                crate::type_scale::TypeStep::Body.px(16.0),
                TextWeight::Regular,
                face,
                [255, 255, 255, 255],
            )];
            let (px, _, _) = rasterize_tray(&prims, 120, 28, 1.0, [0, 0, 0, 255]);
            px.as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from(c[0]))
                .sum::<u32>()
        };
        let ui = text(TextFace::Ui);
        let bold = text(TextFace::UiBold);
        assert!(ui > 0, "Ui text rendered");
        assert!(
            bold > ui,
            "UiBold strikes more coverage than Ui: {bold} vs {ui}"
        );
    }

    #[test]
    fn blend_composites_in_linear_light() {
        // 50%-coverage opaque WHITE over opaque BLACK must land near the linear-light
        // midpoint (sRGB ~188 / 0xBC), NOT the gamma-space 128 — matching the main
        // glyph path, so thin tray text/edges don't smear.
        let mut cv = Canvas::new(1, 1, [0, 0, 0, 255]);
        cv.blend(0, 0, [255, 255, 255, 255], 0.5);
        assert!(
            (185..=190).contains(&cv.px[0]),
            "linear-light midpoint expected ~188, got {}",
            cv.px[0]
        );
    }

    #[test]
    fn glyph_rasterization_is_memoized_for_one_retained_surface() {
        let font = fontdue::Font::from_bytes(
            aterm_render::embedded_font(),
            fontdue::FontSettings::default(),
        )
        .expect("embedded font parses");
        let mut canvas = Canvas::new(1, 1, [0, 0, 0, 0]);

        let (first_metrics, first) = canvas.raster_glyph(&font, 'A', 13.0);
        let (second_metrics, second) = canvas.raster_glyph(&font, 'A', 13.0);

        assert_eq!(first_metrics, second_metrics);
        assert!(std::sync::Arc::ptr_eq(&first, &second));
        assert_eq!(canvas.glyphs.len(), 1);
    }

    #[test]
    fn threshold_oetf_matches_direct_rounded_srgb_quantization() {
        let direct = |linear: f32| {
            let linear = linear.clamp(0.0, 1.0);
            let encoded = if linear <= 0.003_130_8 {
                linear * 12.92
            } else {
                1.055 * linear.powf(1.0 / 2.4) - 0.055
            };
            (encoded * 255.0).round().clamp(0.0, 255.0) as u8
        };

        for sample in 0..=65_536 {
            let linear = sample as f32 / 65_536.0;
            assert_eq!(linear_to_srgb_u8(linear), direct(linear));
        }
        for output in 0..255 {
            let encoded = (output as f32 + 0.5) / 255.0;
            let threshold = if encoded <= 0.040_45 {
                encoded / 12.92
            } else {
                ((encoded + 0.055) / 1.055).powf(2.4)
            };
            for bits in
                threshold.to_bits().saturating_sub(2)..=threshold.to_bits().saturating_add(2)
            {
                let linear = f32::from_bits(bits);
                assert_eq!(
                    linear_to_srgb_u8(linear),
                    direct(linear),
                    "output boundary {output}, linear={linear:?}"
                );
            }
        }
    }

    #[test]
    fn scanline_round_rect_is_byte_exact_to_the_analytic_reference() {
        let cases = [
            (2.0, 3.0, 31.0, 22.0, 8.0),
            (1.25, 2.75, 34.5, 25.25, 11.5),
            (-4.0, -3.0, 21.0, 16.0, 5.0),
            (7.0, 5.0, 2.0, 17.0, 9.0),
            (4.0, 8.0, 29.0, 7.0, 0.0),
        ];
        let paints = [
            ([0, 0, 0, 0], [18, 28, 42, 255]),
            ([0, 0, 0, 0], [0, 0, 0, 48]),
            ([31, 47, 63, 255], [92, 176, 255, 73]),
        ];

        for &(backdrop, color) in &paints {
            for &(x, y, width, height, radius) in &cases {
                let mut optimized = Canvas::new(40, 32, backdrop);
                let mut reference = Canvas::new(40, 32, backdrop);
                optimized.push_clip(1.5, 1.25, 35.0, 27.5);
                reference.push_clip(1.5, 1.25, 35.0, 27.5);

                optimized.round_rect(x, y, width, height, radius, color);

                let (cx, cy, hx, hy) =
                    (x + width / 2.0, y + height / 2.0, width / 2.0, height / 2.0);
                let radius = radius.min(hx).min(hy);
                let x0 = (x - 1.0).floor() as i32;
                let y0 = (y - 1.0).floor() as i32;
                let x1 = (x + width + 1.0).ceil() as i32;
                let y1 = (y + height + 1.0).ceil() as i32;
                for py in y0..y1 {
                    for px in x0..x1 {
                        let distance =
                            sd_round_box(px as f32 + 0.5, py as f32 + 0.5, cx, cy, hx, hy, radius);
                        reference.blend(px, py, color, cov(distance));
                    }
                }

                assert_eq!(
                    optimized.px, reference.px,
                    "geometry=({x},{y},{width},{height},{radius}) color={color:?}"
                );
            }
        }
    }

    #[test]
    fn stroke_outline_is_hollow() {
        // A square 1px Stroke paints its border and leaves the interior at backdrop.
        let prims = vec![DrawPrim::Stroke {
            x: 2.0,
            y: 2.0,
            w: 16.0,
            h: 16.0,
            radius: 0.0,
            width: 1.0,
            color: [255, 0, 0, 255],
        }];
        let (px, pw, _ph) = rasterize_tray(&prims, 20, 20, 1.0, [0, 0, 0, 255]);
        let rgb = |x: u32, y: u32| {
            let i = ((y * pw + x) * 4) as usize;
            [px[i], px[i + 1], px[i + 2]]
        };
        assert_eq!(rgb(9, 2), [255, 0, 0], "top border drawn");
        assert_eq!(rgb(9, 9), [0, 0, 0], "interior stays hollow");
    }

    #[test]
    fn diagonal_line_segment_is_antialiased_round_capped_and_bounded() {
        let prims = vec![DrawPrim::Line {
            x1: 3.0,
            y1: 3.0,
            x2: 16.0,
            y2: 16.0,
            width: 2.0,
            color: [255, 255, 255, 255],
        }];
        let (px, pw, _ph) = rasterize_tray(&prims, 20, 20, 1.0, [0, 0, 0, 255]);
        let red = |x: u32, y: u32| px[((y * pw + x) * 4) as usize];
        assert_eq!(red(9, 9), 255, "center line is fully covered");
        assert!(
            (1..255).contains(&red(8, 9)),
            "diagonal edge carries fractional coverage"
        );
        assert_eq!(red(0, 19), 0, "pixels outside the capsule stay untouched");
    }

    #[test]
    fn clip_rejects_pixels_outside_the_pushed_rect() {
        // A full-canvas Panel between ClipPush/ClipPop only paints inside the clip.
        let prims = vec![
            DrawPrim::ClipPush {
                x: 4.0,
                y: 4.0,
                w: 4.0,
                h: 4.0,
            },
            DrawPrim::Panel {
                x: 0.0,
                y: 0.0,
                w: 16.0,
                h: 16.0,
                radius: 0.0,
                fill: [0, 255, 0, 255],
                blur: false,
            },
            DrawPrim::ClipPop,
        ];
        let (px, pw, _ph) = rasterize_tray(&prims, 16, 16, 1.0, [0, 0, 0, 255]);
        let rgb = |x: u32, y: u32| {
            let i = ((y * pw + x) * 4) as usize;
            [px[i], px[i + 1], px[i + 2]]
        };
        assert_eq!(rgb(5, 5), [0, 255, 0], "inside the clip is painted");
        assert_eq!(rgb(1, 1), [0, 0, 0], "outside the clip stays backdrop");
        assert_eq!(rgb(12, 12), [0, 0, 0], "outside the clip stays backdrop");
    }

    /// The HsvDisk raster: the centre (saturation 0) is the grey axis at
    /// `value` (white×value), the rim is anti-aliased over ~1px (a partial blend,
    /// neither full disk colour nor pure backdrop), and pixels past the rim stay
    /// untouched backdrop.
    #[test]
    fn hsv_disk_center_edge_and_outside() {
        // Centre the disk on a pixel SAMPLE point (x+0.5, y+0.5) so pixel (10,10)
        // reads the exact centre (saturation 0).
        let disk = |value: f32| DrawPrim::HsvDisk {
            cx: 10.5,
            cy: 10.5,
            r: 8.0,
            value,
        };
        let raster = |value: f32| rasterize_tray(&[disk(value)], 21, 21, 1.0, [0, 0, 0, 255]).0;
        let rgb = |px: &[u8], x: u32, y: u32| {
            let i = ((y * 21 + x) * 4) as usize;
            [px[i], px[i + 1], px[i + 2]]
        };
        let full = raster(1.0);
        assert_eq!(
            rgb(&full, 10, 10),
            [255, 255, 255],
            "centre = white at value 1"
        );
        // `value` scales the whole disk: the centre reads white×value.
        let half = raster(0.5);
        assert_eq!(
            rgb(&half, 10, 10),
            [128, 128, 128],
            "centre = white×0.5 at value 0.5"
        );
        // The rim pixel (sample at distance == r) has ~50% coverage — an AA blend
        // strictly between backdrop and the full edge colour.
        let edge = rgb(&full, 18, 10); // 3 o'clock: hue 0.25 = full-sat green-ish
        assert!(
            edge[1] > 0 && edge[1] < 255,
            "rim is anti-aliased, got {edge:?}"
        );
        // Outside the disk (past the 1px AA skirt): untouched backdrop.
        assert_eq!(rgb(&full, 0, 0), [0, 0, 0], "far corner untouched");
        assert_eq!(rgb(&full, 20, 10), [0, 0, 0], "just past the rim untouched");
    }

    #[test]
    fn hairline_stroke_snaps_to_one_device_row() {
        // A 1px-tall horizontal Stroke fully covers EXACTLY one device row (no smear).
        let prims = vec![DrawPrim::Stroke {
            x: 0.0,
            y: 4.0,
            w: 10.0,
            h: 1.0,
            radius: 0.0,
            width: 1.0,
            color: [255, 255, 255, 255],
        }];
        let (px, pw, ph) = rasterize_tray(&prims, 10, 10, 1.0, [0, 0, 0, 255]);
        let row_white = |y: u32| (0..pw).all(|x| px[((y * pw + x) * 4) as usize] == 255);
        let covered: Vec<u32> = (0..ph).filter(|&y| row_white(y)).collect();
        assert_eq!(covered, vec![4], "exactly the y=4 row is fully white");
    }

    #[test]
    fn regional_raster_is_byte_exact_slice_of_full_raster() {
        let (w, h, scale) = (180_u32, 96_u32, 1.5_f32);
        let mut prims = vec![DrawPrim::Panel {
            x: 0.0,
            y: 0.0,
            w: w as f32,
            h: h as f32,
            radius: 0.0,
            fill: [21, 24, 31, 255],
            blur: false,
        }];
        prims.extend(demo_prims(w as f32, h as f32, Theme::default()));
        prims.extend([
            DrawPrim::ClipPush {
                x: 150.0,
                y: 4.0,
                w: 24.0,
                h: 30.0,
            },
            DrawPrim::Panel {
                x: 150.0,
                y: 4.0,
                w: 24.0,
                h: 30.0,
                radius: 5.0,
                fill: [230, 90, 75, 255],
                blur: false,
            },
            DrawPrim::ClipPop,
        ]);
        let (full, full_width, full_height) = rasterize_tray(&prims, w, h, scale, [4, 7, 9, 255]);
        let region = RasterRect {
            x: 37,
            y: 19,
            width: 151,
            height: 83,
        };
        assert!(region.x + region.width <= full_width);
        assert!(region.y + region.height <= full_height);

        let regional = rasterize_tray_region(&prims, scale, [4, 7, 9, 255], region);
        let mut expected = Vec::with_capacity(regional.len());
        for row in region.y..region.y + region.height {
            let start = ((row * full_width + region.x) * 4) as usize;
            expected.extend_from_slice(&full[start..start + (region.width * 4) as usize]);
        }
        assert_eq!(regional, expected);
        assert_eq!(
            regional.len() as u64,
            region.pixels() * 4,
            "instrumentation counts only the device pixels inside the tile"
        );
        assert!(region.pixels() < u64::from(full_width) * u64::from(full_height));
    }

    #[test]
    fn raster_patch_preserves_every_byte_outside_its_region() {
        let (width, height) = (11_u32, 9_u32);
        let mut destination = (0..width * height * 4)
            .map(|byte| byte.wrapping_mul(37) as u8)
            .collect::<Vec<_>>();
        let before = destination.clone();
        let region = RasterRect {
            x: 3,
            y: 2,
            width: 4,
            height: 5,
        };
        let patch = vec![0xA7; region.pixels() as usize * 4];
        assert!(apply_raster_patch(
            &mut destination,
            width,
            height,
            region,
            &patch,
        ));
        for y in 0..height {
            for x in 0..width {
                let index = ((y * width + x) * 4) as usize;
                if x >= region.x
                    && x < region.x + region.width
                    && y >= region.y
                    && y < region.y + region.height
                {
                    assert_eq!(&destination[index..index + 4], &[0xA7; 4]);
                } else {
                    assert_eq!(&destination[index..index + 4], &before[index..index + 4]);
                }
            }
        }
    }

    #[test]
    fn regional_patch_matches_a_fresh_full_raster_after_a_local_change() {
        let (logical_width, logical_height, scale) = (120_u32, 80_u32, 2.0_f32);
        let scene = |fill| {
            vec![
                DrawPrim::Panel {
                    x: 0.0,
                    y: 0.0,
                    w: logical_width as f32,
                    h: logical_height as f32,
                    radius: 0.0,
                    fill: [17, 21, 29, 255],
                    blur: false,
                },
                DrawPrim::Panel {
                    x: 30.0,
                    y: 20.0,
                    w: 50.0,
                    h: 30.0,
                    radius: 6.0,
                    fill,
                    blur: false,
                },
            ]
        };
        let old = scene([80, 120, 210, 255]);
        let new = scene([70, 205, 145, 255]);
        let (mut patched, full_width, full_height) =
            rasterize_tray(&old, logical_width, logical_height, scale, [0, 0, 0, 0]);
        let fresh = rasterize_tray(&new, logical_width, logical_height, scale, [0, 0, 0, 0]).0;
        let region = RasterRect::from_logical(28, 18, 55, 35, scale, full_width, full_height)
            .expect("local change maps to device pixels");
        let tile = rasterize_tray_region(&new, scale, [0, 0, 0, 0], region);
        assert!(apply_raster_patch(
            &mut patched,
            full_width,
            full_height,
            region,
            &tile,
        ));
        assert_eq!(
            patched, fresh,
            "patching the declared local change is pixel-identical to rerasterizing the scene"
        );
    }

    #[test]
    fn rasterizes_a_nonempty_tray() {
        let (cw, ch) = (380.0_f32, 168.0_f32);
        let mut prims = vec![DrawPrim::Panel {
            x: 0.0,
            y: 0.0,
            w: cw,
            h: ch,
            radius: 16.0,
            fill: [24, 27, 33, 0xC4],
            blur: true,
        }];
        prims.extend(demo_prims(cw, ch, Theme::default()));
        let (px, pw, ph) = rasterize_tray(&prims, cw as u32, ch as u32, 2.0, [13, 15, 20, 255]);
        assert_eq!(px.len(), (pw * ph * 4) as usize);
        // not a flat image: some pixel differs from the backdrop (the rings drew)
        assert!(
            px.as_chunks::<4>()
                .0
                .iter()
                .any(|c| *c != [13, 15, 20, 255]),
            "tray rendered content over the backdrop"
        );

        // Emit a preview PNG for visual inspection when ATERM_TRAY_PREVIEW is set.
        if let Ok(path) = std::env::var("ATERM_TRAY_PREVIEW") {
            let cv = Canvas {
                origin_x: 0,
                origin_y: 0,
                w: pw,
                h: ph,
                px: px.clone(),
                glyphs: std::collections::HashMap::new(),
                clip: Vec::new(),
            };
            let _ = std::fs::write(path, cv.to_png());
        }
    }

    /// Render the tray across several built-in themes (panel + colors DERIVED from each
    /// theme, backdrop = each theme's bg), stacked into one composite PNG, so the tray's
    /// theme-awareness is visible. Gated on ATERM_TRAY_THEMES=path.
    #[test]
    fn previews_across_themes() {
        let Ok(path) = std::env::var("ATERM_TRAY_THEMES") else {
            return;
        };
        let names = ["Default", "Dracula", "GitHub Light", "Gruvbox Light"];
        let (cw, ch) = (380.0_f32, 168.0_f32);
        let scale = 2.0_f32;
        let (tw, th) = ((cw * scale) as u32, (ch * scale) as u32);
        let gap = 16u32;
        let comp_w = tw + 2 * gap;
        let comp_h = (th + gap) * names.len() as u32 + gap;
        let mut comp = vec![0u8; (comp_w * comp_h * 4) as usize];
        // neutral mid backdrop so both dark + light cards read against it
        for c in comp.as_chunks_mut::<4>().0 {
            c.copy_from_slice(&[40, 42, 48, 255]);
        }
        for (i, name) in names.iter().enumerate() {
            let theme = aterm_types::scheme::builtin(name).map_or_else(Theme::default, |s| {
                let p = s.to_theme_parts();
                Theme {
                    fg: p.fg,
                    bg: p.bg,
                    cursor: p.cursor,
                    selection: p.selection,
                }
            });
            let bg = theme.bg;
            let backdrop = [
                ((bg >> 16) & 0xff) as u8,
                ((bg >> 8) & 0xff) as u8,
                (bg & 0xff) as u8,
                255,
            ];
            let panel = crate::chrome_band::band_colors(theme).bar_bg;
            let mut prims = vec![DrawPrim::Panel {
                x: 0.0,
                y: 0.0,
                w: cw,
                h: ch,
                radius: 16.0,
                fill: [panel[0], panel[1], panel[2], 0xF0],
                blur: true,
            }];
            prims.extend(demo_prims(cw, ch, theme));
            let (px, pw, ph) = rasterize_tray(&prims, cw as u32, ch as u32, scale, backdrop);
            // blit into the composite
            let oy = gap + i as u32 * (th + gap);
            let ox = gap;
            for row in 0..ph.min(th) {
                let src = (row * pw * 4) as usize;
                let dst = (((oy + row) * comp_w + ox) * 4) as usize;
                let n = (pw.min(tw) * 4) as usize;
                comp[dst..dst + n].copy_from_slice(&px[src..src + n]);
            }
        }
        let cv = Canvas {
            origin_x: 0,
            origin_y: 0,
            w: comp_w,
            h: comp_h,
            px: comp,
            glyphs: std::collections::HashMap::new(),
            clip: Vec::new(),
        };
        let _ = std::fs::write(path, cv.to_png());
    }
}
