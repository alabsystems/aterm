// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A small anti-aliased CPU rasterizer for the widget tray's [`DrawPrim`] list.

use std::collections::HashMap;
use std::f32::consts::TAU;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use aterm_grapheme::GraphemeClusters;
use aterm_render::font::{Font as ChromeFont, Metrics as ChromeMetrics};

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

/// Each `(slot, family)` (`regular`, `bold italic`, `emoji`, …) a
/// [`SemanticFontResolution::Fallback`] status names as unresolved, so the specimen's status names
/// the font that failed rather than the row that has focus.
pub(crate) fn unresolved_slots(status: &str) -> Vec<(&'static str, String)> {
    const SLOTS: [&str; 7] = [
        "regular",
        "bold italic",
        "bold",
        "italic",
        "fallback",
        "symbol",
        "emoji",
    ];
    let Some(rest) = status.split_once("unresolved ").map(|(_, rest)| rest) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut rest = rest;
    while let Some(slot) = SLOTS
        .iter()
        .find(|slot| rest.starts_with(&format!("{slot} \"")))
    {
        rest = &rest[slot.len() + 2..];
        let mut family = String::new();
        let mut chars = rest.char_indices();
        let mut end = None;
        while let Some((at, ch)) = chars.next() {
            match ch {
                '\\' => {
                    if let Some((_, escaped)) = chars.next() {
                        family.push(escaped);
                    }
                }
                '"' => {
                    end = Some(at + 1);
                    break;
                }
                _ => family.push(ch),
            }
        }
        let Some(end) = end else {
            break;
        };
        out.push((*slot, family));
        let tail = &rest[end..];
        let tail = tail
            .split_once("; requested")
            .map_or(tail, |(head, _)| head);
        let Some(next) = SLOTS
            .iter()
            .filter_map(|slot| tail.find(&format!(", {slot} \"")))
            .min()
        else {
            break;
        };
        rest = &tail[next + 2..];
    }
    out
}

#[derive(Clone)]
struct ResolvedCandidateFace {
    face: aterm_render::FaceRef,
    bytes: Arc<Vec<u8>>,
}

type CandidateFaceCache =
    HashMap<(String, aterm_render::FaceTarget), Result<ResolvedCandidateFace, String>>;

struct SemanticPrewarmJob {
    generation: u64,
    request: u64,
    candidate: crate::widget::SemanticFontCandidate,
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

#[derive(Default)]
struct SemanticPrewarmSlot {
    job: Option<SemanticPrewarmJob>,
    closed: bool,
}

struct SemanticPrewarmQueue {
    semantic_job: Mutex<SemanticPrewarmSlot>,
    ready: Condvar,
}

impl SemanticPrewarmQueue {
    fn new() -> Self {
        Self {
            semantic_job: Mutex::new(SemanticPrewarmSlot::default()),
            ready: Condvar::new(),
        }
    }

    #[cfg(test)]
    fn with_job(job: SemanticPrewarmJob) -> Self {
        Self {
            semantic_job: Mutex::new(SemanticPrewarmSlot {
                job: Some(job),
                closed: false,
            }),
            ready: Condvar::new(),
        }
    }
}

struct SemanticPrewarmWorker {
    queue: Arc<SemanticPrewarmQueue>,
    results: std::sync::mpsc::Receiver<SemanticPrewarmResult>,
}

/// The event loop's wake for a chrome font that finished loading off the event loop: a
/// semantic renderer the prewarm worker built, or a face the coverage warm parsed
/// ([`chrome_coverage_epoch`]).
static SEMANTIC_READY_WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Install [`SEMANTIC_READY_WAKE`]; the first install wins.
pub(crate) fn set_semantic_ready_wake(wake: impl Fn() + Send + Sync + 'static) {
    let _ = SEMANTIC_READY_WAKE.set(Box::new(wake));
}

impl SemanticPrewarmWorker {
    fn spawn() -> Option<Self> {
        Self::spawn_serialized_by(&SEMANTIC_PREWARM_GATE, crate::qos::Role::Background)
    }

    fn spawn_serialized_by(gate: &'static Mutex<()>, role: crate::qos::Role) -> Option<Self> {
        let queue = Arc::new(SemanticPrewarmQueue::new());
        let worker_queue = Arc::clone(&queue);
        let (tx, results) = std::sync::mpsc::channel();
        crate::qos::spawn_lane_thread("aterm-semantic-font-prewarm", role, move || {
            let mut base: Option<(u64, Renderer)> = None;
            let mut faces = CandidateFaceCache::new();
            loop {
                let mut slot = worker_queue
                    .semantic_job
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                while slot.job.is_none() && !slot.closed {
                    slot = worker_queue
                        .ready
                        .wait(slot)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                if slot.closed {
                    break;
                }
                let Some(job) = slot.job.take() else {
                    continue;
                };
                drop(slot);

                let _serial = gate
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                {
                    let current = worker_queue
                        .semantic_job
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if current.closed {
                        break;
                    }
                    if current
                        .job
                        .as_ref()
                        .is_some_and(|newer| newer.generation != job.generation)
                    {
                        continue;
                    }
                }
                let started = std::time::Instant::now();
                if let Some(mut renderer) = job.base {
                    renderer.prepare_semantic_typography(
                        "Input→present λ π √ ✓ 你好世界 日本語 한글 🚀 😀 🐈‍⬛ e\u{301}",
                    );
                    base = Some((job.generation, renderer));
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
                if let Some(wake) = SEMANTIC_READY_WAKE.get() {
                    wake();
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
        if pending.closed {
            return;
        }
        if let Some(previous) = pending.job.take() {
            let carries_base = crate::semantic_prewarm_replacement_carries_base(
                job.base.is_some(),
                previous.base.is_some(),
            );
            if job.base.is_none() && carries_base {
                job.base = previous.base;
            }
        }
        pending.job = Some(job);
        drop(pending);
        self.queue.ready.notify_one();
    }

    fn cancel_queued(&self) {
        self.queue
            .semantic_job
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .job
            .take();
    }
}

impl Drop for SemanticPrewarmWorker {
    fn drop(&mut self) {
        let mut slot = self
            .queue
            .semantic_job
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.job = None;
        slot.closed = true;
        drop(slot);
        self.queue.ready.notify_all();
    }
}

fn resolve_candidate_face(
    family: &str,
    target: aterm_render::FaceTarget,
    cache: &mut CandidateFaceCache,
) -> Result<ResolvedCandidateFace, String> {
    let key = (family.trim().to_ascii_lowercase(), target);
    if let Some(cached) = cache.get(&key) {
        return cached.clone();
    }
    let resolved = aterm_render::resolve_config_font(family, target).and_then(|face| {
        aterm_render::read_resolved_font(&face.path)
            .map(|bytes| ResolvedCandidateFace {
                bytes: aterm_render::intern_font_bytes(bytes.into_owned()),
                face,
            })
            .map_err(|error| format!("font {family:?} could not be read ({error})"))
    });
    cache.insert(key, resolved.clone());
    resolved
}

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
    let mut unresolved = Vec::new();
    if let Some(family) = candidate.regular.as_deref() {
        match resolve_candidate_face(family, aterm_render::FaceTarget::REGULAR, faces).and_then(
            |face| renderer.set_primary_font_from_resolved_path(face.bytes.as_slice(), face.face),
        ) {
            Ok(()) => {}
            Err(error) => unresolved.push(format!("regular {family:?}: {error}")),
        }
    }
    if let Err(error) = renderer.set_display_faces(&candidate.display) {
        unresolved.push(format!("display {:?}: {error}", candidate.display));
    }
    for (slot, label, family) in [
        (0, "bold", candidate.bold.as_deref()),
        (1, "italic", candidate.italic.as_deref()),
        (2, "bold italic", candidate.bold_italic.as_deref()),
    ] {
        let Some(family) = family else {
            continue;
        };
        match resolve_candidate_face(family, aterm_render::FaceTarget::for_slot(slot), faces)
            .and_then(|face| renderer.set_styled_face(slot, face.bytes.as_slice(), face.face.index))
        {
            Ok(()) => {}
            Err(error) => unresolved.push(format!("{label} {family:?}: {error}")),
        }
    }
    let mut installed_fallback = false;
    for family in &candidate.fallback {
        match resolve_candidate_face(family, aterm_render::FaceTarget::REGULAR, faces).and_then(
            |face| {
                if installed_fallback {
                    renderer.add_fallback_bytes(face.bytes.as_slice())
                } else {
                    renderer.set_fallback_bytes(face.bytes.as_slice())
                }
            },
        ) {
            Ok(()) => installed_fallback = true,
            Err(error) => unresolved.push(format!("fallback {family:?}: {error}")),
        }
    }
    if let Some(family) = candidate.symbol.as_deref() {
        match resolve_candidate_face(family, aterm_render::FaceTarget::REGULAR, faces)
            .and_then(|face| renderer.set_symbol_fallback_bytes(face.bytes.as_slice()))
        {
            Ok(()) => {}
            Err(error) => unresolved.push(format!("symbol {family:?}: {error}")),
        }
    }
    if let Some(family) = candidate.emoji.as_deref() {
        match resolve_candidate_face(family, aterm_render::FaceTarget::REGULAR, faces)
            .and_then(|face| renderer.set_color_font_arc(Arc::clone(&face.bytes)))
        {
            Ok(()) => {}
            Err(error) => unresolved.push(format!("emoji {family:?}: {error}")),
        }
    }
    renderer.set_synthetic_styles(candidate.synthetic_styles);
    let variations = candidate.variation_requests();
    let _ = renderer.set_font_variations(&variations, 0.0);
    let _ = renderer.seal_admitted_font_sources();
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

use aterm_render::Renderer;
use aterm_render::chrome_metrics::{baseline_in_row_q, px_to_q, q_round_to_px, q_to_px};

use crate::widget::{DrawPrim, SpecimenTextBlending, TerminalSpecimenSpec, TextFace, TextWeight};

/// How the chrome turns a char into a glyph of a [`ChromeFont`]: through the face's Unicode cmap
/// subtables ([`ChromeFont::unicode_glyph_index`]), as the terminal grid draws its primary face,
/// never through `lookup_glyph_index`.
pub(crate) trait UnicodeChars {
    fn maps(&self, ch: char) -> bool;
    fn char_metrics(&self, ch: char, px: f32) -> ChromeMetrics;
    fn char_raster(&self, ch: char, px: f32) -> (ChromeMetrics, Vec<u8>);
    fn char_raster_at(&self, ch: char, px: f32, dx: f32) -> (ChromeMetrics, Vec<u8>);
    fn char_kern(&self, left: char, right: char, px: f32) -> Option<f32>;
}

impl UnicodeChars for ChromeFont {
    fn maps(&self, ch: char) -> bool {
        self.unicode_glyph_index(ch) != 0
    }

    fn char_metrics(&self, ch: char, px: f32) -> ChromeMetrics {
        self.metrics_indexed(self.unicode_glyph_index(ch), px)
    }

    fn char_raster(&self, ch: char, px: f32) -> (ChromeMetrics, Vec<u8>) {
        self.rasterize_indexed(self.unicode_glyph_index(ch), px)
    }

    fn char_raster_at(&self, ch: char, px: f32, dx: f32) -> (ChromeMetrics, Vec<u8>) {
        self.rasterize_indexed_at(self.unicode_glyph_index(ch), px, dx)
    }

    fn char_kern(&self, left: char, right: char, px: f32) -> Option<f32> {
        self.horizontal_kern_indexed(
            self.unicode_glyph_index(left),
            self.unicode_glyph_index(right),
            px,
        )
    }
}

struct ChromeFace {
    font: std::sync::Arc<ChromeFont>,
    cap_ratio: f32,
    advances: std::collections::HashMap<char, f32>,
}

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

    fn from_font(font: std::sync::Arc<ChromeFont>) -> Self {
        Self {
            font,
            cap_ratio: 0.7,
            advances: std::collections::HashMap::new(),
        }
    }

    fn has(&self, ch: char) -> bool {
        self.font.maps(ch)
    }

    fn advance_em(&mut self, ch: char) -> f32 {
        if let Some(&a) = self.advances.get(&ch) {
            return a;
        }
        let a = self.font.char_metrics(ch, ADVANCE_REF_PX).advance_width / ADVANCE_REF_PX;
        self.advances.insert(ch, a);
        a
    }
}

mod synthesized {
    use super::ChromeMetrics;

    /// Whether the pen draws `ch` itself when no loaded face covers it.
    pub(super) const fn covers(ch: char) -> bool {
        matches!(ch, '\u{2139}' | '\u{23f8}')
    }

    struct Design {
        left: f32,
        right: f32,
        bottom: f32,
        top: f32,
        advance: f32,
        inside: fn(f32, f32, f32) -> bool,
    }

    const INFO: Design = Design {
        left: 0.05,
        right: 0.71,
        bottom: 0.03,
        top: 0.69,
        advance: 0.76,
        inside: info_inside,
    };

    const PAUSE: Design = Design {
        left: 0.07,
        right: 0.53,
        bottom: 0.08,
        top: 0.64,
        advance: 0.6,
        inside: pause_inside,
    };

    fn info_inside(x: f32, y: f32, px: f32) -> bool {
        const CX: f32 = 0.38;
        const CY: f32 = 0.36;
        const OUTER: f32 = 0.33;
        let dx = x - CX;
        let stroke = 0.08_f32.max(1.0 / px);
        let ring = (OUTER - stroke..=OUTER).contains(&(dx * dx + (y - CY) * (y - CY)).sqrt());
        let stem_w = (0.10 * px).round().max(1.0);
        let stem_left = (CX * px - stem_w / 2.0).round();
        let stem =
            (stem_left..stem_left + stem_w).contains(&(x * px)) && (0.15..=0.39).contains(&y);
        let dot = (dx * dx + (y - 0.505) * (y - 0.505)).sqrt() <= 0.06_f32.max(0.6 / px);
        ring || stem || dot
    }

    fn pause_inside(x: f32, y: f32, _px: f32) -> bool {
        (0.08..=0.64).contains(&y) && ((0.07..=0.23).contains(&x) || (0.37..=0.53).contains(&x))
    }

    const fn design(ch: char) -> Option<&'static Design> {
        match ch {
            '\u{2139}' => Some(&INFO),
            '\u{23f8}' => Some(&PAUSE),
            _ => None,
        }
    }

    /// The advance of `ch` at `px`, or `None` when the pen does not draw it.
    pub(super) fn advance(ch: char, px: f32) -> Option<f32> {
        design(ch).map(|d| d.advance * px)
    }

    /// The pen-owned glyph's placement without allocating its coverage bitmap.
    pub(super) fn metrics(ch: char, px: f32) -> Option<ChromeMetrics> {
        let d = design(ch)?;
        let xmin = (d.left * px).floor() as i32;
        let ymin = (d.bottom * px).floor() as i32;
        let top = (d.top * px).ceil() as i32;
        let width = ((d.right * px).ceil() as i32 - xmin).max(0) as usize;
        let height = (top - ymin).max(0) as usize;
        Some(ChromeMetrics {
            xmin,
            ymin,
            width,
            height,
            advance_width: d.advance * px,
        })
    }

    const SUB: usize = 4;

    /// `ch` at `px`: its coverage bitmap and placement in the convention every glyph the pen blits
    /// uses (`xmin`/`ymin` from the pen and the baseline, rows top-down), or `None` for a char the
    /// pen does not draw.
    pub(super) fn rasterize(ch: char, px: f32) -> Option<(ChromeMetrics, Vec<u8>)> {
        let d = design(ch)?;
        let metrics = metrics(ch, px)?;
        let (xmin, top, width, height) = (
            metrics.xmin,
            metrics.ymin + metrics.height as i32,
            metrics.width,
            metrics.height,
        );
        if width == 0 || height == 0 {
            return Some((metrics, Vec::new()));
        }
        let step = 1.0 / SUB as f32;
        let mut bitmap = vec![0u8; width * height];
        for (row, line) in bitmap.chunks_mut(width).enumerate() {
            let y_px = (top - row as i32) as f32;
            for (col, alpha) in line.iter_mut().enumerate() {
                let x_px = (xmin + col as i32) as f32;
                let mut hits = 0;
                for sy in 0..SUB {
                    for sx in 0..SUB {
                        let x = (x_px + (sx as f32 + 0.5) * step) / px;
                        let y = (y_px - (sy as f32 + 0.5) * step) / px;
                        if (d.inside)(x, y, px) {
                            hits += 1;
                        }
                    }
                }
                *alpha = ((hits * 255 + SUB * SUB / 2) / (SUB * SUB)) as u8;
            }
        }
        Some((metrics, bitmap))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UiCjkWeight {
    Regular,
    Semibold,
}

impl UiCjkWeight {
    fn of(face: TextFace) -> Self {
        if face == TextFace::UiBold {
            Self::Semibold
        } else {
            Self::Regular
        }
    }

    const fn slot(self) -> usize {
        match self {
            Self::Regular => 0,
            Self::Semibold => 1,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UiCjkScript {
    Han,
    Hangul,
}

/// Whether `ch` is in one of the East Asian blocks ([`ui_cjk_script`]), whose text breaks between
/// its characters with no hyphen.
pub(crate) fn is_east_asian(ch: char) -> bool {
    ui_cjk_script(ch).is_some()
}

fn ui_cjk_script(ch: char) -> Option<UiCjkScript> {
    match u32::from(ch) {
        0x1100..=0x11FF
        | 0x3130..=0x318F
        | 0x3200..=0x321E
        | 0x3260..=0x327E
        | 0xA960..=0xA97F
        | 0xAC00..=0xD7FF
        | 0xFFA0..=0xFFDC => Some(UiCjkScript::Hangul),
        0x2E80..=0x2FFF
        | 0x3000..=0x312F
        | 0x3190..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF
        | 0xFE10..=0xFE1F
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF9F
        | 0xFFE0..=0xFFEF
        | 0x1B000..=0x1B16F
        | 0x20000..=0x3FFFF => Some(UiCjkScript::Han),
        _ => None,
    }
}

#[cfg(windows)]
const UI_CJK_FILES: [&[(&str, u32, UiCjkScript)]; 2] = [
    &[
        ("YuGothM.ttc", 1, UiCjkScript::Han),
        ("msjh.ttc", 1, UiCjkScript::Han),
        ("malgun.ttf", 0, UiCjkScript::Hangul),
    ],
    &[
        ("YuGothB.ttc", 2, UiCjkScript::Han),
        ("msjhbd.ttc", 1, UiCjkScript::Han),
        ("malgunbd.ttf", 0, UiCjkScript::Hangul),
    ],
];

#[cfg(not(windows))]
const UI_CJK_FILES: [&[(&str, u32, UiCjkScript)]; 2] = [&[], &[]];

struct UiCjkFace {
    path: std::path::PathBuf,
    index: u32,
    script: UiCjkScript,
    font: OnceLock<Option<Arc<ChromeFont>>>,
}

impl UiCjkFace {
    fn parsed(&self) -> Option<&Arc<ChromeFont>> {
        self.font.get().and_then(Option::as_ref)
    }

    fn materialise(&self) -> (Option<&Arc<ChromeFont>>, bool) {
        let mut landed = false;
        if self.font.get().is_none() {
            let parsed = aterm_render::font_file::admit_font_file(&self.path)
                .ok()
                .and_then(|bytes| {
                    ChromeFont::from_shared(
                        bytes,
                        aterm_render::font::FontSettings {
                            collection_index: self.index,
                        },
                    )
                    .ok()
                })
                .map(Arc::new);
            let some = parsed.is_some();
            landed = self.font.set(parsed).is_ok() && some;
        }
        (self.parsed(), landed)
    }
}

/// The chrome's own fallback for East Asian text in a UI-face run (the tab strip's titles,
/// Settings), at the run's weight: the faces Windows itself falls back to from Segoe UI
/// ([`UI_CJK_FILES`]).
pub(crate) struct UiCjkFaces {
    rungs: [Vec<UiCjkFace>; 2],
}

impl UiCjkFaces {
    fn host() -> Option<Self> {
        #[cfg(windows)]
        let root = std::env::var_os("WINDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
            .join("Fonts");
        #[cfg(not(windows))]
        let root = std::path::PathBuf::new();
        let rung = |files: &[(&str, u32, UiCjkScript)]| -> Vec<UiCjkFace> {
            files
                .iter()
                .map(|&(file, index, script)| UiCjkFace {
                    path: root.join(file),
                    index,
                    script,
                    font: OnceLock::new(),
                })
                .collect()
        };
        let store = Self {
            rungs: [rung(UI_CJK_FILES[0]), rung(UI_CJK_FILES[1])],
        };
        (!store.rungs.iter().all(Vec::is_empty)).then_some(store)
    }

    fn rung(&self, weight: UiCjkWeight) -> &[UiCjkFace] {
        &self.rungs[weight.slot()]
    }

    fn materialise_for(&self, weight: UiCjkWeight, text: &str) -> (usize, String) {
        let mut chars: Vec<char> = text
            .chars()
            .filter(|ch| !ch.is_whitespace() && !ch.is_control())
            .collect();
        chars.sort_unstable();
        chars.dedup();
        let mut landed = 0;
        let mut rest = String::new();
        'chars: for ch in chars {
            if let Some(script) = ui_cjk_script(ch) {
                for face in self
                    .rung(weight)
                    .iter()
                    .filter(|face| face.script == script)
                {
                    let (font, fresh) = face.materialise();
                    landed += usize::from(fresh);
                    if font.is_some_and(|font| font.maps(ch)) {
                        continue 'chars;
                    }
                }
            }
            rest.push(ch);
        }
        (landed, rest)
    }

    fn settled(&self, weight: UiCjkWeight, ch: char) -> Option<bool> {
        let Some(script) = ui_cjk_script(ch) else {
            return Some(false);
        };
        for face in self
            .rung(weight)
            .iter()
            .filter(|face| face.script == script)
        {
            match face.font.get() {
                None => return None,
                Some(Some(font)) if font.maps(ch) => return Some(true),
                Some(_) => {}
            }
        }
        Some(false)
    }

    /// The parsed faces, as `file#index`, Regular rung first.
    #[cfg(all(test, windows))]
    pub(crate) fn parsed_files_for_test(&self) -> Vec<String> {
        self.rungs
            .iter()
            .flatten()
            .filter(|face| face.parsed().is_some())
            .map(|face| {
                let file = face.path.file_name().unwrap_or_default().to_string_lossy();
                format!("{file}#{}", face.index)
            })
            .collect()
    }

    fn parsed_count(&self) -> usize {
        self.rungs
            .iter()
            .flatten()
            .filter(|face| face.parsed().is_some())
            .count()
    }
}

#[cfg(not(test))]
fn ui_cjk_store_for_install() -> Option<Arc<UiCjkFaces>> {
    static CJK_STORE: OnceLock<Option<Arc<UiCjkFaces>>> = OnceLock::new();
    CJK_STORE
        .get_or_init(|| UiCjkFaces::host().map(Arc::new))
        .clone()
}

#[cfg(test)]
fn ui_cjk_store_for_install() -> Option<Arc<UiCjkFaces>> {
    UiCjkFaces::host().map(Arc::new)
}

struct ChromeFonts {
    primary: Option<ChromeFace>,
    bold: Option<ChromeFace>,
    fallback: Option<ChromeFace>,
    coverage: Vec<ChromeFace>,
    ui_cjk: Option<Arc<UiCjkFaces>>,
    ui_cjk_held: [Vec<Option<ChromeFace>>; 2],
    ui_regular: Option<Arc<ChromeFont>>,
    ui_semibold: Option<Arc<ChromeFont>>,
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
    semantic_displaced: Option<crate::widget::SemanticFontCandidate>,
    semantic_ready_epoch: u64,
    semantic_prewarm_ms: Option<u64>,
    measures: MeasureMemo,
    /// The glyphs rasters drew, kept for the next ([`GlyphMemo`]).
    glyphs: GlyphMemo,
    /// Chars chrome text measured or drew with no face of this stack ([`chrome_text_holes`]).
    text_holes: u64,
}

#[derive(Default)]
struct MeasureMemo {
    widths: aterm_hash::FxHashMap<(u16, u32), HeldMeasures<f32>>,
    wraps: aterm_hash::FxHashMap<(u32, u32), HeldMeasures<WrapLines>>,
    lines: aterm_hash::FxHashMap<(LinesRule, u16, u32, u32), HeldMeasures<KeptLines>>,
    entries: usize,
}

type WrapLines = (Vec<std::ops::Range<usize>>, usize);

/// Lines [`kept_lines`] keeps, with each char of their text the UI face has no glyph for and the
/// advance it measured at ([`ui_misses`]): the lines stand while every one still measures so.
type KeptLines = (Vec<String>, Box<[(char, u32)]>);

type HeldMeasures<T> = (Arc<ChromeFont>, aterm_hash::KeyedHashMap<Box<str>, T>);

const MEASURE_MEMO_ENTRIES: usize = 16_384;

fn face_code(face: TextFace) -> u16 {
    match face {
        TextFace::Mono => 0,
        TextFace::Ui => 1,
        TextFace::UiBold => 2,
        TextFace::Display(index) => 3 + u16::from(index),
    }
}

impl MeasureMemo {
    fn width(&self, face: TextFace, font: &Arc<ChromeFont>, px: f32, s: &str) -> Option<f32> {
        let (held, widths) = self.widths.get(&(face_code(face), px.to_bits()))?;
        Arc::ptr_eq(held, font)
            .then(|| widths.get(s).copied())
            .flatten()
    }

    fn keep_width(&mut self, face: TextFace, font: &Arc<ChromeFont>, px: f32, s: &str, w: f32) {
        self.make_room();
        let (held, widths) = self
            .widths
            .entry((face_code(face), px.to_bits()))
            .or_insert_with(|| (Arc::clone(font), aterm_hash::KeyedHashMap::default()));
        if !Arc::ptr_eq(held, font) {
            *held = Arc::clone(font);
            self.entries = self.entries.saturating_sub(widths.len());
            widths.clear();
        }
        if widths.insert(s.into(), w).is_none() {
            self.entries += 1;
        }
    }

    fn wrap(&self, font: &Arc<ChromeFont>, px: f32, width: f32, text: &str) -> Option<WrapLines> {
        let (held, wraps) = self.wraps.get(&(px.to_bits(), width.to_bits()))?;
        Arc::ptr_eq(held, font)
            .then(|| wraps.get(text).cloned())
            .flatten()
    }

    fn keep_wrap(
        &mut self,
        font: &Arc<ChromeFont>,
        px: f32,
        width: f32,
        text: &str,
        lines: &WrapLines,
    ) {
        self.make_room();
        let (held, wraps) = self
            .wraps
            .entry((px.to_bits(), width.to_bits()))
            .or_insert_with(|| (Arc::clone(font), aterm_hash::KeyedHashMap::default()));
        if !Arc::ptr_eq(held, font) {
            *held = Arc::clone(font);
            self.entries = self.entries.saturating_sub(wraps.len());
            wraps.clear();
        }
        if wraps.insert(text.into(), lines.clone()).is_none() {
            self.entries += 1;
        }
    }

    fn lines(&self, at: LinesAt, font: &Arc<ChromeFont>, text: &str) -> Option<&KeptLines> {
        let (held, kept) = self.lines.get(&at)?;
        Arc::ptr_eq(held, font).then(|| kept.get(text)).flatten()
    }

    fn keep_lines(
        &mut self,
        at: LinesAt,
        font: &Arc<ChromeFont>,
        text: &str,
        lines: &[String],
        misses: Box<[(char, u32)]>,
    ) {
        self.make_room();
        let (held, kept) = self
            .lines
            .entry(at)
            .or_insert_with(|| (Arc::clone(font), aterm_hash::KeyedHashMap::default()));
        if !Arc::ptr_eq(held, font) {
            *held = Arc::clone(font);
            self.entries = self.entries.saturating_sub(kept.len());
            kept.clear();
        }
        if kept.insert(text.into(), (lines.to_vec(), misses)).is_none() {
            self.entries += 1;
        }
    }

    fn make_room(&mut self) {
        if self.entries >= MEASURE_MEMO_ENTRIES {
            *self = Self::default();
        }
    }
}

/// The rule a text's kept lines were laid by ([`kept_lines`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum LinesRule {
    /// A paragraph of Settings copy (`native_settings::copy_paragraph_lines`).
    Copy,
    /// A setting's name on its row's label lines (`native_settings::row_name_lines`).
    RowName,
    /// A setting's name and the state its last line holds, as the row's line reads them
    /// (`native_settings::row_name_lines_holding`).
    RowNameHolding,
}

type LinesAt = (LinesRule, u16, u32, u32);

/// The lines `rule` lays `text` on in `face` at `px` in a box `width` wide, memoised: `lay_out`
/// must measure only in `face` at `px`, and kept lines are laid out again once a char the face has
/// no glyph for ([`ui_misses`]) measures otherwise. Text with an East Asian char the face lacks is
/// never kept.
pub(crate) fn kept_lines(
    rule: LinesRule,
    face: TextFace,
    text: &str,
    px: f32,
    width: f32,
    lay_out: impl FnOnce() -> Vec<String>,
) -> Vec<String> {
    use std::ops::ControlFlow::{Break, Continue};
    let at = (rule, face_code(face), px.to_bits(), width.to_bits());
    let font = match with_fonts(|fonts| {
        let Some(font) = fonts.ui_font(face).cloned() else {
            return Continue(None);
        };
        match fonts.measures.lines(at, &font, text) {
            Some((lines, misses)) if misses.is_empty() => return Break(lines.clone()),
            Some((lines, misses)) => {
                let (lines, misses) = (lines.clone(), misses.clone());
                if misses.iter().all(|&(character, advance)| {
                    fonts.ui_miss_advance(face, character, px).to_bits() == advance
                }) {
                    return Break(lines);
                }
            }
            None => {}
        }
        Continue(Some(font))
    }) {
        Break(lines) => return lines,
        Continue(None) => return lay_out(),
        Continue(Some(font)) => font,
    };
    let lines = lay_out();
    with_fonts(|fonts| {
        if let Some(misses) = ui_misses(fonts, &font, face, text, px) {
            fonts.measures.keep_lines(at, &font, text, &lines, misses);
        }
    });
    lines
}

/// The chars of `text` `font` has no glyph for, each with the advance a UI run in `face` gives it
/// at `px` ([`ChromeFonts::ui_miss_advance`]); `None` where one is East Asian, whose face a run
/// picks by its weight, so a layout measuring in more than `face` could charge it otherwise.
fn ui_misses(
    fonts: &mut ChromeFonts,
    font: &ChromeFont,
    face: TextFace,
    text: &str,
    px: f32,
) -> Option<Box<[(char, u32)]>> {
    let mut misses: Vec<(char, u32)> = Vec::new();
    for character in text.chars() {
        if font.maps(character) || misses.iter().any(|(miss, _)| *miss == character) {
            continue;
        }
        if ui_cjk_script(character).is_some() {
            return None;
        }
        misses.push((
            character,
            fonts.ui_miss_advance(face, character, px).to_bits(),
        ));
    }
    Some(misses.into_boxed_slice())
}

#[cfg(test)]
thread_local! {
    static MEASURE_WALKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static WHOLE_WALKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The strings this thread has measured glyph by glyph so far (test-only).
#[cfg(test)]
pub(crate) fn measure_walks() -> u64 {
    MEASURE_WALKS.with(std::cell::Cell::get)
}

/// Of [`measure_walks`], the strings the UI face mapped every char of, which [`MeasureMemo`] can
/// keep (test-only).
#[cfg(test)]
pub(crate) fn measure_walks_whole() -> u64 {
    WHOLE_WALKS.with(std::cell::Cell::get)
}

fn note_measure_walk() {
    #[cfg(test)]
    MEASURE_WALKS.with(|walks| walks.set(walks.get() + 1));
}

fn note_whole_walk() {
    #[cfg(test)]
    WHOLE_WALKS.with(|walks| walks.set(walks.get() + 1));
}

#[cfg(test)]
thread_local! {
    static GLYPH_RASTERS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The glyphs this thread has rasterized so far, not found kept (test-only): a raster's glyph
/// work is the difference across it.
#[cfg(test)]
pub(crate) fn glyph_rasters() -> u64 {
    GLYPH_RASTERS.with(std::cell::Cell::get)
}

fn note_glyph_raster() {
    #[cfg(test)]
    GLYPH_RASTERS.with(|rasters| rasters.set(rasters.get() + 1));
}

#[cfg(test)]
thread_local! {
    static GLYPH_BLITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The glyphs this thread has blended texel by texel so far (test-only): a raster's text work
/// is the difference across it. A glyph whose box misses the canvas is not blended.
#[cfg(test)]
pub(crate) fn glyph_blits() -> u64 {
    GLYPH_BLITS.with(std::cell::Cell::get)
}

fn note_glyph_blit() {
    #[cfg(test)]
    GLYPH_BLITS.with(|blits| blits.set(blits.get() + 1));
}

fn default_chrome_fonts() -> ChromeFonts {
    ChromeFonts {
        primary: None,
        bold: None,
        fallback: ChromeFace::from_bytes(aterm_render::embedded_font(), 0),
        coverage: Vec::new(),
        ui_cjk: None,
        ui_cjk_held: [Vec::new(), Vec::new()],
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
        semantic_displaced: None,
        semantic_ready_epoch: 0,
        semantic_prewarm_ms: None,
        measures: MeasureMemo::default(),
        glyphs: GlyphMemo::default(),
        text_holes: 0,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SemanticFontSnapshot {
    pub(crate) status: String,
    pub(crate) ready_epoch: u64,
    pub(crate) pending: bool,
}

/// One host-prepared, immutable semantic renderer source captured for a single view compilation.
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
    pub(crate) const fn is_ready(&self) -> bool {
        self.renderer.is_some()
    }

    /// The cell height a specimen paints at `px`: pure geometry from the prepared face, with no
    /// request, worker poll, font lock, fork or raster.
    pub(crate) fn specimen_cell_height(
        &self,
        px: f32,
        line_height: f32,
        variations: &[(u32, f32)],
    ) -> Option<usize> {
        Some(
            self.renderer
                .as_ref()?
                .specimen_cell_height(px, line_height, variations),
        )
    }

    /// The `(width, height)` cell a specimen paints at `px`, as pure geometry.
    pub(crate) fn specimen_cell_size(
        &self,
        px: f32,
        line_height: f32,
        variations: &[(u32, f32)],
    ) -> Option<(usize, usize)> {
        Some(
            self.renderer
                .as_ref()?
                .specimen_cell_size(px, line_height, variations),
        )
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

#[cfg(not(test))]
fn with_fonts<R>(visit: impl FnOnce(&mut ChromeFonts) -> R) -> R {
    visit(
        &mut chrome_fonts()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
}

#[cfg(test)]
thread_local! {
    static FONT_STORE_VISITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The font-store entries this thread has made so far (test-only): a render's text work is the
/// difference across it.
#[cfg(test)]
pub(crate) fn font_store_visits() -> u64 {
    FONT_STORE_VISITS.with(std::cell::Cell::get)
}

#[cfg(test)]
thread_local! {
    static LINEAR_COMPOSITES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The linear-light composites this thread has run so far (test-only): a raster's blend work is
/// the difference across it.
#[cfg(test)]
pub(crate) fn linear_composites() -> u64 {
    LINEAR_COMPOSITES.with(std::cell::Cell::get)
}

#[cfg(test)]
thread_local! {
    static STROKE_VISITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The pixels this thread's rounded strokes have weighed by their distance field so far
/// (test-only): a stroke's cost is the difference across it.
#[cfg(test)]
fn stroke_visits() -> u64 {
    STROKE_VISITS.with(std::cell::Cell::get)
}

#[cfg(test)]
// A visit cannot return a borrow of this thread's store. Owning it here lets thread exit
// release its faces, glyphs and semantic worker instead of retaining them for the process.
fn with_fonts<R>(visit: impl FnOnce(&mut ChromeFonts) -> R) -> R {
    thread_local! {
        static FONTS: Mutex<ChromeFonts> = Mutex::new(default_chrome_fonts());
    }
    FONT_STORE_VISITS.with(|visits| visits.set(visits.get() + 1));
    FONTS.with(|fonts| {
        visit(
            &mut fonts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    })
}

/// Install the renderer-resolved chrome faces, `(bytes, collection index)` of the primary terminal
/// face and its real bold sibling (`Renderer::chrome_primary_face`, `chrome_bold_face`), so tray
/// text renders in the user's font.
pub(crate) fn set_chrome_fonts(
    primary: Option<(std::sync::Arc<[u8]>, u32)>,
    bold: Option<(std::sync::Arc<[u8]>, u32)>,
    semantic: Option<Renderer>,
) {
    let (p, b) = (chrome_face_for_slot(&primary), chrome_face_for_slot(&bold));
    with_fonts(|fonts| reload_chrome_fonts_locked(fonts, p, b, semantic));
}

fn chrome_face_for_slot(slot: &Option<(std::sync::Arc<[u8]>, u32)>) -> Option<ChromeFace> {
    slot.as_ref()
        .and_then(|(bytes, index)| ChromeFace::from_bytes(bytes, *index))
}

/// Fully parsed chrome/semantic font generation.
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
    PreparedChromeFonts {
        primary: chrome_face_for_slot(&primary),
        bold: chrome_face_for_slot(&bold),
        semantic,
    }
}

pub(crate) fn set_prepared_chrome_fonts(prepared: PreparedChromeFonts) {
    with_fonts(|fonts| {
        reload_chrome_fonts_locked(fonts, prepared.primary, prepared.bold, prepared.semantic)
    });
}

fn reload_chrome_fonts_locked(
    fonts: &mut ChromeFonts,
    primary: Option<ChromeFace>,
    bold: Option<ChromeFace>,
    semantic: Option<Renderer>,
) {
    install_chrome_faces_locked(fonts, primary, bold, semantic);
    if fonts.semantic_worker.is_none() {
        fonts.semantic_worker = SemanticPrewarmWorker::spawn();
    }
    if let Some(worker) = fonts.semantic_worker.as_ref() {
        worker.cancel_queued();
    }
    #[cfg(not(test))]
    if let Some(warm) = chrome_coverage_warm(fonts) {
        let _ = crate::qos::spawn_lane_thread(
            "aterm-chrome-coverage-warm",
            crate::qos::Role::Background,
            move || {
                land_chrome_coverage(warm, || {
                    if let Some(wake) = SEMANTIC_READY_WAKE.get() {
                        wake();
                    }
                })
            },
        );
    }
}

/// Run the coverage `warm`, and `wake` the event loop when it parsed a face: what a native
/// view drew before that face could ink was drawn with holes where it would draw, and the
/// view draws again ([`chrome_coverage_epoch`]).
fn land_chrome_coverage(warm: impl FnOnce() -> usize, wake: impl FnOnce()) -> usize {
    let landed = warm();
    if landed > 0 {
        wake();
    }
    landed
}

/// WHICH FACES CHROME TEXT CAN INK A GLYPH FROM NOW: the UI faces, their parsed East Asian
/// faces and the terminal chain's parsed faces ([`ChromeFonts::face_for`]). Chrome text
/// draws only parsed faces, so a native view's raster is stamped with this
/// (`App::native_ui_compile_stamp_for`): one drawn before a face it needed had parsed (a
/// first open's Japanese, parsed by the coverage warm or a terminal's fallback) is drawn
/// again once it has, not left blank until something else redraws.
pub(crate) fn chrome_coverage_epoch() -> u64 {
    use std::hash::Hasher;

    with_fonts(|fonts| coverage_epoch_locked(fonts).finish())
}

/// EVERY FACE CHROME TEXT MEASURES IN: [`chrome_coverage_epoch`]'s, and the terminal faces
/// installed as chrome's primary and bold ([`set_chrome_fonts`]), by install, which measure a
/// char the UI face lacks before any other ([`ChromeFonts::face_for`]).
pub(crate) fn chrome_measure_epoch() -> u64 {
    use std::hash::{Hash, Hasher};

    with_fonts(|fonts| {
        let mut epoch = coverage_epoch_locked(fonts);
        fonts.semantic_generation.hash(&mut epoch);
        epoch.finish()
    })
}

fn coverage_epoch_locked(fonts: &mut ChromeFonts) -> std::collections::hash_map::DefaultHasher {
    use std::hash::Hash;

    fonts.sync_coverage();
    let mut epoch = std::collections::hash_map::DefaultHasher::new();
    fonts
        .ui_cjk
        .as_ref()
        .map_or(0, |store| store.parsed_count())
        .hash(&mut epoch);
    for face in &fonts.coverage {
        Arc::as_ptr(&face.font).hash(&mut epoch);
    }
    fonts.ui_regular.as_ref().map(Arc::as_ptr).hash(&mut epoch);
    fonts.ui_semibold.as_ref().map(Arc::as_ptr).hash(&mut epoch);
    epoch
}

/// How many chars chrome text has measured or drawn with no face of the chrome stack (a
/// synthesized mark's or a hole's advance): the chars a face landing later would draw otherwise.
/// A native raster that added none while it compiled and painted is stamped with the faces as
/// they stand once it painted (`retain_native_leaf_raster`), so a face its own previews parsed
/// mid-paint does not draw it again.
pub(crate) fn chrome_text_holes() -> u64 {
    with_fonts(|fonts| fonts.text_holes)
}

fn chrome_coverage_warm(
    fonts: &ChromeFonts,
) -> Option<impl FnOnce() -> usize + Send + 'static + use<>> {
    let chain = fonts.semantic.as_ref().map(Renderer::fallback_coverage);
    let ui = fonts.ui_cjk.clone();
    (chain.is_some() || ui.is_some())
        .then_some(move || warm_chrome_coverage(chain.as_ref(), ui.as_deref()))
}

const CHROME_COVERAGE_WARM: &str = "日の한";

fn warm_chrome_coverage(
    chain: Option<&aterm_render::FallbackCoverage>,
    ui: Option<&UiCjkFaces>,
) -> usize {
    let mut landed = 0;
    if let Some(ui) = ui {
        for weight in [UiCjkWeight::Regular, UiCjkWeight::Semibold] {
            landed += ui.materialise_for(weight, CHROME_COVERAGE_WARM).0;
        }
    }
    if let Some(chain) = chain {
        landed += chain.materialise_for(CHROME_COVERAGE_WARM);
    }
    landed
}

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
    fonts.set_ui_cjk(ui_cjk_store_for_install());
    fonts.semantic_generation = fonts.semantic_generation.wrapping_add(1);
    fonts.semantic = semantic;
    fonts.coverage.clear();
    fonts.semantic_identity = fonts
        .semantic
        .as_ref()
        .map(|_| crate::widget::SemanticFontCandidate::default());
    fonts.semantic_resolution = SemanticFontResolution::Host;
    fonts.semantic_requested = crate::widget::SemanticFontCandidate::default();
    fonts.semantic_pending = None;
    fonts.semantic_worker_generation = None;
    fonts.semantic_cache.clear();
    fonts.semantic_displaced = None;
    fonts.semantic_ready_epoch = fonts.semantic_ready_epoch.wrapping_add(1);
    fonts.semantic_prewarm_ms = None;
    fonts.glyphs = GlyphMemo::default();
}

/// Drop the glyphs rasters keep for the next ([`GlyphMemo`]), as a window's scale change and a
/// theme change do. Neither could make a kept glyph draw stale, as its key names its size in device
/// px and its bitmap is coverage alone: the drop frees what the old scale drew, and starts the
/// memo again with the chrome a new theme redraws.
pub(crate) fn forget_kept_glyphs() {
    with_fonts(|fonts| fonts.glyphs = GlyphMemo::default());
}

/// How many glyphs rasters keep for the next on this test thread ([`GlyphMemo`]).
#[cfg(test)]
pub(crate) fn kept_glyphs() -> usize {
    with_fonts(|fonts| fonts.glyphs.len())
}

/// Test-only: turn every kept glyph's coverage over, the stale glyph a test of the memo must see.
#[cfg(test)]
pub(crate) fn spoil_kept_glyphs_for_test() {
    with_fonts(|fonts| {
        for kept in fonts.glyphs.glyphs.values_mut() {
            kept.bitmap = kept.bitmap.iter().map(|coverage| !coverage).collect();
        }
    });
}

/// Test-only: install `renderer` as this test thread's settled semantic chrome font.
#[cfg(test)]
pub(crate) fn install_settled_chrome_fonts_for_test(mut renderer: Renderer) -> u64 {
    renderer.prepare_semantic_typography(
        "Input→present λ π √ ✓ 你好世界 日本語 한글 🚀 😀 🐈\u{200d}⬛ e\u{301}",
    );
    with_fonts(|fonts| {
        install_chrome_faces_locked(fonts, None, None, Some(renderer));
        fonts.semantic_prewarm_ms = Some(0);
        fonts.semantic_worker = None;
        fonts.semantic_ready_epoch
    })
}

/// Drive the real request/result channel without a racing worker.
#[cfg(test)]
pub(crate) fn install_pending_chrome_fonts_for_test(
    renderer: Renderer,
) -> PendingChromeFontsForTest {
    let queue = Arc::new(SemanticPrewarmQueue::new());
    let (tx, results) = std::sync::mpsc::channel();
    with_fonts(|fonts| {
        install_chrome_faces_locked(fonts, None, None, Some(renderer));
        fonts.semantic_worker = Some(SemanticPrewarmWorker {
            queue: queue.clone(),
            results,
        });
        PendingChromeFontsForTest { queue, tx }
    })
}

#[cfg(test)]
pub(crate) struct PendingChromeFontsForTest {
    queue: Arc<SemanticPrewarmQueue>,
    tx: std::sync::mpsc::Sender<SemanticPrewarmResult>,
}

#[cfg(test)]
impl PendingChromeFontsForTest {
    pub(crate) fn complete(&self) {
        let job = self
            .queue
            .semantic_job
            .lock()
            .unwrap()
            .job
            .take()
            .expect("a real font request");
        assert!(
            job.candidate.is_host(),
            "fixture is for committed host fonts"
        );
        let mut base = job.base.expect("first request carries the committed seed");
        base.prepare_semantic_typography(
            "Input→present λ π √ ✓ 你好世界 日本語 한글 🚀 😀 🐈‍⬛ e\u{301}",
        );
        let (renderer, resolution) =
            build_semantic_candidate(&base, &job.candidate, &mut CandidateFaceCache::new());
        self.tx
            .send(SemanticPrewarmResult {
                generation: job.generation,
                request: job.request,
                candidate: job.candidate,
                renderer,
                resolution,
                elapsed_ms: 0,
            })
            .unwrap_or_else(|_| panic!("the font context still owns its result channel"));
    }
}

#[cfg(test)]
impl Drop for PendingChromeFontsForTest {
    fn drop(&mut self) {
        with_fonts(|fonts| {
            if fonts
                .semantic_worker
                .as_ref()
                .is_some_and(|worker| Arc::ptr_eq(&worker.queue, &self.queue))
            {
                fonts.semantic_worker = None;
                fonts.semantic_pending = None;
            }
        });
    }
}

/// Like [`install_settled_chrome_fonts_for_test`], but installs `renderer` as [`set_chrome_fonts`]
/// does (dormant, no warmup) and parks no worker, so nothing lands through this store later.
#[cfg(test)]
pub(crate) fn install_dormant_chrome_fonts_for_test(renderer: Renderer) -> u64 {
    with_fonts(|fonts| {
        install_chrome_faces_locked(fonts, None, None, Some(renderer));
        fonts.semantic_worker = None;
        fonts.semantic_ready_epoch
    })
}

/// The broad CJK face the host's built-in fallback chain leads with, where it is installed, so a
/// test draws with a real ideograph-bearing face.
#[cfg(test)]
pub(crate) fn cjk_chain_face_path_for_test() -> Option<&'static str> {
    const CANDIDATES: &[&str] = &[
        #[cfg(target_os = "macos")]
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        #[cfg(target_os = "macos")]
        "/System/Library/Fonts/STHeiti Light.ttc",
        #[cfg(target_os = "macos")]
        "/System/Library/Fonts/Supplemental/Songti.ttc",
        #[cfg(target_os = "linux")]
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        #[cfg(windows)]
        "C:\\Windows\\Fonts\\msyh.ttc",
        #[cfg(windows)]
        "C:\\Windows\\Fonts\\YuGothR.ttc",
        #[cfg(windows)]
        "C:\\Windows\\Fonts\\msgothic.ttc",
    ];
    CANDIDATES
        .iter()
        .copied()
        .find(|path| std::path::Path::new(path).is_file())
}

/// Install a settled semantic chrome font whose chain holds the host's CJK face already parsed
/// (the store's state once the terminal has drawn an ideograph), plus the host UI faces.
#[cfg(test)]
pub(crate) fn install_chrome_fonts_with_cjk_chain_for_test() -> bool {
    let Some(path) = cjk_chain_face_path_for_test() else {
        return false;
    };
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let mut renderer = Renderer::from_bytes(
        aterm_render::embedded_font(),
        14.0,
        aterm_render::Theme::default(),
    )
    .expect("embedded renderer");
    renderer.set_runtime_font_discovery(false);
    if renderer.add_fallback_bytes(&bytes).is_err() {
        return false;
    }
    install_settled_chrome_fonts_for_test(renderer);
    prepare_ui_fonts_for_direct_view_test();
    true
}

/// Install, dormant, a semantic seed whose chain holds the host's CJK face unparsed, plus the host
/// UI faces: a fresh window's state before anything drew an ideograph, built in the window's own
/// order (configure, seal, fork the seed). Returns the live renderer the seed shares its cell with.
#[cfg(test)]
pub(crate) fn install_unparsed_cjk_chain_for_test() -> Option<Renderer> {
    let path = cjk_chain_face_path_for_test()?;
    let theme = aterm_render::Theme::default();
    let mut live = Renderer::from_bytes(aterm_render::embedded_font(), 14.0, theme).ok()?;
    live.set_runtime_font_discovery(false);
    live.set_config_fallback_fonts(&[path.to_string()]);
    let _ = live.seal_admitted_font_sources();
    let seed = live.fork_semantic_surface(14.0, theme)?;
    install_dormant_chrome_fonts_for_test(seed);
    prepare_ui_fonts_for_direct_view_test();
    Some(live)
}

/// Parse what the startup warm parses in `live`'s chain on this test thread, as the warm
/// thread does; how many faces landed.
#[cfg(test)]
pub(crate) fn warm_chrome_coverage_for_test(live: &Renderer) -> usize {
    warm_chrome_coverage(Some(&live.fallback_coverage()), None)
}

/// Whether this test thread's chrome cascade draws `ch` from a face it holds parsed (the terminal
/// stack and the coverage rung: [`ChromeFonts::face_for`]), without parsing anything.
#[cfg(test)]
pub(crate) fn chrome_cascade_reaches_for_test(ch: char) -> bool {
    with_fonts(|fonts| fonts.face_for(TextWeight::Regular, ch).is_some())
}

/// Whether this test thread's chrome draws `ch` in a run set in `face` from a face it holds parsed
/// (the UI faces' East Asian fallback at `face`'s weight, then the terminal cascade:
/// [`ChromeFonts::ui_miss_face`]), without parsing anything.
#[cfg(all(test, not(target_os = "macos")))]
pub(crate) fn chrome_ui_reaches_for_test(face: TextFace, ch: char) -> bool {
    with_fonts(|fonts| fonts.ui_miss_face(face, TextWeight::Regular, ch).is_some())
}

/// Test-only generation token for the current libtest worker's chrome-font context.
#[cfg(test)]
pub(crate) fn chrome_font_epoch_for_test() -> u64 {
    with_fonts(|fonts| fonts.semantic_ready_epoch)
}

/// For direct semantic-view tests, which build no renderer backend: install the proportional UI
/// faces backend startup would, leaving any installed semantic renderer in place.
#[cfg(test)]
pub(crate) fn prepare_ui_fonts_for_direct_view_test() {
    let ui = prepared_ui_font_assets();
    with_fonts(|fonts| {
        fonts.ui_regular = ui.regular.clone();
        fonts.ui_semibold = ui.semibold.clone();
        if fonts.ui_cjk.is_none() {
            fonts.set_ui_cjk(ui_cjk_store_for_install());
        }
    })
}

/// Replace this test thread's East Asian UI fallback ([`UiCjkFaces`]): `None` stands in for a host
/// with no such faces, for a test of the terminal chain's rung under it.
#[cfg(all(test, not(target_os = "macos")))]
pub(crate) fn set_ui_cjk_faces_for_test(store: Option<Arc<UiCjkFaces>>) {
    with_fonts(|fonts| fonts.set_ui_cjk(store));
}

/// A cold East Asian UI fallback store of this host's faces, nothing parsed, or `None` when the
/// host has none.
#[cfg(all(test, windows))]
pub(crate) fn host_ui_cjk_faces_for_test() -> Option<Arc<UiCjkFaces>> {
    UiCjkFaces::host().map(Arc::new)
}

/// Stand `face` in for this test thread's UI faces, regular and semibold, until the next install
/// ([`set_chrome_fonts`]): a test that holds it measures in the same faces whatever the host's.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn set_ui_face_for_test(face: &Arc<ChromeFont>) {
    with_fonts(|fonts| {
        fonts.ui_regular = Some(Arc::clone(face));
        fonts.ui_semibold = Some(Arc::clone(face));
    })
}

/// The inverse seam: return this test thread's chrome stack to the cold, UI-face-less store.
#[cfg(test)]
pub(crate) fn clear_ui_fonts_for_test() {
    with_fonts(|fonts| {
        fonts.ui_regular = None;
        fonts.ui_semibold = None;
        fonts.set_ui_cjk(None);
    })
}

/// An owned, immutable semantic renderer input for deterministic direct-view tests.
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

/// A host-prepared semantic font over `renderer` (a test's own face, such as a system one:
/// macOS's, the only host a test reads one on).
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn prepared_semantic_font_from_renderer_for_test(
    renderer: Renderer,
) -> PreparedSemanticFont {
    PreparedSemanticFont {
        candidate: crate::widget::SemanticFontCandidate::default(),
        snapshot: SemanticFontSnapshot {
            status: "host-prepared test renderer ready".to_string(),
            ready_epoch: 1,
            pending: false,
        },
        renderer: Some(Rc::new(renderer)),
    }
}

/// Which face of the chrome stack draws a glyph: the pure policy (no font I/O).
pub(crate) const fn select_chrome_face(
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

fn display_chrome_font(index: u8) -> Option<&'static Arc<ChromeFont>> {
    static FACES: OnceLock<Vec<Option<Arc<ChromeFont>>>> = OnceLock::new();
    FACES
        .get_or_init(|| {
            aterm_render::DISPLAY_FACES
                .iter()
                .map(|face| {
                    let bytes = aterm_render::display_face_bytes(face.id)?;
                    aterm_render::shared_parsed_face(bytes, 0)
                        .ok()
                        .map(|(_, font)| font)
                })
                .collect()
        })
        .get(usize::from(index))?
        .as_ref()
}

impl ChromeFonts {
    fn ui_font(&self, face: TextFace) -> Option<&Arc<ChromeFont>> {
        match face {
            TextFace::UiBold => self.ui_semibold.as_ref().or(self.ui_regular.as_ref()),
            TextFace::Display(index) => display_chrome_font(index),
            TextFace::Mono | TextFace::Ui => self.ui_regular.as_ref(),
        }
    }

    fn face_for(&mut self, weight: TextWeight, ch: char) -> Option<&mut ChromeFace> {
        let bold_run = weight == TextWeight::Bold;
        let bold_has = self.bold.as_ref().is_some_and(|f| f.has(ch));
        let primary_has = self.primary.as_ref().is_some_and(|f| f.has(ch));
        match select_chrome_face(bold_run, bold_has, primary_has) {
            ChromeFacePick::Bold => self.bold.as_mut(),
            ChromeFacePick::Primary => self.primary.as_mut(),
            ChromeFacePick::Fallback => {
                if self.fallback.as_ref().is_some_and(|face| face.has(ch)) {
                    return self.fallback.as_mut();
                }
                self.sync_coverage();
                self.coverage.iter_mut().find(|face| face.has(ch))
            }
        }
    }

    fn sync_coverage(&mut self) {
        let Some(semantic) = self.semantic.as_ref() else {
            self.coverage.clear();
            return;
        };
        let unchanged = semantic.ready_fallback_faces().count() == self.coverage.len()
            && semantic
                .ready_fallback_faces()
                .zip(&self.coverage)
                .all(|(ready, held)| Arc::ptr_eq(ready, &held.font));
        if unchanged {
            return;
        }
        let faces: Vec<Arc<ChromeFont>> = semantic.ready_fallback_faces().cloned().collect();
        self.coverage = faces.into_iter().map(ChromeFace::from_font).collect();
    }

    fn set_ui_cjk(&mut self, store: Option<Arc<UiCjkFaces>>) {
        let same = match (&self.ui_cjk, &store) {
            (Some(held), Some(new)) => Arc::ptr_eq(held, new),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.ui_cjk_held = [Vec::new(), Vec::new()];
        }
        self.ui_cjk = store;
    }

    fn sync_ui_cjk(&mut self, weight: UiCjkWeight) {
        let held = &mut self.ui_cjk_held[weight.slot()];
        let Some(store) = self.ui_cjk.as_ref() else {
            held.clear();
            return;
        };
        let rung = store.rung(weight);
        if held.len() != rung.len() {
            *held = rung.iter().map(|_| None).collect();
        }
        for (slot, face) in held.iter_mut().zip(rung) {
            if slot.is_none()
                && let Some(font) = face.parsed()
            {
                *slot = Some(ChromeFace::from_font(Arc::clone(font)));
            }
        }
    }

    fn ui_cjk_index(&mut self, weight: UiCjkWeight, ch: char) -> Option<usize> {
        let script = ui_cjk_script(ch)?;
        self.sync_ui_cjk(weight);
        let store = self.ui_cjk.as_ref()?;
        store
            .rung(weight)
            .iter()
            .zip(&self.ui_cjk_held[weight.slot()])
            .position(|(face, held)| {
                face.script == script && held.as_ref().is_some_and(|held| held.has(ch))
            })
    }

    fn ui_miss_face(
        &mut self,
        face: TextFace,
        weight: TextWeight,
        ch: char,
    ) -> Option<&mut ChromeFace> {
        let rung = UiCjkWeight::of(face);
        if let Some(entry) = self.ui_cjk_index(rung, ch) {
            return self.ui_cjk_held[rung.slot()][entry].as_mut();
        }
        self.face_for(weight, ch)
    }

    /// The advance a UI run in `face` gives `ch`, which the UI face has no glyph for, at `px`: its
    /// [`Self::ui_miss_face`]'s, else a synthesized mark's, else 0.6 em.
    fn ui_miss_advance(&mut self, face: TextFace, ch: char, px: f32) -> f32 {
        if let Some(cface) = self.ui_miss_face(face, TextWeight::Regular, ch) {
            return cface.font.char_metrics(ch, px).advance_width;
        }
        self.note_text_hole();
        synthesized::advance(ch, px).unwrap_or(px * 0.6)
    }

    /// Count a char chrome text measured or drew with no face of this stack.
    const fn note_text_hole(&mut self) {
        self.text_holes = self.text_holes.wrapping_add(1);
    }

    fn label_misses(&mut self, face: TextFace, s: &str) -> Vec<char> {
        let ui = self.ui_font(face).cloned();
        let weight = UiCjkWeight::of(face);
        let mut misses = Vec::new();
        for ch in s.chars() {
            let unsettled = self
                .ui_cjk
                .as_ref()
                .is_some_and(|store| store.settled(weight, ch).is_none());
            let drawn = ch == ' '
                || ui.as_deref().is_some_and(|font| font.maps(ch))
                || (!unsettled && self.ui_miss_face(face, TextWeight::Regular, ch).is_some())
                || (!unsettled && synthesized::covers(ch));
            if !drawn && !misses.contains(&ch) {
                misses.push(ch);
            }
        }
        misses
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreviewFontConvergence",
            action = "Finish",
            project = "aterm_gui::app_render::native_damage_tests::project_native_preview_font_convergence"
        )
    )]
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
            && let Some(evicted) = self
                .semantic_cache
                .keys()
                .find(|key| self.semantic_displaced.as_ref() != Some(*key))
                .cloned()
        {
            self.semantic_cache.remove(&evicted);
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
        self.semantic_ready_epoch = self.semantic_ready_epoch.wrapping_add(1);
    }

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
            self.semantic_displaced = self.semantic_identity.clone();
            self.cache_active_semantic();
        }
        let job = SemanticPrewarmJob {
            generation: self.semantic_generation,
            request: self.semantic_request,
            candidate: candidate.clone(),
            base,
        };
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

    fn semantic_renderer_fork_for(
        &self,
        candidate: &crate::widget::SemanticFontCandidate,
        px: f32,
        theme: aterm_render::Theme,
    ) -> Option<Renderer> {
        let exact = self.semantic_identity.as_ref() == Some(candidate);
        let committed_fallback = self
            .semantic_identity
            .as_ref()
            .is_some_and(crate::widget::SemanticFontCandidate::is_host);
        if let Some(renderer) = self
            .semantic
            .as_ref()
            .filter(|_| exact || committed_fallback)
        {
            return renderer.fork_semantic_surface(px, theme);
        }
        self.semantic_displaced
            .as_ref()
            .and_then(|identity| self.semantic_cache.get(identity))
            .or_else(|| {
                self.semantic_cache
                    .iter()
                    .find(|(identity, _)| identity.is_host())
                    .map(|(_, entry)| entry)
            })
            .or_else(|| self.semantic_cache.values().next())
            .and_then(|(renderer, _, _)| renderer.fork_semantic_surface(px, theme))
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

/// Observe a conformance fixture without requesting or polling a font job.
#[cfg(test)]
pub(crate) fn semantic_font_snapshot_for_test(
    candidate: &crate::widget::SemanticFontCandidate,
) -> SemanticFontSnapshot {
    with_fonts(|fonts| semantic_font_snapshot_locked(fonts, candidate))
}

type SemanticForkMemo = (
    crate::widget::SemanticFontCandidate,
    u64,
    Option<Rc<Renderer>>,
);

thread_local! {
    static SEMANTIC_FORK_MEMO: std::cell::RefCell<Option<SemanticForkMemo>> =
        const { std::cell::RefCell::new(None) };
}

/// Host-only request/poll/capture seam.
pub(crate) fn prepare_semantic_font(
    candidate: &crate::widget::SemanticFontCandidate,
) -> PreparedSemanticFont {
    let (snapshot, renderer, remember) = with_fonts(|fonts| {
        let _ = fonts.ensure_semantic_candidate(candidate);
        let snapshot = semantic_font_snapshot_locked(fonts, candidate);
        let memoized = SEMANTIC_FORK_MEMO.with(|slot| {
            slot.borrow()
                .as_ref()
                .and_then(|(memo_candidate, memo_epoch, renderer)| {
                    (memo_candidate == candidate && *memo_epoch == snapshot.ready_epoch)
                        .then(|| renderer.clone())
                })
        });
        let (renderer, remember) = match memoized {
            Some(renderer) => (renderer, false),
            None => {
                let renderer = fonts
                    .semantic_renderer_fork_for(candidate, 14.0, aterm_render::Theme::default())
                    .map(Rc::new);
                (renderer, true)
            }
        };
        (snapshot, renderer, remember)
    });
    if remember {
        let memo = renderer.clone();
        SEMANTIC_FORK_MEMO.with(|slot| {
            *slot.borrow_mut() = Some((candidate.clone(), snapshot.ready_epoch, memo));
        });
    }
    PreparedSemanticFont {
        candidate: candidate.clone(),
        snapshot,
        renderer,
    }
}

/// The width of `s` at `px` in the chrome stack: real advances with the raster pen's per-char face
/// pick and fixed-point accumulation.
pub(crate) fn measure_text(s: &str, px: f32, weight: TextWeight) -> f32 {
    with_fonts(|fonts| {
        let mut pen_q: i64 = 0;
        for ch in s.chars() {
            if let Some(face) = fonts.face_for(weight, ch) {
                pen_q += px_to_q(face.advance_em(ch) * px);
                continue;
            }
            fonts.note_text_hole();
            if let Some(advance) = synthesized::advance(ch, px) {
                pen_q += px_to_q(advance);
            }
        }
        q_to_px(pen_q)
    })
}

pub(crate) fn chrome_cap_ratio() -> f32 {
    with_fonts(|fonts| {
        fonts
            .primary
            .as_ref()
            .or(fonts.fallback.as_ref())
            .map_or(0.7, |f| f.cap_ratio)
    })
}

/// The cap-height-centred baseline for a `size`-px run in the row `[y0, y0 + row_h)`
/// (`aterm_render::chrome_metrics::baseline_in_row_q`): top and bottom gaps balance within 1px
/// for any `cap <= row_h`.
pub(crate) fn row_baseline(y0: f32, row_h: f32, size: f32) -> f32 {
    let cap_q = px_to_q(size * chrome_cap_ratio());
    q_to_px(baseline_in_row_q(px_to_q(y0), px_to_q(row_h), cap_q))
}

/// Conservative vertical ink bounds for the faces and fallbacks [`Canvas::text`] paints.
/// One logical point either side covers device-pixel rounding at native scales of 1 or more.
/// An unmeasured fallback keeps the caller from claiming complete ink geometry.
pub(crate) fn text_ink_band(
    s: &str,
    px: f32,
    weight: TextWeight,
    face: TextFace,
    baseline: f32,
) -> Option<(f32, f32)> {
    with_fonts(|fonts| {
        let ui = fonts.ui_font(face).cloned();
        let baseline = q_round_to_px(px_to_q(baseline)) as f32;
        let mut bounds: Option<(f32, f32)> = None;
        for ch in s.chars() {
            let metrics = if face == TextFace::Mono {
                fonts
                    .face_for(weight, ch)
                    .map(|font| font.font.char_metrics(ch, px))
            } else if let Some(font) = ui.as_ref().filter(|font| font.maps(ch)) {
                Some(font.char_metrics(ch, px))
            } else {
                fonts
                    .ui_miss_face(face, weight, ch)
                    .map(|font| font.font.char_metrics(ch, px))
            }
            .or_else(|| synthesized::metrics(ch, px))?;
            if metrics.height == 0 || metrics.width == 0 {
                continue;
            }
            let top = baseline - metrics.height as f32 - metrics.ymin as f32 - 1.0;
            let bottom = baseline - metrics.ymin as f32 + 1.0;
            bounds = Some(bounds.map_or((top, bottom), |(a, b)| (a.min(top), b.max(bottom))));
        }
        bounds
    })
}

#[derive(Clone, Default)]
struct UiFontAssets {
    regular: Option<Arc<ChromeFont>>,
    semibold: Option<Arc<ChromeFont>>,
    #[cfg(not(target_os = "macos"))]
    variable_semibold: Option<UiVariableSemibold>,
}

/// The variable UI face instanced at semibold, for the one painter that instances a variation
/// itself (the strip band's active label, [`crate::tab_bar::pixel_band`]).
#[derive(Clone)]
#[cfg(not(target_os = "macos"))]
pub(crate) struct UiVariableSemibold {
    pub(crate) bytes: aterm_render::font::FaceBytes,
    pub(crate) index: u32,
    pub(crate) coords: Vec<(u32, f32)>,
    pub(crate) cmap: Arc<ChromeFont>,
}

#[cfg(not(target_os = "macos"))]
const UI_SEMIBOLD_WGHT: f32 = 600.0;

#[cfg(not(target_os = "macos"))]
fn variable_semibold_of(
    bytes: aterm_render::font::FaceBytes,
    index: u32,
    regular: &Arc<ChromeFont>,
) -> Option<UiVariableSemibold> {
    use aterm_render::variation::{REGULAR_WGHT, WGHT_TAG, clamp_axis, probe};
    let probe = probe(&bytes, index)?;
    let wght = probe.axes.iter().find(|axis| axis.tag == WGHT_TAG)?;
    let weight = clamp_axis(wght, UI_SEMIBOLD_WGHT);
    if weight < REGULAR_WGHT + 100.0 {
        return None;
    }
    Some(UiVariableSemibold {
        bytes,
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
    vec![UiFontCandidate {
        regular_path: "/System/Library/Fonts/HelveticaNeue.ttc".into(),
        regular_index: 0,
        semibold_path: "/System/Library/Fonts/HelveticaNeue.ttc".into(),
        semibold_index: 10,
    }]
}

#[cfg(windows)]
fn ui_font_candidates() -> Vec<UiFontCandidate> {
    let root = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
        .join("Fonts");
    vec![
        UiFontCandidate {
            regular_path: root.join("SegUIVar.ttf"),
            regular_index: 0,
            semibold_path: root.join("seguisb.ttf"),
            semibold_index: 0,
        },
        UiFontCandidate {
            regular_path: root.join("segoeui.ttf"),
            regular_index: 0,
            semibold_path: root.join("seguisb.ttf"),
            semibold_index: 0,
        },
    ]
}

#[cfg(any(target_os = "linux", test))]
const LINUX_UI_FONT_PAIRS: [(&str, &str); 6] = [
    (
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-SemiBold.ttf",
    ),
    (
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
    ),
    (
        "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/noto/NotoSans-Bold.ttf",
    ),
    (
        "/usr/share/fonts/opentype/cantarell/Cantarell-Regular.otf",
        "/usr/share/fonts/opentype/cantarell/Cantarell-Bold.otf",
    ),
    (
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    ),
    (
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
    ),
];

#[cfg(target_os = "linux")]
fn ui_font_candidates() -> Vec<UiFontCandidate> {
    LINUX_UI_FONT_PAIRS
        .into_iter()
        .map(|(regular, semibold)| UiFontCandidate {
            regular_path: regular.into(),
            regular_index: 0,
            semibold_path: semibold.into(),
            semibold_index: 0,
        })
        .collect()
}

/// Install, as this test thread's UI faces, the first pair of the Linux ladder
/// ([`LINUX_UI_FONT_PAIRS`]) whose files both parse, so a test can judge a layout's fit with Linux
/// metrics on any host.
#[cfg(test)]
pub(crate) fn install_linux_ui_fonts_for_test(
    root: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    let locate = |path: &str| -> std::path::PathBuf {
        let path = std::path::Path::new(path);
        match (root, path.file_name()) {
            (Some(root), Some(name)) => root.join(name),
            _ => path.to_path_buf(),
        }
    };
    let mut files = UiFontFiles::default();
    for (regular, semibold) in LINUX_UI_FONT_PAIRS {
        let regular = locate(regular);
        let (Some((regular_face, _)), Some((semibold_face, _))) = (
            parse_ui_font(&mut files, &regular, 0),
            parse_ui_font(&mut files, &locate(semibold), 0),
        ) else {
            continue;
        };
        with_fonts(|fonts| {
            fonts.ui_regular = Some(regular_face);
            fonts.ui_semibold = Some(semibold_face);
        });
        return Some(regular);
    }
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn ui_font_candidates() -> Vec<UiFontCandidate> {
    Vec::new()
}

#[derive(Default)]
struct UiFontFiles {
    admitted: std::collections::HashMap<std::path::PathBuf, Option<aterm_render::font::FaceBytes>>,
    reads: usize,
}

impl UiFontFiles {
    fn admit(&mut self, path: &std::path::Path) -> Option<aterm_render::font::FaceBytes> {
        if let Some(hit) = self.admitted.get(path) {
            return hit.clone();
        }
        self.reads += 1;
        let admitted = aterm_render::font_file::admit_font_file(path).ok();
        self.admitted.insert(path.to_path_buf(), admitted.clone());
        admitted
    }
}

fn parse_ui_font(
    files: &mut UiFontFiles,
    path: &std::path::Path,
    index: u32,
) -> Option<(Arc<ChromeFont>, aterm_render::font::FaceBytes)> {
    let bytes = files.admit(path)?;
    let font = ChromeFont::from_shared(
        bytes.clone(),
        aterm_render::font::FontSettings {
            collection_index: index,
        },
    )
    .ok()?;
    Some((Arc::new(font), bytes))
}

/// How many times the host resolved its UI faces ([`resolve_ui_font_assets`]): once, for
/// [`prepared_ui_font_assets`]. A test that resolves faces of its own goes past it
/// ([`resolve_ui_font_assets_with`]), so a test that reads it sees only the host's.
#[cfg(test)]
static UI_FONT_RESOLVE_ATTEMPTS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn resolve_ui_font_assets() -> UiFontAssets {
    #[cfg(test)]
    UI_FONT_RESOLVE_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut files = UiFontFiles::default();
    resolve_ui_font_assets_with(&mut files)
}

fn resolve_ui_font_assets_with(files: &mut UiFontFiles) -> UiFontAssets {
    let mut first_regular: Option<(Arc<ChromeFont>, aterm_render::font::FaceBytes, u32)> = None;
    for candidate in ui_font_candidates() {
        let regular = parse_ui_font(files, &candidate.regular_path, candidate.regular_index);
        let semibold = parse_ui_font(files, &candidate.semibold_path, candidate.semibold_index);
        match (regular, semibold) {
            (Some((regular, bytes)), Some((semibold, _))) => {
                return ui_font_assets(regular, Some(semibold), bytes, candidate.regular_index);
            }
            (Some(regular), None) if first_regular.is_none() => {
                first_regular = Some((regular.0, regular.1, candidate.regular_index));
            }
            _ => {}
        }
    }
    match first_regular {
        Some((regular, bytes, index)) => {
            // No static semibold anywhere: a variable regular can still instance its own, so
            // the band's active label keeps its weight where `UiBold` falls back to the regular.
            ui_font_assets(regular, None, bytes, index)
        }
        None => UiFontAssets::default(),
    }
}

#[cfg(not(target_os = "macos"))]
fn ui_font_assets(
    regular: Arc<ChromeFont>,
    semibold: Option<Arc<ChromeFont>>,
    bytes: aterm_render::font::FaceBytes,
    index: u32,
) -> UiFontAssets {
    UiFontAssets {
        variable_semibold: variable_semibold_of(bytes, index, &regular),
        regular: Some(regular),
        semibold,
    }
}

#[cfg(target_os = "macos")]
fn ui_font_assets(
    regular: Arc<ChromeFont>,
    semibold: Option<Arc<ChromeFont>>,
    _bytes: aterm_render::font::FaceBytes,
    _index: u32,
) -> UiFontAssets {
    UiFontAssets {
        regular: Some(regular),
        semibold,
    }
}

fn prepared_ui_font_assets() -> &'static UiFontAssets {
    static ASSETS: OnceLock<UiFontAssets> = OnceLock::new();
    ASSETS.get_or_init(resolve_ui_font_assets)
}

/// Populate the two process-global font `OnceLock`s [`set_chrome_fonts`] would otherwise fill on
/// whichever thread calls it first: the system UI face parse ([`prepared_ui_font_assets`]) and the
/// embedded default store ([`chrome_fonts`]).
#[cfg(not(test))]
pub(crate) fn warm_chrome_font_assets() {
    let _ = prepared_ui_font_assets();
    with_fonts(|_| ());
}

/// Tests keep chrome fonts per thread, so a process-global warm means nothing there.
#[cfg(test)]
pub(crate) fn warm_chrome_font_assets() {}

/// The pixel tab strip's font-readiness fingerprint (`crate::tab_bar::pixel_band`): the
/// chrome-face install epoch with the parsed East Asian UI faces, the coverage rung's size and
/// the UI regular's presence folded in.
pub(crate) fn strip_band_font_epoch() -> u64 {
    with_fonts(font_epoch_in)
}

/// The UI face `face` sets text in, then [`strip_band_font_epoch`], read under one lock.
pub(crate) fn ui_font_and_epoch(face: TextFace) -> (Option<Arc<ChromeFont>>, u64) {
    with_fonts(|fonts| {
        let font = fonts.ui_font(face).cloned();
        (font, font_epoch_in(fonts))
    })
}

/// [`strip_band_font_epoch`] of `fonts`.
fn font_epoch_in(fonts: &mut ChromeFonts) -> u64 {
    fonts.poll_semantic_renderer();
    fonts.sync_coverage();
    let ui_cjk = fonts
        .ui_cjk
        .as_ref()
        .map_or(0, |store| store.parsed_count() as u64);
    fonts
        .semantic_ready_epoch
        .wrapping_shl(12)
        .wrapping_add(ui_cjk.min(15) << 8)
        .wrapping_add((fonts.coverage.len() as u64).min(127) << 1)
        .wrapping_add(u64::from(fonts.ui_regular.is_some()))
}

/// Whether a real proportional UI face is installed.
#[cfg(any(not(target_os = "macos"), test))]
pub(crate) fn strip_band_ui_ready() -> bool {
    with_fonts(|fonts| fonts.ui_regular.is_some())
}

/// The variable semibold instance for the strip band's active label, or `None` when the host's UI
/// regular is static (the caller then draws `TextFace::UiBold` from the static sibling or regular).
#[cfg(not(target_os = "macos"))]
pub(crate) fn strip_band_variable_semibold() -> Option<UiVariableSemibold> {
    let installed = with_fonts(|fonts| fonts.ui_regular.clone())?;
    prepared_ui_font_assets()
        .variable_semibold
        .clone()
        .filter(|variable| Arc::ptr_eq(&variable.cmap, &installed))
}

/// Whether the chrome stack draws every char of `s` as real ink, routed as [`Canvas::text`]'s
/// proportional arm routes it (UI face, terminal cascade, parsed chain faces, the pen's own
/// glyphs); a char none of them draws would be a hole in a tab title.
pub(crate) fn strip_band_run_coverable(s: &str) -> bool {
    strip_band_run_coverable_in(TextFace::Ui, s)
}

/// [`strip_band_run_coverable`] for a label set in `face`, whose East Asian text is looked up in
/// the faces of `face`'s weight ([`UiCjkWeight::of`]).
pub(crate) fn strip_band_run_coverable_in(face: TextFace, s: &str) -> bool {
    use std::ops::ControlFlow::{Break, Continue};
    let (ui_cjk, coverage, wanted) = match with_fonts(|fonts| {
        if fonts.ui_regular.is_none() {
            return Break(false);
        }
        let misses = fonts.label_misses(face, s);
        if misses.is_empty() {
            return Break(true);
        }
        if !misses.iter().copied().all(chain_may_draw_in_a_label) {
            return Break(false);
        }
        Continue((
            fonts.ui_cjk.clone(),
            fonts.semantic.as_ref().map(Renderer::fallback_coverage),
            misses.into_iter().collect::<String>(),
        ))
    }) {
        Break(coverable) => return coverable,
        Continue(wanted) => wanted,
    };
    let rest = match ui_cjk {
        Some(store) => store.materialise_for(UiCjkWeight::of(face), &wanted).1,
        None => wanted,
    };
    if !rest.is_empty() {
        let Some(coverage) = coverage else {
            return false;
        };
        coverage.materialise_for(&rest);
    }
    with_fonts(|fonts| fonts.label_misses(face, s).is_empty())
}

fn chain_may_draw_in_a_label(ch: char) -> bool {
    !(ch.is_control()
        || aterm_grapheme::is_emoji_char(ch)
        || aterm_grapheme::is_emoji_presentation(ch)
        || matches!(
            ch,
            '\u{200C}'
                | '\u{200D}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{E0100}'..='\u{E01EF}'
                | '\u{E000}'..='\u{F8FF}'
                | '\u{F0000}'..='\u{FFFFD}'
                | '\u{100000}'..='\u{10FFFD}'
        ))
}

/// One colour emoji the chrome draws inline in a label (the tab strip's titles): the colour face's
/// own straight-alpha RGBA, `width × height`, at the size it was asked for.
#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug)]
pub(crate) struct ChromeEmoji {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) rgba: Vec<u8>,
}

/// The colour glyph of the emoji grapheme `cluster` at `px` device px from the chrome stack's
/// semantic renderer (the terminal's own font set), so a tab title draws the emoji the grid draws.
#[cfg(any(windows, target_os = "linux"))]
pub(crate) fn chrome_colour_emoji(cluster: &str, px: f32) -> Option<Arc<ChromeEmoji>> {
    type Memo = (
        u64,
        u32,
        Renderer,
        HashMap<String, Option<Arc<ChromeEmoji>>>,
    );
    thread_local! {
        static MEMO: std::cell::RefCell<Option<Memo>> = const { std::cell::RefCell::new(None) };
    }
    if !(px.is_finite() && px >= 1.0) || cluster.is_empty() {
        return None;
    }
    let generation = with_fonts(|fonts| fonts.semantic_generation);
    MEMO.with_borrow_mut(|memo| {
        if memo
            .as_ref()
            .is_none_or(|(held, bits, _, _)| *held != generation || *bits != px.to_bits())
        {
            let fork = with_fonts(|fonts| {
                fonts
                    .semantic
                    .as_ref()?
                    .fork_semantic_surface(px, aterm_render::Theme::default())
            })?;
            *memo = Some((generation, px.to_bits(), fork, HashMap::new()));
        }
        let (_, _, renderer, rasters) = memo.as_mut()?;
        if let Some(raster) = rasters.get(cluster) {
            return raster.clone();
        }
        let raster =
            renderer
                .glyph_key_cluster(cluster)
                .and_then(|key| match renderer.glyph_image(key) {
                    aterm_render::GlyphImage::Rgba {
                        width,
                        height,
                        bytes,
                        ..
                    } if *width > 0 && *height > 0 && bytes.len() == width * height * 4 => {
                        Some(Arc::new(ChromeEmoji {
                            width: *width,
                            height: *height,
                            rgba: bytes.clone(),
                        }))
                    }
                    _ => None,
                });
        rasters.insert(cluster.to_string(), raster.clone());
        raster
    })
}

/// Slight negative tracking (em) per glyph advance of the UI face, which reads a touch loose at
/// panel sizes.
pub(crate) const UI_TRACKING_EM: f32 = -0.008;

const PEN_PHASES: f32 = 4.0;

fn pen_phase(pen: f32) -> (i32, u8) {
    let quarters = (pen * PEN_PHASES).round();
    let whole = (quarters / PEN_PHASES).floor();
    let phase = (quarters - whole * PEN_PHASES).clamp(0.0, PEN_PHASES - 1.0);
    (whole as i32, phase as u8)
}

/// Width of `s` at `px` in the UI face: real advances plus `horizontal_kern` plus the
/// [`UI_TRACKING_EM`] the raster pen applies.
pub(crate) fn ui_text_width(s: &str, px: f32) -> f32 {
    ui_text_width_for(TextFace::Ui, s, px)
}

fn ui_text_wrap_ranges_impl(
    text: &str,
    px: f32,
    maximum_width: f32,
) -> (Vec<std::ops::Range<usize>>, usize) {
    let maximum_width = maximum_width.max(1.0);
    if let Some(lines) = with_fonts(|fonts| {
        fonts
            .ui_font(TextFace::Ui)
            .and_then(|font| fonts.measures.wrap(font, px, maximum_width, text))
    }) {
        return lines;
    }
    let (lines, font) = ui_text_wrap_walk(text, px, maximum_width);
    if let Some(font) = font {
        with_fonts(|fonts| {
            fonts
                .measures
                .keep_wrap(&font, px, maximum_width, text, &lines)
        });
    }
    lines
}

fn ui_text_wrap_walk(
    text: &str,
    px: f32,
    maximum_width: f32,
) -> (WrapLines, Option<Arc<ChromeFont>>) {
    note_measure_walk();
    let graphemes = text
        .grapheme_indices()
        .map(|(byte, grapheme)| {
            (
                byte,
                grapheme,
                grapheme.chars().all(aterm_grapheme::is_break_space),
            )
        })
        .collect::<Vec<_>>();
    if graphemes.is_empty() {
        return ((std::iter::once(0..0).collect(), 0), None);
    }

    let ascii_slot = |character: char| {
        u8::try_from(character)
            .ok()
            .filter(u8::is_ascii)
            .map(usize::from)
    };
    let (font, ascii, advances) = with_fonts(|fonts| {
        let font = fonts.ui_font(TextFace::Ui).cloned();
        let mut ascii: [Option<(f32, bool)>; 128] = [None; 128];
        let mut advances: HashMap<char, (f32, bool)> = HashMap::new();
        for character in text.chars() {
            let known = match ascii_slot(character) {
                Some(slot) => ascii[slot].is_some(),
                None => advances.contains_key(&character),
            };
            if known {
                continue;
            }
            let (advance, ui_covers) = match font.as_ref() {
                None => (px * 0.6, false),
                Some(f) if f.maps(character) => (
                    f.char_metrics(character, px).advance_width + UI_TRACKING_EM * px,
                    true,
                ),
                Some(_) => (fonts.ui_miss_advance(TextFace::Ui, character, px), false),
            };
            match ascii_slot(character) {
                Some(slot) => ascii[slot] = Some((advance, ui_covers)),
                None => {
                    advances.insert(character, (advance, ui_covers));
                }
            }
        }
        (font, ascii, advances)
    });
    let whole_font = font
        .clone()
        .filter(|_| advances.values().all(|(_, ui_covers)| *ui_covers));
    if whole_font.is_some() {
        note_whole_walk();
    }
    let charged = |character: char| -> (f32, bool) {
        match ascii_slot(character) {
            Some(slot) => ascii[slot],
            None => advances.get(&character).copied(),
        }
        .unwrap_or((px * 0.6, false))
    };
    let font = font.as_deref();
    let piece_width = |piece: &str, previous: Option<char>| {
        let mut width = 0.0;
        let mut previous = previous;
        for character in piece.chars() {
            let (advance, ui_covers) = charged(character);
            if ui_covers && let (Some(font), Some(previous)) = (font, previous) {
                width += font.char_kern(previous, character, px).unwrap_or(0.0);
            }
            width += advance;
            previous = ui_covers.then_some(character);
        }
        (width, previous)
    };
    let exact_width = |piece: &str| piece_width(piece, None).0;

    let byte_at = |index: usize| {
        graphemes
            .get(index)
            .map_or(text.len(), |(byte, _, _)| *byte)
    };
    let ideographic: Vec<usize> = if text.is_ascii() {
        Vec::new()
    } else {
        aterm_grapheme::line_breaks(text)
            .into_iter()
            .filter(|brk| brk.kind == aterm_grapheme::BreakKind::Ideographic)
            .map(|brk| brk.at)
            .collect()
    };
    let breaks_before = |index: usize| {
        !ideographic.is_empty() && ideographic.binary_search(&byte_at(index)).is_ok()
    };
    let held: Vec<usize> = {
        let mut held = Vec::new();
        let mut index = 0;
        while index < graphemes.len() {
            if graphemes[index].2 {
                index += 1;
                continue;
            }
            let start = index;
            while index < graphemes.len() && !graphemes[index].2 {
                index += 1;
            }
            let token = &text[byte_at(start)..byte_at(index)];
            if start > 0 && aterm_grapheme::never_starts_a_line(token) {
                let mut space = start;
                while space > 0 && graphemes[space - 1].2 {
                    held.push(space);
                    space -= 1;
                }
            }
        }
        held.sort_unstable();
        held
    };
    let may_break_before = |index: usize| held.binary_search(&index).is_err();
    let mut ranges = Vec::new();
    let mut line_start = 0usize;
    let mut index = 0usize;
    let mut width = 0.0;
    let mut previous = None;
    let mut soft_break = None;
    let mut scans = 0usize;
    while index < graphemes.len() {
        let (_, grapheme, whitespace) = graphemes[index];
        let (advance, next) = piece_width(grapheme, previous);
        scans += 1;
        if index > line_start && width + advance > maximum_width {
            let split = if whitespace {
                let mut split = index + 1;
                while graphemes.get(split).is_some_and(|(_, _, space)| *space) {
                    split += 1;
                }
                if may_break_before(split) {
                    split
                } else {
                    soft_break
                        .filter(|soft| *soft > line_start)
                        .unwrap_or(split)
                }
            } else {
                soft_break
                    .filter(|split| *split > line_start)
                    .unwrap_or(index)
            };
            let range = byte_at(line_start)..byte_at(split);
            debug_assert!(
                exact_width(text[range.clone()].trim_end()) <= maximum_width + 0.5
                    || split == line_start + 1
            );
            ranges.push(range);
            line_start = split;
            index = split;
            width = 0.0;
            previous = None;
            soft_break = None;
            continue;
        }
        width += advance;
        previous = next;
        index += 1;
        if (whitespace && may_break_before(index)) || breaks_before(index) {
            soft_break = Some(index);
        }
    }
    if line_start < graphemes.len() || ranges.is_empty() {
        let range = byte_at(line_start)..text.len();
        debug_assert!(
            exact_width(text[range.clone()].trim_end()) <= maximum_width + 0.5
                || line_start + 1 == graphemes.len()
        );
        ranges.push(range);
    }
    ((ranges, scans), whole_font)
}

/// Exact, whitespace-preserving UI-face line breaks: after white space, or between CJK characters
/// with kinsoku kept ([`aterm_grapheme::line_breaks`]), else inside a token too long for the line.
pub(crate) fn ui_text_wrap_ranges(
    text: &str,
    px: f32,
    maximum_width: f32,
) -> Vec<std::ops::Range<usize>> {
    ui_text_wrap_ranges_impl(text, px, maximum_width).0
}

thread_local! {
    /// Whether a [`ui_text_width_for`] in [`ui_measures_whole`] measured a width the memo drops.
    static UI_MEASURE_PARTIAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// What `measure` returns, and whether every UI-face width it measured is one the memo keeps.
pub(crate) fn ui_measures_whole<T>(measure: impl FnOnce() -> T) -> (T, bool) {
    let outer = UI_MEASURE_PARTIAL.with(|partial| partial.replace(false));
    let out = measure();
    let partial = UI_MEASURE_PARTIAL.with(|partial| partial.replace(outer || partial.get()));
    (out, !partial)
}

/// Face-aware proportional measure.
pub(crate) fn ui_text_width_for(face: TextFace, s: &str, px: f32) -> f32 {
    with_fonts(|fonts| ui_text_width_in(fonts, face, s, px))
}

/// [`ui_text_width_for`] of each of `texts`, in one visit to the font store: a list measuring a
/// line of each of its many entries (the Messages log's titles, up to its ring's 512) measures
/// them together rather than entering the store once each.
pub(crate) fn ui_text_widths_for(face: TextFace, texts: &[&str], px: f32) -> Vec<f32> {
    with_fonts(|fonts| {
        texts
            .iter()
            .map(|text| ui_text_width_in(fonts, face, text, px))
            .collect()
    })
}

fn ui_text_width_in(fonts: &mut ChromeFonts, face: TextFace, s: &str, px: f32) -> f32 {
    if let Some(font) = fonts.ui_font(face)
        && let Some(width) = fonts.measures.width(face, font, px, s)
    {
        return width;
    }
    let font = fonts.ui_font(face).cloned();
    let (width, _, whole) = ui_text_width_walk(fonts, face, s, px, 0.0);
    match font.as_ref() {
        Some(font) if whole => fonts.measures.keep_width(face, font, px, s, width),
        _ => UI_MEASURE_PARTIAL.with(|partial| partial.set(true)),
    }
    width
}

/// The installed font [`ui_text_width_for`] measures `face` in, or `None` with no UI face installed
/// (each scalar then measures 0.6 em).
pub(crate) fn ui_font_of(face: TextFace) -> Option<Arc<ChromeFont>> {
    with_fonts(|fonts| fonts.ui_font(face).cloned())
}

/// Whether every scalar in any substring of `s` advances at least `minimum` pixels under the
/// routing, tracking and kerning the proportional pen uses.
pub(crate) fn ui_text_advances_at_least(face: TextFace, s: &str, px: f32, minimum: f32) -> bool {
    with_fonts(|fonts| ui_text_width_with_floor(fonts, face, s, px, minimum).1)
}

fn ui_text_width_with_floor(
    fonts: &mut ChromeFonts,
    face: TextFace,
    s: &str,
    px: f32,
    minimum: f32,
) -> (f32, bool) {
    let (width, bounded, _) = ui_text_width_walk(fonts, face, s, px, minimum);
    (width, bounded)
}

fn ui_text_width_walk(
    fonts: &mut ChromeFonts,
    face: TextFace,
    s: &str,
    px: f32,
    minimum: f32,
) -> (f32, bool, bool) {
    note_measure_walk();
    let fits = |advance: f32, kern: f32| {
        advance.is_finite()
            && kern.is_finite()
            && advance >= minimum
            && advance + kern.min(0.0) >= minimum
    };
    let mut bounded = minimum.is_finite() && minimum >= 0.0;
    let Some(f) = fonts.ui_font(face).cloned() else {
        return (
            s.chars().count() as f32 * px * 0.6,
            bounded && fits(px * 0.6, 0.0),
            false,
        );
    };
    let mut pen = UiPen::default();
    for ch in s.chars() {
        bounded &= pen.push(fonts, &f, face, ch, px, &fits);
    }
    if pen.whole {
        note_whole_walk();
    }
    (pen.w, bounded, pen.whole)
}

#[derive(Clone, Copy)]
struct UiPen {
    w: f32,
    prev: Option<(char, f32)>,
    whole: bool,
}

impl Default for UiPen {
    fn default() -> Self {
        Self {
            w: 0.0,
            prev: None,
            whole: true,
        }
    }
}

impl UiPen {
    fn push(
        &mut self,
        fonts: &mut ChromeFonts,
        f: &ChromeFont,
        face: TextFace,
        ch: char,
        px: f32,
        fits: &impl Fn(f32, f32) -> bool,
    ) -> bool {
        if f.maps(ch) {
            let advance = f.char_metrics(ch, px).advance_width + UI_TRACKING_EM * px;
            let kept = if let Some((p, before)) = self.prev {
                let kern = f.char_kern(p, ch, px).unwrap_or(0.0);
                self.w += kern;
                fits(advance, kern) && fits(before, kern)
            } else {
                fits(advance, 0.0)
            };
            self.w += advance;
            self.prev = Some((ch, advance));
            return kept;
        }
        self.prev = None;
        self.whole = false;
        let advance = fonts.ui_miss_advance(face, ch, px);
        self.w += advance;
        fits(advance, 0.0)
    }
}

/// What [`ui_text_width_for`] measures `text[..cut] + tail` at for each of `cuts` (ascending byte
/// offsets into `text`, each on a char boundary), in one pass of the pen, so an elider pays one
/// measure rather than one per grapheme it sheds.
pub(crate) fn ui_text_widths_at(
    face: TextFace,
    text: &str,
    cuts: &[usize],
    tail: &str,
    px: f32,
) -> Vec<f32> {
    with_fonts(|fonts| {
        let Some(f) = fonts.ui_font(face).cloned() else {
            let tail_chars = tail.chars().count();
            return cuts
                .iter()
                .map(|&cut| {
                    (text.get(..cut).map_or(0, |prefix| prefix.chars().count()) + tail_chars) as f32
                        * px
                        * 0.6
                })
                .collect();
        };
        let any = |_: f32, _: f32| true;
        let mut pen = UiPen::default();
        let mut chars = text.char_indices().peekable();
        cuts.iter()
            .map(|&cut| {
                while let Some(&(at, ch)) = chars.peek() {
                    if at >= cut {
                        break;
                    }
                    pen.push(fonts, &f, face, ch, px, &any);
                    chars.next();
                }
                let mut tailed = pen;
                for ch in tail.chars() {
                    tailed.push(fonts, &f, face, ch, px, &any);
                }
                tailed.w
            })
            .collect()
    })
}

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

/// [`linear_to_srgb_u8`]'s tables: the least linear value that encodes past each byte, and a
/// coarse first guess per bucket of the unit interval.
struct SrgbQuantizer {
    thresholds: [f32; 255],
    coarse: [u8; QUANTIZER_BUCKETS + 1],
}

const QUANTIZER_BUCKETS: usize = 8_192;

fn srgb_quantizer() -> &'static SrgbQuantizer {
    const BUCKETS: usize = QUANTIZER_BUCKETS;
    static QUANTIZER: OnceLock<SrgbQuantizer> = OnceLock::new();
    QUANTIZER.get_or_init(|| {
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
        SrgbQuantizer { thresholds, coarse }
    })
}

fn linear_to_srgb_u8(l: f32) -> u8 {
    const BUCKETS: usize = QUANTIZER_BUCKETS;
    let l = l.clamp(0.0, 1.0);
    let quantizer = srgb_quantizer();
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

/// What a glyph's bitmap is a function of: the address of the face that rasterized it (0 for the
/// pen's own designs, [`synthesized`]), its size in device px (so the scale factor is in it), the
/// char, and the pen's quarter-pixel phase ([`pen_phase`]). A face is one family at one weight,
/// immutable once parsed. The bitmap is coverage alone: ink, ground and the contrast correction
/// are applied as it is drawn ([`Canvas::blend_glyph_texel`]), so none of them is in the key.
type GlyphCacheKey = (usize, u32, char, u8);

/// The glyphs [`Canvas::raster_glyph_at`] rasterized, kept from one raster to the next in
/// [`ChromeFonts::glyphs`] ([`rasterize_tray_on_canvas`]): a reading of the CPU close-up or the
/// Deck draws the glyphs the reading before it drew. A font install drops them, and so do a
/// window's scale change and a theme change ([`forget_kept_glyphs`]).
#[derive(Default)]
struct GlyphMemo {
    glyphs: aterm_hash::KeyedHashMap<GlyphCacheKey, KeptGlyph>,
    /// Every face a key names by its address, held, so no other face is allocated at that address
    /// while the key stands.
    faces: Vec<Arc<ChromeFont>>,
    /// The kept bitmaps' bytes.
    bytes: usize,
    /// The raster drawing now ([`Self::begin`]), whose glyphs [`Self::trim`] keeps.
    raster: u64,
}

struct KeptGlyph {
    metrics: ChromeMetrics,
    bitmap: Arc<[u8]>,
    /// The last raster that drew it.
    raster: u64,
}

/// The most glyphs [`GlyphMemo`] keeps past a raster: a few surfaces' text at two scales.
const GLYPH_MEMO_ENTRIES: usize = 4096;
/// The most bitmap bytes [`GlyphMemo`] keeps past a raster.
const GLYPH_MEMO_BYTES: usize = 4 * 1024 * 1024;

impl GlyphMemo {
    fn get(&mut self, key: &GlyphCacheKey) -> Option<(ChromeMetrics, Arc<[u8]>)> {
        let kept = self.glyphs.get_mut(key)?;
        kept.raster = self.raster;
        Some((kept.metrics, Arc::clone(&kept.bitmap)))
    }

    /// Keep `bitmap` under `key`, holding `face`, the face whose address the key names.
    fn keep(
        &mut self,
        face: Option<&Arc<ChromeFont>>,
        key: GlyphCacheKey,
        metrics: ChromeMetrics,
        bitmap: &Arc<[u8]>,
    ) {
        if let Some(face) = face
            && !self.faces.iter().any(|held| Arc::ptr_eq(held, face))
        {
            self.faces.push(Arc::clone(face));
        }
        self.bytes = self.bytes.saturating_add(bitmap.len());
        let kept = KeptGlyph {
            metrics,
            bitmap: Arc::clone(bitmap),
            raster: self.raster,
        };
        if let Some(was) = self.glyphs.insert(key, kept) {
            self.bytes = self.bytes.saturating_sub(was.bitmap.len());
        }
    }

    const fn begin(&mut self) {
        self.raster = self.raster.wrapping_add(1);
    }

    /// Past its bounds, keep only the glyphs the raster that just ended drew, and the faces their
    /// keys name; a raster that alone draws past them leaves nothing kept.
    fn trim(&mut self) {
        if self.glyphs.len() <= GLYPH_MEMO_ENTRIES && self.bytes <= GLYPH_MEMO_BYTES {
            return;
        }
        let raster = self.raster;
        self.glyphs.retain(|_, kept| kept.raster == raster);
        self.bytes = self.glyphs.values().map(|kept| kept.bitmap.len()).sum();
        if self.glyphs.len() > GLYPH_MEMO_ENTRIES || self.bytes > GLYPH_MEMO_BYTES {
            *self = Self {
                raster,
                ..Self::default()
            };
            return;
        }
        let glyphs = &self.glyphs;
        self.faces.retain(|face| {
            let at = Arc::as_ptr(face) as usize;
            glyphs.keys().any(|&(address, ..)| address == at)
        });
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.glyphs.len()
    }
}

#[derive(Default)]
struct RimMemo {
    entries: [Option<RimComposite>; 8],
    next: usize,
}

type RimComposite = ((u32, u32), [u8; 4]);

impl RimMemo {
    fn get(&self, key: (u32, u32)) -> Option<[u8; 4]> {
        self.entries
            .iter()
            .flatten()
            .find(|(held, _)| *held == key)
            .map(|(_, became)| *became)
    }

    const fn put(&mut self, key: (u32, u32), became: [u8; 4]) {
        self.entries[self.next] = Some((key, became));
        self.next = (self.next + 1) % self.entries.len();
    }
}

struct Canvas {
    origin_x: i32,
    origin_y: i32,
    w: u32,
    h: u32,
    px: Vec<u8>,
    glyphs: GlyphMemo,
    clip: Vec<(i32, i32, i32, i32)>,
    glyph_remap: (u32, u32, [f32; 256]),
    fade: Vec<FadeLayer>,
    /// How many [`DrawPrim::SubpixelPush`]es are open: while any is, panels and rims keep
    /// their sub-pixel places instead of the device grid.
    subpixel: u32,
}

struct FadeLayer {
    top: f32,
    bottom: f32,
    from: f32,
    to: f32,
    region: DeviceRect,
    under: Vec<u8>,
}

impl FadeLayer {
    fn opacity_at(&self, y: i32) -> f32 {
        let at = y as f32 + 0.5;
        if at < self.top {
            return 1.0;
        }
        let t = if self.bottom > self.top {
            ((at - self.top) / (self.bottom - self.top)).clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.to + (self.from - self.to) * (1.0 - t) * (1.0 - t)
    }
}

impl Canvas {
    fn new(w: u32, h: u32, backdrop: [u8; 4]) -> Self {
        Self::with_origin(0, 0, w, h, backdrop)
    }

    /// A `w × h` canvas holding `px` (transparent wherever `px` falls short of it).
    fn over(mut px: Vec<u8>, w: u32, h: u32) -> Self {
        px.resize((w * h * 4) as usize, 0);
        Self {
            w,
            h,
            px,
            ..Self::with_origin(0, 0, 0, 0, [0, 0, 0, 0])
        }
    }

    fn with_origin(origin_x: i32, origin_y: i32, w: u32, h: u32, backdrop: [u8; 4]) -> Self {
        let len = (w * h * 4) as usize;
        // `repeat` fills by doubling copies, not one store a pixel.
        let px = if backdrop == [0, 0, 0, 0] {
            vec![0u8; len]
        } else {
            backdrop.repeat(len / 4)
        };
        Self {
            origin_x,
            origin_y,
            w,
            h,
            px,
            glyphs: GlyphMemo::default(),
            clip: Vec::new(),
            glyph_remap: (u32::MAX, u32::MAX, [f32::NAN; 256]),
            fade: Vec::new(),
            subpixel: 0,
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
        font: &Arc<ChromeFont>,
        character: char,
        px: f32,
    ) -> (ChromeMetrics, Arc<[u8]>) {
        self.raster_glyph_at(font, character, px, 0)
    }

    fn raster_glyph_at(
        &mut self,
        font: &Arc<ChromeFont>,
        character: char,
        px: f32,
        phase: u8,
    ) -> (ChromeMetrics, Arc<[u8]>) {
        let key = (Arc::as_ptr(font) as usize, px.to_bits(), character, phase);
        if let Some(kept) = self.glyphs.get(&key) {
            return kept;
        }
        note_glyph_raster();
        let (metrics, bitmap) = if phase == 0 {
            font.char_raster(character, px)
        } else {
            font.char_raster_at(character, px, f32::from(phase) / PEN_PHASES)
        };
        let bitmap: Arc<[u8]> = bitmap.into();
        self.glyphs.keep(Some(font), key, metrics, &bitmap);
        (metrics, bitmap)
    }

    fn raster_synthesized(
        &mut self,
        character: char,
        px: f32,
    ) -> Option<(ChromeMetrics, Arc<[u8]>)> {
        let key = (0, px.to_bits(), character, 0);
        if let Some(kept) = self.glyphs.get(&key) {
            return Some(kept);
        }
        note_glyph_raster();
        let (metrics, bitmap) = synthesized::rasterize(character, px)?;
        let bitmap: Arc<[u8]> = bitmap.into();
        self.glyphs.keep(None, key, metrics, &bitmap);
        Some((metrics, bitmap))
    }

    fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let mut r = device_clip_rect(x, y, w, h);
        if let Some(&(cx0, cy0, cx1, cy1)) = self.clip.last() {
            r.0 = r.0.max(cx0);
            r.1 = r.1.max(cy0);
            r.2 = r.2.min(cx1);
            r.3 = r.3.min(cy1);
        }
        self.clip.push(r);
    }

    fn pop_clip(&mut self) {
        self.clip.pop();
    }

    fn push_fade(&mut self, top: f32, bottom: f32, from: f32, to: f32, draw: bool) {
        let mut layer = FadeLayer {
            top,
            bottom,
            from,
            to,
            region: (0, 0, 0, 0),
            under: Vec::new(),
        };
        if draw {
            let first = (top - 0.5).ceil();
            let first = if first.is_finite() {
                first.clamp(i32::MIN as f32, i32::MAX as f32) as i32
            } else {
                i32::MAX
            };
            let (x0, _, x1, y1) = self.device_bounds();
            let (x0, y0, x1, y1) = self.clipped_bounds(x0, first.max(self.origin_y), x1, y1);
            if x0 < x1 && y0 < y1 {
                let stride = self.w as usize * 4;
                let (local_x0, local_x1) =
                    ((x0 - self.origin_x) as usize, (x1 - self.origin_x) as usize);
                let mut under = Vec::with_capacity((x1 - x0) as usize * (y1 - y0) as usize * 4);
                for y in y0..y1 {
                    let row = (y - self.origin_y) as usize * stride;
                    under.extend_from_slice(&self.px[row + local_x0 * 4..row + local_x1 * 4]);
                }
                layer.region = (x0, y0, x1, y1);
                layer.under = under;
            }
        }
        self.fade.push(layer);
    }

    fn pop_fade(&mut self) {
        let Some(layer) = self.fade.pop() else {
            return;
        };
        let (x0, y0, x1, y1) = layer.region;
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let lut = srgb_to_linear_lut();
        let stride = self.w as usize * 4;
        let width = (x1 - x0) as usize * 4;
        let local_x0 = (x0 - self.origin_x) as usize * 4;
        for y in y0..y1 {
            let opacity = layer.opacity_at(y).clamp(0.0, 1.0);
            if opacity >= 1.0 {
                continue;
            }
            let start = (y - self.origin_y) as usize * stride + local_x0;
            let under = &layer.under[(y - y0) as usize * width..][..width];
            let target = &mut self.px[start..start + width];
            if opacity <= 0.0 {
                target.copy_from_slice(under);
                continue;
            }
            let mut last: Option<([u8; 4], [u8; 4], [u8; 4])> = None;
            for (out, was) in target
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(under.as_chunks::<4>().0)
            {
                if out == was {
                    continue;
                }
                if let Some((drawn, ground, became)) = last
                    && drawn == *out
                    && ground == *was
                {
                    *out = became;
                    continue;
                }
                let drawn = *out;
                let (ua, da) = (f32::from(was[3]) / 255.0, f32::from(drawn[3]) / 255.0);
                let (wu, wd) = (ua * (1.0 - opacity), da * opacity);
                let out_a = wu + wd;
                let became = if out_a > 0.0 {
                    let mut mixed = [0, 0, 0, (out_a * 255.0).round().clamp(0.0, 255.0) as u8];
                    for k in 0..3 {
                        let lin = lut[usize::from(was[k])]
                            .mul_add(wu, lut[usize::from(drawn[k])] * wd)
                            / out_a;
                        mixed[k] = linear_to_srgb_u8(lin);
                    }
                    mixed
                } else {
                    [0, 0, 0, 0]
                };
                *out = became;
                last = Some((drawn, *was, became));
            }
        }
    }

    fn active_clip_intersects_tile(&self) -> bool {
        let Some(&(clip_x0, clip_y0, clip_x1, clip_y1)) = self.clip.last() else {
            return true;
        };
        let (tile_x0, tile_y0, tile_x1, tile_y1) = self.device_bounds();
        clip_x1 > tile_x0 && clip_y1 > tile_y0 && clip_x0 < tile_x1 && clip_y0 < tile_y1
    }

    fn blend(&mut self, x: i32, y: i32, c: [u8; 4], cov: f32) {
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
        let cov = cov.clamp(0.0, 1.0);
        let a = (f32::from(c[3]) / 255.0) * cov;
        if a <= 0.0 {
            return;
        }
        let i = ((local_y as u32 * self.w + local_x as u32) * 4) as usize;
        self.composite_at(i, c, cov, a);
    }

    fn blend_span(&mut self, x0: i32, x1: i32, y: i32, c: [u8; 4], cov: f32, memo: &mut RimMemo) {
        let (canvas_x0, canvas_y0, canvas_x1, canvas_y1) = self.device_bounds();
        let (mut x0, mut x1) = (x0.max(canvas_x0), x1.min(canvas_x1));
        if y < canvas_y0 || y >= canvas_y1 {
            return;
        }
        if let Some(&(clip_x0, clip_y0, clip_x1, clip_y1)) = self.clip.last() {
            if y < clip_y0 || y >= clip_y1 {
                return;
            }
            (x0, x1) = (x0.max(clip_x0), x1.min(clip_x1));
        }
        if x0 >= x1 {
            return;
        }
        let local_y = y - self.origin_y;
        let (local_x0, local_x1) = (x0 - self.origin_x, x1 - self.origin_x);
        let cov = cov.clamp(0.0, 1.0);
        let a = (f32::from(c[3]) / 255.0) * cov;
        if a <= 0.0 {
            return;
        }
        let row = (local_y as u32 * self.w) as usize;
        let start = (row + local_x0 as usize) * 4;
        let end = (row + local_x1 as usize) * 4;
        if c[3] == u8::MAX && cov >= 1.0 {
            let (pixels, _) = self.px[start..end].as_chunks_mut::<4>();
            pixels.fill(c);
            return;
        }
        let mut last: Option<(u32, [u8; 4])> = None;
        for i in (start..end).step_by(4) {
            let under = self.pixel_word(i);
            if let Some((was, became)) = last
                && was == under
            {
                self.px[i..i + 4].copy_from_slice(&became);
                continue;
            }
            let key = (under, cov.to_bits());
            if let Some(became) = memo.get(key) {
                self.px[i..i + 4].copy_from_slice(&became);
            } else {
                self.composite_at(i, c, cov, a);
                memo.put(
                    key,
                    [self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]],
                );
            }
            last = Some((
                under,
                [self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]],
            ));
        }
    }

    fn blend_rim(&mut self, x: i32, y: i32, c: [u8; 4], cov: f32, memo: &mut RimMemo) {
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
        let cov = cov.clamp(0.0, 1.0);
        let a = (f32::from(c[3]) / 255.0) * cov;
        if a <= 0.0 {
            return;
        }
        let i = ((local_y as u32 * self.w + local_x as u32) * 4) as usize;
        let key = (self.pixel_word(i), cov.to_bits());
        if let Some(became) = memo.get(key) {
            self.px[i..i + 4].copy_from_slice(&became);
            return;
        }
        self.composite_at(i, c, cov, a);
        memo.put(
            key,
            [self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]],
        );
    }

    fn pixel_word(&self, i: usize) -> u32 {
        u32::from_ne_bytes([self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]])
    }

    fn composite_at(&mut self, i: usize, c: [u8; 4], cov: f32, a: f32) {
        // A fully covered opaque source replaces the destination exactly: source-over reduces to
        // this copy, which spares large opaque surfaces (native views, modal cards) three lookups.
        if c[3] == u8::MAX && cov >= 1.0 {
            self.px[i..i + 4].copy_from_slice(&c);
            return;
        }
        let da = f32::from(self.px[i + 3]) / 255.0;
        if cov >= 1.0 && self.px[i] == c[0] && self.px[i + 1] == c[1] && self.px[i + 2] == c[2] {
            let out_a = a + da * (1.0 - a);
            self.px[i + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
            return;
        }
        #[cfg(test)]
        LINEAR_COMPOSITES.with(|n| n.set(n.get() + 1));
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

    /// Whether a glyph whose texels land from pixel `first` to pixel `last` (inclusive) has any
    /// on this canvas: one that has none blends nothing ([`Self::blend_glyph_texel`] drops a
    /// texel off the canvas first), so a region raster ([`rasterize_tray_region`]) need not
    /// visit the texels of the text outside its region.
    fn texels_meet_canvas(&self, first: (i32, i32), last: (i32, i32)) -> bool {
        let (x0, y0, x1, y1) = self.device_bounds();
        last.0 >= x0 && last.1 >= y0 && first.0 < x1 && first.1 < y1
    }

    fn blend_glyph_texel(&mut self, x: i32, y: i32, c: [u8; 4], coverage: u8) {
        if coverage == 0 {
            return;
        }
        let local_x = x.saturating_sub(self.origin_x);
        let local_y = y.saturating_sub(self.origin_y);
        if local_x < 0 || local_y < 0 || local_x as u32 >= self.w || local_y as u32 >= self.h {
            return;
        }
        let plain = f32::from(coverage) / 255.0;
        let i = ((local_y as u32 * self.w + local_x as u32) * 4) as usize;
        if coverage == u8::MAX || self.px[i + 3] == 0 {
            self.blend(x, y, c, plain);
            return;
        }
        let pack = |p: &[u8]| u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2]);
        let ink = pack(&c[..3]);
        let ground = pack(&self.px[i..i + 3]);
        if (self.glyph_remap.0, self.glyph_remap.1) != (ink, ground) {
            self.glyph_remap = (ink, ground, [f32::NAN; 256]);
        }
        let slot = &mut self.glyph_remap.2[usize::from(coverage)];
        if slot.is_nan() {
            *slot = aterm_render::correct_alpha(
                aterm_render::linear_luminance(ink),
                aterm_render::linear_luminance(ground),
                plain,
            );
        }
        let corrected = *slot;
        self.blend(x, y, c, corrected);
    }
}

fn sd_round_box(px: f32, py: f32, cx: f32, cy: f32, hx: f32, hy: f32, r: f32) -> f32 {
    let qx = (px - cx).abs() - (hx - r);
    let qy = (py - cy).abs() - (hy - r);
    let ax = qx.max(0.0);
    let ay = qy.max(0.0);
    (ax * ax + ay * ay).sqrt() + qx.max(qy).min(0.0) - r
}

fn cov(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

/// The columns, within `x0..x1`, of the row `dy` px from a band's centre column `cx` that a band
/// covering only pixels between the radii `inner` and `outer` can cover: the run left of its
/// hole and the run right of it, the right one starting where the left ends. The rows and the
/// runs each keep a pixel's slack past those radii, so a pixel the band's distance field covers
/// only by a rounding is still walked; `None` for a row a pixel or more past `outer`. Columns
/// saturate, so a centre or radius past `i32`'s range walks no column.
fn band_runs(
    cx: f32,
    dy: f32,
    (inner, outer): (f32, f32),
    (x0, x1): (i32, i32),
) -> Option<[std::ops::Range<i32>; 2]> {
    if dy.abs() >= outer + 1.0 {
        return None;
    }
    let reach = (outer * outer - dy * dy).max(0.0).sqrt();
    let hole = if inner > dy.abs() {
        (inner * inner - dy * dy).sqrt()
    } else {
        0.0
    };
    let col = |x: f32| x - 0.5;
    let (lo, hole_end) = (
        col(cx - reach).floor() as i32,
        (col(cx - hole).ceil() as i32).saturating_add(1),
    );
    let (hole_start, hi) = (
        (col(cx + hole).floor() as i32).max(hole_end),
        (col(cx + reach).ceil() as i32).saturating_add(1),
    );
    Some([lo.max(x0)..hole_end.min(x1), hole_start.max(x0)..hi.min(x1)])
}

const SPECIMEN_FRAME_CACHE_ENTRIES: usize = 2;

struct SpecimenFrameEntry {
    key: u64,
    source: std::rc::Weak<Renderer>,
    frame: Rc<aterm_render::Frame>,
}

thread_local! {
    static SPECIMEN_FRAMES: std::cell::RefCell<Vec<SpecimenFrameEntry>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn terminal_specimen_frame_key(spec: &TerminalSpecimenSpec, scale: f32) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hash = std::collections::hash_map::DefaultHasher::new();
    specimen_renderer_key(spec, scale).hash(&mut hash);
    spec.input_fingerprint.hash(&mut hash);
    spec.motion
        .as_ref()
        .map(|scene| scene.fingerprint)
        .hash(&mut hash);
    spec.wallpaper
        .as_ref()
        .map(crate::widget::SpecimenWallpaper::identity)
        .hash(&mut hash);
    hash.finish()
}

fn specimen_with_wallpaper(
    input: &aterm_render::RenderInput,
    wallpaper: &crate::widget::SpecimenWallpaper,
    theme: aterm_render::Theme,
    frame: (usize, usize),
) -> aterm_render::RenderInput {
    type Memo = (u64, Rc<crate::wallpaper_present::PreparedWallpaper>);
    thread_local! {
        static LAST_WALLPAPER: std::cell::RefCell<Option<Memo>> =
            const { std::cell::RefCell::new(None) };
    }
    let mut out = input.clone();
    let (w, h) = frame;
    if w == 0
        || h == 0
        || wallpaper.w == 0
        || wallpaper.h == 0
        || wallpaper.rgba.len() < wallpaper.w.saturating_mul(wallpaper.h).saturating_mul(4)
    {
        return out;
    }
    let fg = if input.default_fg == aterm_core::render::COLOR_UNSET {
        theme.fg
    } else {
        input.default_fg
    };
    let key = {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        wallpaper.identity().hash(&mut hash);
        (w, h, input.rows, input.cols, theme.bg, fg).hash(&mut hash);
        hash.finish()
    };
    let prepared = LAST_WALLPAPER.with_borrow_mut(|slot| match slot {
        Some((held, prepared)) if *held == key => Rc::clone(prepared),
        _ => {
            let prepared = Rc::new(crate::wallpaper_present::prepare(
                crate::wallpaper_present::WallpaperSource {
                    rgba: &wallpaper.rgba,
                    w: wallpaper.w,
                    h: wallpaper.h,
                    fp: wallpaper.fp,
                },
                (w, h),
                (input.rows, input.cols),
                wallpaper.dim,
                (theme.bg, fg),
            ));
            *slot = Some((key, Rc::clone(&prepared)));
            prepared
        }
    });
    out.wallpaper = Some(std::sync::Arc::clone(&prepared.atlas));
    if wallpaper.tint {
        let default_fg = [(fg >> 16) as u8, (fg >> 8) as u8, fg as u8];
        let mut scratch = Vec::new();
        crate::wallpaper_present::merge_wallpaper_tint_ink(
            &input.cells,
            input.rows,
            input.cols,
            default_fg,
            &prepared.tint,
            &mut out.ink,
            &mut scratch,
        );
    }
    out
}

fn specimen_renderer_key(spec: &TerminalSpecimenSpec, scale: f32) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hash = std::collections::hash_map::DefaultHasher::new();
    spec.prepared_font.snapshot.ready_epoch.hash(&mut hash);
    spec.theme.fg.hash(&mut hash);
    spec.theme.bg.hash(&mut hash);
    spec.theme.cursor.hash(&mut hash);
    spec.theme.selection.hash(&mut hash);
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
    spec.variations.hash(&mut hash);
    spec.ligatures.hash(&mut hash);
    spec.merged_ligatures.hash(&mut hash);
    spec.cursor_break_ligatures.hash(&mut hash);
    spec.minimum_contrast.to_bits().hash(&mut hash);
    spec.selection_foreground.hash(&mut hash);
    spec.selection_inactive.hash(&mut hash);
    hash.finish()
}

fn specimen_frame(spec: &TerminalSpecimenSpec, scale: f32) -> Option<Rc<aterm_render::Frame>> {
    let key = terminal_specimen_frame_key(spec, scale);
    let source = spec.prepared_font.renderer.as_ref();
    if let Some(frame) = cached_specimen_frame(key, source) {
        return Some(frame);
    }
    let frame = Rc::new(render_specimen_frame(spec, scale)?);
    if let Some(source) = source
        && spec.motion.as_ref().is_none_or(|scene| !scene.is_moving())
    {
        retain_specimen_frame(key, source, &frame);
    }
    Some(frame)
}

fn render_specimen_frame(spec: &TerminalSpecimenSpec, scale: f32) -> Option<aterm_render::Frame> {
    render_specimen_frame_observed(spec, scale, |_, _, _| {})
}

fn render_specimen_frame_observed(
    spec: &TerminalSpecimenSpec,
    scale: f32,
    observe: impl FnOnce(
        &aterm_render::RenderInput,
        &mut Renderer,
        Option<&crate::settings_preview::CursorScene>,
    ),
) -> Option<aterm_render::Frame> {
    type HeldFork = (u64, std::rc::Weak<Renderer>, Renderer);
    thread_local! {
        static SPECIMEN_RENDERERS: std::cell::RefCell<Vec<HeldFork>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }
    let source = spec.prepared_font.renderer.as_ref()?;
    let key = specimen_renderer_key(spec, scale);
    SPECIMEN_RENDERERS.with_borrow_mut(|held| {
        let hit = held.iter().position(|(held_key, held_source, _)| {
            *held_key == key && std::ptr::eq(held_source.as_ptr(), Rc::as_ptr(source))
        });
        if let Some(hit) = hit {
            let fork = held.remove(hit);
            held.insert(0, fork);
        } else {
            let mut renderer = spec.prepared_font.fork(spec.font_px * scale, spec.theme)?;
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
                SpecimenTextBlending::LinearCorrected => {
                    aterm_render::TextBlending::LinearCorrected
                }
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
            held.insert(0, (key, Rc::downgrade(source), renderer));
            held.truncate(SPECIMEN_FRAME_CACHE_ENTRIES);
        }
        let (_, _, renderer) = held.first_mut()?;
        let frame_size = renderer
            .checked_frame_size(spec.input.rows, spec.input.cols)
            .ok()?;
        let walled = spec.wallpaper.as_ref().map(|wallpaper| {
            specimen_with_wallpaper(&spec.input, wallpaper, spec.theme, frame_size)
        });
        let input = walled.as_ref().unwrap_or(&*spec.input);
        Some(match spec.motion.as_deref() {
            Some(scene) => {
                let input = scene.decorate_with_renderer(input, renderer);
                let frame = render_decorated_specimen_input(renderer, &input, Some(scene))?;
                observe(&input, renderer, Some(scene));
                frame
            }
            None => {
                let frame = render_decorated_specimen_input(renderer, input, None)?;
                observe(input, renderer, None);
                frame
            }
        })
    })
}

/// Rasterize already-decorated input, including the scene's actual post passes, without
/// advancing or replaying the scene.
fn render_decorated_specimen_input(
    renderer: &mut Renderer,
    input: &aterm_render::RenderInput,
    scene: Option<&crate::settings_preview::CursorScene>,
) -> Option<aterm_render::Frame> {
    let mut frame = renderer.try_render_input(input).ok()?;
    if let Some(scene) = scene {
        let place = renderer.fx_frame(frame.width, frame.height, input.rows);
        let (cell_w, cell_h) = renderer.cell_size();
        let grid = aterm_render::bloom::LetterGrid {
            pad: place.pad,
            grid_top: place.grid_top,
            cell_w,
            cell_h,
        };
        scene.post_process(&mut frame, input, grid).ok()?;
    }
    Some(frame)
}

/// `spec`'s card with `input` (a cursor scene's decorated cells, its lights cleared) in place
/// of its own and no scene played: the bare twin `settings card clean` answers.
pub(crate) fn render_bare_scene_card(
    spec: &TerminalSpecimenSpec,
    input: aterm_render::RenderInput,
    scale: f32,
) -> Option<aterm_render::Frame> {
    render_specimen_frame(
        &TerminalSpecimenSpec {
            input: std::sync::Arc::new(input),
            motion: None,
            wallpaper: None,
            ..spec.clone()
        },
        scale,
    )
}

/// The rendered card and exact input/geometry its live renderer observed, plus the
/// body-only counterfactual when the scene requested that observation.
#[cfg(test)]
pub(crate) struct ObservedSpecimen {
    pub(crate) frame: aterm_render::Frame,
    pub(crate) input: aterm_render::RenderInput,
    pub(crate) cell: (usize, usize),
    pub(crate) without_body: Option<aterm_render::Frame>,
}

#[cfg(test)]
pub(crate) fn render_specimen_observed_for_test(
    spec: &TerminalSpecimenSpec,
    scale: f32,
) -> Option<ObservedSpecimen> {
    let mut observed = None;
    let frame = render_specimen_frame_observed(spec, scale, |input, renderer, scene| {
        let without_body = scene.and_then(|scene| {
            let input = crate::settings_preview::scene_card_without_body_for_test(
                input,
                scene.fingerprint,
            )?;
            render_decorated_specimen_input(renderer, &input, Some(scene))
        });
        observed = Some((input.clone(), renderer.cell_size(), without_body));
    })?;
    let (input, cell, without_body) = observed?;
    Some(ObservedSpecimen {
        frame,
        input,
        cell,
        without_body,
    })
}

/// What `spec`'s card face lays of each glyph at `scale` ([`Renderer::glyph_ink`]), as the
/// card's raster hands its scene. A fork rasterizes every printable glyph again, so this
/// thread keeps the ink of each face, size, line height and theme it has asked for; an entry
/// holds its face, so no later face takes its address.
#[cfg(test)]
pub(crate) fn specimen_glyph_ink_for_test(
    spec: &TerminalSpecimenSpec,
    scale: f32,
) -> Option<std::sync::Arc<aterm_render::GlyphInk>> {
    type Inks = Vec<(
        Rc<Renderer>,
        (u32, u32, aterm_render::Theme),
        Arc<aterm_render::GlyphInk>,
    )>;
    thread_local! {
        static INKS: std::cell::RefCell<Inks> = const { std::cell::RefCell::new(Vec::new()) };
    }
    let face = spec.prepared_font.renderer.as_ref()?;
    let px = spec.font_px * scale;
    let key = (px.to_bits(), spec.line_height.to_bits(), spec.theme);
    let held = INKS.with_borrow(|inks| {
        inks.iter()
            .find(|(held, at, _)| Rc::ptr_eq(held, face) && *at == key)
            .map(|(_, _, ink)| Arc::clone(ink))
    });
    if held.is_some() {
        return held;
    }
    let mut renderer = spec.prepared_font.fork(px, spec.theme)?;
    renderer.set_pad(0);
    renderer.set_head(0);
    renderer.set_line_height(spec.line_height);
    renderer.activate_px(px);
    let ink = renderer.glyph_ink();
    INKS.with_borrow_mut(|inks| inks.push((Rc::clone(face), key, Arc::clone(&ink))));
    Some(ink)
}

/// The device cell size a specimen renders with at `scale`, from a fresh fork.
#[cfg(test)]
pub(crate) fn specimen_cell_size_for_test(
    spec: &TerminalSpecimenSpec,
    scale: f32,
) -> Option<(usize, usize)> {
    let mut renderer = spec.prepared_font.fork(spec.font_px * scale, spec.theme)?;
    renderer.set_pad(0);
    renderer.set_head(0);
    renderer.set_line_height(spec.line_height);
    renderer.activate_px(spec.font_px * scale);
    Some(renderer.cell_size())
}

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

/// One specimen frame as [`Canvas::terminal_specimen`] renders it, bypassing the frame memo: the
/// shipping renderer's output for `spec` at `scale` on this thread's configured fork, with the
/// cursor scene played on its cells, for tests that compare the tray blit against it.
#[cfg(test)]
pub(crate) fn render_specimen_frame_for_test(
    spec: &TerminalSpecimenSpec,
    scale: f32,
) -> Option<aterm_render::Frame> {
    render_specimen_frame(spec, scale)
}

impl Canvas {
    fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: [u8; 4]) {
        self.round_rect_under(x, y, w, h, r, c, None);
    }

    /// [`Self::round_rect`], leaving `hole` (device pixels a later opaque panel fills whole:
    /// [`covered_by_a_later_panel`]) out of its rows' inner spans.
    #[allow(
        clippy::too_many_arguments,
        reason = "primitive rounded-rect fill: rect (x,y,w,h) + radius + color + the device \
                  rect it may leave out are all irreducible inputs; bundling them into a \
                  struct would only move the argument count to its two call sites"
    )]
    fn round_rect_under(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
        c: [u8; 4],
        hole: Option<DeviceRect>,
    ) {
        let (cx, cy, hx, hy) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
        let r = r.min(hx).min(hy);
        let mut x0 = (x - 1.0).floor() as i32;
        let mut y0 = (y - 1.0).floor() as i32;
        let mut x1 = (x + w + 1.0).ceil() as i32;
        let mut y1 = (y + h + 1.0).ceil() as i32;
        (x0, y0, x1, y1) = self.clipped_bounds(x0, y0, x1, y1);

        let inset = 1.0_f32;
        let inner_w = w - inset * 2.0;
        let inner_h = h - inset * 2.0;
        let inner_r = (r - inset).max(0.0).min(inner_w / 2.0).min(inner_h / 2.0);
        let inner_x = x + inset;
        let inner_y = y + inset;
        let mut rim = RimMemo::default();
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
            self.round_rect_rim(x0, left_end, py, (cx, cy, hx, hy, r), c, &mut rim);
            if let Some((start, end)) = full_span {
                match hole.filter(|&(_, top, _, bottom)| (top..bottom).contains(&py)) {
                    Some((left, _, right, _)) => {
                        self.blend_span(start, end.min(left).max(start), py, c, 1.0, &mut rim);
                        self.blend_span(start.max(right).min(end), end, py, c, 1.0, &mut rim);
                    }
                    None => self.blend_span(start, end, py, c, 1.0, &mut rim),
                }
            }
            self.round_rect_rim(right_start, x1, py, (cx, cy, hx, hy, r), c, &mut rim);
        }
    }

    fn round_rect_rim(
        &mut self,
        x0: i32,
        x1: i32,
        py: i32,
        (cx, cy, hx, hy, r): (f32, f32, f32, f32, f32),
        c: [u8; 4],
        rim: &mut RimMemo,
    ) {
        let pyc = py as f32 + 0.5;
        let qy = (pyc - cy).abs() - (hy - r);
        let straight = |px: i32| ((px as f32 + 0.5) - cx).abs() - (hx - r) <= qy.min(0.0);
        let mut px = x0;
        while px < x1 {
            if straight(px) {
                let mut end = px + 1;
                while end < x1 && straight(end) {
                    end += 1;
                }
                let d = sd_round_box(px as f32 + 0.5, pyc, cx, cy, hx, hy, r);
                self.blend_span(px, end, py, c, cov(d), rim);
                px = end;
            } else {
                let d = sd_round_box(px as f32 + 0.5, pyc, cx, cy, hx, hy, r);
                self.blend_rim(px, py, c, cov(d), rim);
                px += 1;
            }
        }
    }

    fn band_icon(&mut self, x: f32, y: f32, size: f32, icon: aterm_render::BandIcon, c: [u8; 4]) {
        let n = size.round().max(2.0) as usize;
        let metrics = aterm_render::procedural::boxed_icon_metrics(n);
        let coverage = aterm_render::procedural::band_icon_coverage(icon, n, n, metrics, false);
        let (x0, y0) = (x.round() as i32, y.round() as i32);
        for (row, line) in coverage.chunks(n).enumerate() {
            for (col, &cov) in line.iter().enumerate() {
                if cov == 0 {
                    continue;
                }
                self.blend(x0 + col as i32, y0 + row as i32, c, f32::from(cov) / 255.0);
            }
        }
    }

    /// A [`DrawPrim::Image`] in the `w`×`h` device rect at `(x, y)`: `art`
    /// baked at exactly that many device pixels ([`crate::widget::ImageArt::baked`]) and
    /// composited pixel by pixel at its own alpha scaled by `alpha`, through the clip stack.
    fn image(
        &mut self,
        (x, y, w, h): (f32, f32, f32, f32),
        art: crate::widget::ImageArt,
        alpha: u8,
    ) {
        let (width, height) = (w.round().max(0.0) as u32, h.round().max(0.0) as u32);
        if width == 0 || height == 0 || alpha == 0 {
            return;
        }
        let coverage = f32::from(alpha) / 255.0;
        let pixels = art.baked(width, height);
        let (x0, y0) = (x.round() as i32, y.round() as i32);
        for (row, line) in pixels.chunks_exact(width as usize * 4).enumerate() {
            for (col, pixel) in line.as_chunks::<4>().0.iter().enumerate() {
                if pixel[3] == 0 {
                    continue;
                }
                self.blend(x0 + col as i32, y0 + row as i32, *pixel, coverage);
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

    /// [`DrawPrim::Arc`] in device px: the band `width` across about radius `r` from `from` to
    /// `to` turns clockwise from 12 o'clock, its ends round. Each pixel is covered by its distance
    /// to the arc's middle line (across the band inside the span, to the nearer end past it), so
    /// the band and its caps cover each pixel once.
    fn arc_band(
        &mut self,
        (cx, cy): (f32, f32),
        (r, width): (f32, f32),
        (from, to): (f32, f32),
        c: [u8; 4],
    ) {
        let half = width.max(0.0) / 2.0;
        let span = (to - from).clamp(0.0, 1.0);
        if half <= 0.0 || r <= 0.0 {
            return;
        }
        let reach = r + half + 1.0;
        let (x0, y0, x1, y1) = self.clipped_bounds(
            (cx - reach).floor() as i32,
            (cy - reach).floor() as i32,
            (cx + reach).ceil() as i32,
            (cy + reach).ceil() as i32,
        );
        let end = |turn: f32| {
            let t = turn * TAU;
            (cx + t.sin() * r, cy - t.cos() * r)
        };
        let ends = [end(from), end(from + span)];
        // No point of the arc, its ends included, is nearer a pixel than the circle is, so only
        // pixels within half a pixel past the band's edges are covered: each row's run left and
        // right of the ring's hole (`band_runs`).
        let band = (r - half - 0.5, r + half + 0.5);
        for py in y0..y1 {
            let dy = py as f32 + 0.5 - cy;
            let Some([left, right]) = band_runs(cx, dy, band, (x0, x1)) else {
                continue;
            };
            for px in left.chain(right) {
                let (sx, sy) = (px as f32 + 0.5, py as f32 + 0.5);
                let dx = sx - cx;
                let radial = (dx.hypot(dy) - r).abs();
                if radial - half >= 0.5 {
                    continue;
                }
                let a = (dx.atan2(-dy) / TAU - from).rem_euclid(1.0);
                let d = if span >= 1.0 || a <= span {
                    radial
                } else {
                    ends.iter()
                        .map(|&(ex, ey)| (sx - ex).hypot(sy - ey))
                        .fold(f32::MAX, f32::min)
                };
                let coverage = cov(d - half);
                if coverage > 0.0 {
                    self.blend(px, py, c, coverage);
                }
            }
        }
    }

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
                let h = dx.atan2(-dy).rem_euclid(TAU) / TAU;
                let s = (rr / r).min(1.0);
                let rgb = crate::widget::hsv_to_rgb(h, s, value);
                self.blend(px, py, [rgb[0], rgb[1], rgb[2], 255], a);
            }
        }
    }

    /// A ring band of `thickness` about `r_mid` around `(cx, cy)`, filled for `frac` of the
    /// turn clockwise from `start` turns past 12 o'clock, in `c`.
    #[expect(
        clippy::too_many_arguments,
        reason = "one call per ring band; a struct would only rename the parameters"
    )]
    fn arc(
        &mut self,
        cx: f32,
        cy: f32,
        r_mid: f32,
        thickness: f32,
        start: f32,
        frac: f32,
        c: [u8; 4],
    ) {
        let frac = frac.clamp(0.0, 1.0);
        // A clear ink (a `Ring`'s clear track) blends nothing.
        if frac <= 0.0 || c[3] == 0 {
            return;
        }
        let r_out = r_mid + thickness / 2.0;
        let (x0, y0, x1, y1) = self.clipped_bounds(
            (cx - r_out - 1.0) as i32,
            (cy - r_out - 1.0) as i32,
            (cx + r_out + 1.0) as i32,
            (cy + r_out + 1.0) as i32,
        );
        // Only pixels within half a pixel past the band's edges are covered: each row's run left
        // and right of the ring's hole (`band_runs`).
        let band = (r_mid - thickness / 2.0 - 0.5, r_out + 0.5);
        for py in y0..y1 {
            let dy = py as f32 + 0.5 - cy;
            let Some([left, right]) = band_runs(cx, dy, band, (x0, x1)) else {
                continue;
            };
            for px in left.chain(right) {
                let dx = px as f32 + 0.5 - cx;
                let rr = (dx * dx + dy * dy).sqrt();
                let radial = (rr - r_mid).abs() - thickness / 2.0;
                let cov_r = cov(radial);
                if cov_r <= 0.0 {
                    continue;
                }
                let mut cov_a = 1.0;
                if frac < 1.0 {
                    let a = (dx.atan2(-dy).rem_euclid(TAU) / TAU - start).rem_euclid(1.0);
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
            let mut pen_q = px_to_q(x);
            let baseline_px = q_round_to_px(px_to_q(baseline)) as i32;
            for ch in s.chars() {
                let glyph = match fonts.face_for(weight, ch) {
                    Some(cface) => self.raster_glyph(&cface.font, ch, px),
                    None => {
                        fonts.note_text_hole();
                        match self.raster_synthesized(ch, px) {
                            Some(own) => own,
                            None => continue,
                        }
                    }
                };
                let (m, bitmap) = glyph;
                let gx = q_round_to_px(pen_q) as i32 + m.xmin;
                let gy = baseline_px - (m.height as i32 + m.ymin);
                if m.width > 0
                    && m.height > 0
                    && self.texels_meet_canvas(
                        (gx, gy),
                        (gx + (m.width - 1) as i32, gy + (m.height - 1) as i32),
                    )
                {
                    note_glyph_blit();
                    for gyy in 0..m.height {
                        for gxx in 0..m.width {
                            let coverage = bitmap[gyy * m.width + gxx];
                            self.blend_glyph_texel(gx + gxx as i32, gy + gyy as i32, c, coverage);
                        }
                    }
                }
                pen_q += px_to_q(m.advance_width);
            }
            return;
        }
        let ui = fonts.ui_font(face).cloned();
        let baseline_i = q_round_to_px(px_to_q(baseline)) as i32 as f32;
        let mut pen = x;
        let mut prev_ui: Option<char> = None;
        for ch in s.chars() {
            if let Some(font) = ui.as_ref().filter(|font| font.maps(ch)) {
                if let Some(previous) = prev_ui {
                    pen += font.char_kern(previous, ch, px).unwrap_or(0.0);
                }
                let advance = self.pen_glyph(font, ch, px, pen, baseline_i, c);
                pen += advance + UI_TRACKING_EM * px;
                prev_ui = Some(ch);
                continue;
            }
            prev_ui = None;

            if let Some(cface) = fonts.ui_miss_face(face, weight, ch) {
                pen += self.pen_glyph(&cface.font, ch, px, pen, baseline_i, c);
                continue;
            }
            fonts.note_text_hole();
            if let Some((m, bitmap)) = self.raster_synthesized(ch, px) {
                self.blit_fontdue_glyph(pen, baseline_i, &m, &bitmap, c);
                pen += m.advance_width;
                continue;
            }

            pen += px * 0.6;
        }
    }

    fn terminal_specimen(&mut self, x: f32, y: f32, spec: &TerminalSpecimenSpec, scale: f32) {
        let Some(frame) = specimen_frame(spec, scale) else {
            return;
        };
        let at = ((x * scale).round() as i32, (y * scale).round() as i32);
        self.blit_terminal_frame(&frame, at.0, at.1);
        crate::settings_preview::note_scene_card_painted(spec, scale, at);
    }

    fn blit_terminal_frame(&mut self, frame: &aterm_render::Frame, x: i32, y: i32) {
        if frame.width == 0 {
            return;
        }
        let rows = frame.pixels.len() / frame.width;
        let (x0, y0, x1, y1) = self.clipped_bounds(
            x,
            y,
            x.saturating_add(i32::try_from(frame.width).unwrap_or(i32::MAX)),
            y.saturating_add(i32::try_from(rows).unwrap_or(i32::MAX)),
        );
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let stride = self.w as usize * 4;
        let span = (x1 - x0) as usize;
        let skip = (x0 - x) as usize;
        let local_x = (x0 - self.origin_x) as usize;
        for py in y0..y1 {
            let source = &frame.pixels[(py - y) as usize * frame.width + skip..][..span];
            let start = (py - self.origin_y) as usize * stride + local_x * 4;
            let target = &mut self.px[start..start + span * 4];
            for (out, pixel) in target.as_chunks_mut::<4>().0.iter_mut().zip(source) {
                *out = [
                    ((pixel >> 16) & 0xff) as u8,
                    ((pixel >> 8) & 0xff) as u8,
                    (pixel & 0xff) as u8,
                    255,
                ];
            }
        }
    }

    fn pen_glyph(
        &mut self,
        font: &Arc<ChromeFont>,
        ch: char,
        px: f32,
        pen: f32,
        baseline: f32,
        color: [u8; 4],
    ) -> f32 {
        let (whole, phase) = pen_phase(pen);
        let (m, bitmap) = self.raster_glyph_at(font, ch, px, phase);
        self.blit_fontdue_glyph(whole as f32, baseline, &m, &bitmap, color);
        m.advance_width
    }

    fn blit_fontdue_glyph(
        &mut self,
        pen: f32,
        baseline: f32,
        metrics: &ChromeMetrics,
        bitmap: &[u8],
        color: [u8; 4],
    ) {
        let gx = pen + metrics.xmin as f32;
        let gy = baseline - (metrics.height as f32 + metrics.ymin as f32);
        // The texels land at `(gx + x) as i32`, which never falls as `x` grows, so the first
        // and the last texel bound the pixels they all land on.
        let (Some(last_x), Some(last_y)) =
            (metrics.width.checked_sub(1), metrics.height.checked_sub(1))
        else {
            return;
        };
        if !self.texels_meet_canvas(
            (gx as i32, gy as i32),
            ((gx + last_x as f32) as i32, (gy + last_y as f32) as i32),
        ) {
            return;
        }
        note_glyph_blit();
        for y in 0..metrics.height {
            for x in 0..metrics.width {
                let coverage = bitmap[y * metrics.width + x];
                self.blend_glyph_texel(
                    (gx + x as f32) as i32,
                    (gy + y as f32) as i32,
                    color,
                    coverage,
                );
            }
        }
    }

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

    #[allow(
        clippy::too_many_arguments,
        reason = "primitive rect-stroke blit: rect (x,y,w,h) + radius + line width + \
                  color are all irreducible geometry inputs; bundling them into a struct \
                  would only move the argument count to the call sites"
    )]
    fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, width: f32, c: [u8; 4]) {
        let sw = width.max(1.0);
        if !rounded_stroke(r, w, h, sw) {
            let t = sw.round().max(1.0);
            if w <= t || h <= t {
                self.fill_px_rect(x, y, w.max(t), h.max(t), c);
                return;
            }
            self.fill_px_rect(x, y, w, t, c);
            self.fill_px_rect(x, y + h - t, w, t, c);
            let mid_h = (h - 2.0 * t).max(0.0);
            if mid_h > 0.0 {
                self.fill_px_rect(x, y + t, t, mid_h, c);
                self.fill_px_rect(x + w - t, y + t, t, mid_h, c);
            }
            return;
        }
        let (cx, cy, hx, hy) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
        let r = r.min(hx).min(hy);
        let hw = sw / 2.0;
        let (x0, y0, x1, y1) = self.clipped_bounds(
            (x - sw - 1.0).floor() as i32,
            (y - sw - 1.0).floor() as i32,
            (x + w + sw + 1.0).ceil() as i32,
            (y + h + sw + 1.0).ceil() as i32,
        );
        // The rect inset by `t` is ink-free: there `sd_round_box ≤ -t`, so `|sd| - hw ≥ 2`, which
        // `cov` makes 0. Each row skips its span of that inset ([`inset_half_width`]), so a
        // stroke costs its line and its corners, not a band as deep as its radius.
        let t = hw + 2.0;
        let mut memo = RimMemo::default();
        for py in y0..y1 {
            let pyc = py as f32 + 0.5;
            let gap = inset_half_width(pyc - cy, (hx, hy, r), t)
                .map_or(x1..x1, |half| columns_within(cx, half, x0, x1));
            // A row the inset misses, past the straight sides' ends (`qy > 0`), is the same
            // distance from the edge (`qy - r`) at every column between those sides: they take
            // one coverage as one span. The column at each end of it is weighed alone.
            let qy = (pyc - cy).abs() - (hy - r);
            let flat = if gap.is_empty() && qy > 0.0 {
                let run = columns_within(cx, hx - r, x0, x1);
                if run.end - run.start > 2 {
                    run.start + 1..run.end - 1
                } else {
                    x1..x1
                }
            } else {
                x1..x1
            };
            let skip = if gap.is_empty() { flat.clone() } else { gap };
            for px in (x0..skip.start).chain(skip.end..x1) {
                #[cfg(test)]
                STROKE_VISITS.with(|n| n.set(n.get() + 1));
                let d = sd_round_box(px as f32 + 0.5, pyc, cx, cy, hx, hy, r).abs() - hw;
                self.blend(px, py, c, cov(d));
            }
            if !flat.is_empty() {
                self.blend_span(
                    flat.start,
                    flat.end,
                    py,
                    c,
                    cov((qy - r).abs() - hw),
                    &mut memo,
                );
            }
        }
    }

    /// A [`DrawPrim::Dashed`] stroke, its rect, `width` and dash pattern in
    /// device px: the bands and dashes are disjoint device rects, so each
    /// pixel is inked once and a translucent ink stays even at the corners.
    fn dashed(
        &mut self,
        (x, y, w, h): (f32, f32, f32, f32),
        width: f32,
        (on, off): (f32, f32),
        c: [u8; 4],
    ) {
        if on.is_nan() || on <= 0.0 {
            return;
        }
        let dash = Dash {
            on: dash_len(on),
            off: if off > 0.0 { dash_len(off) } else { 0 },
        };
        let t = dash_len(width);
        let (x0, x1) = (dash_px(x.min(x + w)), dash_px(x.max(x + w)));
        let (y0, y1) = (dash_px(y.min(y + h)), dash_px(y.max(y + h)));
        if x1 - x0 > 2 * t && y1 - y0 > 2 * t {
            for (top, bottom) in [(y0, y0 + t), (y1 - t, y1)] {
                self.fill_device((x0, top, x0 + t, bottom), c);
                self.dash_band(dash, x0, (x0 + t, x1 - t), (top, bottom), true, c);
                self.fill_device((x1 - t, top, x1, bottom), c);
            }
            for (left, right) in [(x0, x0 + t), (x1 - t, x1)] {
                self.dash_band(dash, y0, (y0 + t, y1 - t), (left, right), false, c);
            }
        } else if x1 - x0 >= y1 - y0 {
            let band = (y0, y0 + (y1 - y0).max(t));
            self.dash_band(dash, x0, (x0, x1), band, true, c);
        } else {
            let band = (x0, x0 + (x1 - x0).max(t));
            self.dash_band(dash, y0, (y0, y1), band, false, c);
        }
    }

    /// The dashes of `dash` begun at `start`, within `run`, across `band`:
    /// along x over rows `band` when `across`, else along y over columns
    /// `band`. Only the dashes the canvas and clip show are walked.
    fn dash_band(
        &mut self,
        dash: Dash,
        start: i64,
        run: (i64, i64),
        band: (i64, i64),
        across: bool,
        c: [u8; 4],
    ) {
        let (vx0, vy0, vx1, vy1) = self.clipped_bounds(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
        let (lo, hi) = if across {
            (run.0.max(vx0.into()), run.1.min(vx1.into()))
        } else {
            (run.0.max(vy0.into()), run.1.min(vy1.into()))
        };
        for (a, b) in dash.within(start, lo, hi) {
            let rect = if across {
                (a, band.0, b, band.1)
            } else {
                (band.0, a, band.1, b)
            };
            self.fill_device(rect, c);
        }
    }

    /// Ink the device rect `x0..x1 × y0..y1` fully, within the canvas and
    /// the clip.
    fn fill_device(&mut self, (x0, y0, x1, y1): (i64, i64, i64, i64), c: [u8; 4]) {
        let (vx0, vy0, vx1, vy1) = self.clipped_bounds(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
        let fit = |v: i64, lo: i32, hi: i32| v.clamp(lo.into(), hi.into()) as i32;
        let (x0, x1) = (fit(x0, vx0, vx1), fit(x1, vx0, vx1));
        let (y0, y1) = (fit(y0, vy0, vy1), fit(y1, vy0, vy1));
        let mut memo = RimMemo::default();
        for y in y0..y1 {
            self.blend_span(x0, x1, y, c, 1.0, &mut memo);
        }
    }
}

/// A [`DrawPrim::Dashed`] stroke's pattern in whole device px: dashes `on`
/// long, `off` apart (`off == 0` runs solid).
#[derive(Clone, Copy)]
struct Dash {
    on: i64,
    off: i64,
}

impl Dash {
    /// The dashes of the pattern begun at `start`, cut to `lo..hi`, in order.
    fn within(self, start: i64, lo: i64, hi: i64) -> impl Iterator<Item = (i64, i64)> {
        let period = self.on + self.off;
        let first = (lo - start).max(0) / period;
        (first..)
            .map(move |k| start + k * period)
            .take_while(move |&at| at < hi)
            .map(move |at| (at.max(lo), (at + self.on).min(hi)))
            .filter(|(a, b)| a < b)
    }
}

/// The farthest a dashed stroke's places and lengths reach, in device px:
/// past any surface, and small enough that their sums never overflow.
const DASH_REACH: f32 = 16_777_216.0;

/// A dashed stroke's edge `v` (device px) on the nearest whole pixel.
fn dash_px(v: f32) -> i64 {
    v.round().clamp(-DASH_REACH, DASH_REACH) as i64
}

/// A dashed stroke's length `v` (device px) in whole pixels, at least one.
fn dash_len(v: f32) -> i64 {
    v.max(1.0).round().min(DASH_REACH) as i64
}

/// A span `[at, at + len)` on one axis (device px) with both edges on the device-pixel
/// grid, so a straight side fills whole pixels instead of two at part strength; a span
/// that would round to nothing keeps its own extent.
fn device_snapped(at: f32, len: f32) -> (f32, f32) {
    let (start, end) = (at.round(), (at + len).round());
    if end - start >= 1.0 {
        (start, end - start)
    } else {
        (at, len)
    }
}

/// A [`DrawPrim::Panel`]'s rect (points) in device px, its edges on the device grid
/// unless a [`DrawPrim::SubpixelPush`] keeps it where it is (`grid` false).
fn panel_in_device_px(
    (x, y, w, h): (f32, f32, f32, f32),
    s: f32,
    grid: bool,
) -> (f32, f32, f32, f32) {
    if !grid {
        return (x * s, y * s, w * s, h * s);
    }
    let (x, w) = device_snapped(x * s, w * s);
    let (y, h) = device_snapped(y * s, h * s);
    (x, y, w, h)
}

/// A rounded [`Canvas::stroke_rect`]'s rect and line (device px) with its outer edges on
/// the device grid and its line whole pixels, so its straight sides are crisp at any
/// layout x; a square one is returned as is, since its spans already snap.
fn rim_on_grid(
    (x, y, w, h): (f32, f32, f32, f32),
    r: f32,
    width: f32,
) -> (f32, f32, f32, f32, f32) {
    let sw = width.max(1.0);
    if !rounded_stroke(r, w, h, sw) {
        return (x, y, w, h, width);
    }
    let (ox, ow) = device_snapped(x - sw / 2.0, w + sw);
    let (oy, oh) = device_snapped(y - sw / 2.0, h + sw);
    let sw = sw.round().max(1.0);
    (ox + sw / 2.0, oy + sw / 2.0, ow - sw, oh - sw, sw)
}

/// How far either side of its centre the rounded box of half-extents `hx`×`hy` and corner `r`,
/// inset by `t`, reaches on the row `dy` from its centre, or `None` where the row misses it. The
/// inset of a rounded box is exactly the box `t` smaller with corners of `r - t`, square once `t`
/// passes `r`: its distance field is the outer one plus `t`.
fn inset_half_width(dy: f32, (hx, hy, r): (f32, f32, f32), t: f32) -> Option<f32> {
    let (ix, iy, ir) = (hx - t, hy - t, (r - t).max(0.0));
    let dy = dy.abs();
    if ix <= 0.0 || dy > iy {
        return None;
    }
    let qy = dy - (iy - ir);
    Some(if qy <= 0.0 {
        ix
    } else {
        ix - ir + (ir * ir - qy * qy).max(0.0).sqrt()
    })
}

/// The columns of `x0..x1` whose pixel centres lie within `half` of `cx`, or the empty `x1..x1`.
fn columns_within(cx: f32, half: f32, x0: i32, x1: i32) -> std::ops::Range<i32> {
    let start = ((cx - half - 0.5).ceil() as i32).clamp(x0, x1);
    let end = ((cx + half - 0.5).floor() as i32 + 1).clamp(x0, x1);
    if start < end { start..end } else { x1..x1 }
}

/// Whether [`Canvas::stroke_rect`] draws a stroke rounded (by its distance field), centred on its
/// rect's edge; otherwise it draws the stroke inside its rect, on whole device px.
pub(crate) fn rounded_stroke(r: f32, w: f32, h: f32, sw: f32) -> bool {
    r > 0.5 && w > sw * 2.0 + 1.0 && h > sw * 2.0 + 1.0
}

/// The device px a [`crate::widget::DrawPrim::Stroke`] of rect `(x, y, w, h)` inks, `(x, y, w, h)`:
/// a rounded stroke is centred on its rect's edge, an unrounded one drawn inside its rect
/// ([`rounded_stroke`]).
#[cfg(test)]
pub(crate) fn stroke_px(
    (x, y, w, h): (f32, f32, f32, f32),
    radius: f32,
    width: f32,
) -> (f32, f32, f32, f32) {
    if rounded_stroke(radius, w, h, width) {
        let half = width / 2.0;
        (x - half, y - half, w + width, h + width)
    } else {
        (x, y, w, h)
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

    /// Map an outward-rounded logical damage rectangle into a clipped device tile.
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

/// Rasterize the tray `prims` into an RGBA8 buffer of `w*scale × h*scale` over `backdrop`.
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

/// Rasterize `prims` over `pixels`, the RGBA8 `w × h` raster of the prims a list draws before
/// them: the bytes one [`rasterize_tray`] of the whole list gives, while those leave no clip or
/// fade open.
pub(crate) fn rasterize_tray_over(
    prims: &[DrawPrim],
    scale: f32,
    pixels: Vec<u8>,
    w: u32,
    h: u32,
) -> Vec<u8> {
    rasterize_tray_on_canvas(prims, scale, Canvas::over(pixels, w, h))
}

/// Rasterize into an exact device-pixel extent.
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

/// Composite one retained straight-alpha RGBA surface over another, in the linear-light
/// source-over space tray primitives rasterize in.
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
        glyphs: GlyphMemo::default(),
        clip: Vec::new(),
        glyph_remap: (u32::MAX, u32::MAX, [f32::NAN; 256]),
        fade: Vec::new(),
        subpixel: 0,
    };
    // Row by row over the source pixels that land on the destination, as `Canvas::blend` at full
    // cover takes each: a clear pixel leaves it, an opaque run is copied, the rest composite.
    let (source_width, source_height) = source_size;
    let (w, h) = (i64::from(destination_width), i64::from(destination_height));
    let dx = i64::from(source_origin.0) - i64::from(canvas.origin_x);
    let dy = i64::from(source_origin.1) - i64::from(canvas.origin_y);
    let landing = |d: i64, n: u32, room: i64| {
        let n = i64::from(n);
        (-d).clamp(0, n)..(room - d).clamp(0, n)
    };
    let cols = landing(dx, source_width, w);
    let stride = source_width as usize * 4;
    for row in landing(dy, source_height, h) {
        let line = &source[row as usize * stride..][..stride];
        let at = |col: i64| (((dy + row) * w + dx + col) * 4) as usize;
        let mut col = cols.start;
        while col < cols.end {
            let s = col as usize * 4;
            match line[s + 3] {
                0 => col += 1,
                u8::MAX => {
                    let mut end = col + 1;
                    while end < cols.end && line[end as usize * 4 + 3] == u8::MAX {
                        end += 1;
                    }
                    let (i, e) = (at(col), end as usize * 4);
                    canvas.px[i..i + (e - s)].copy_from_slice(&line[s..e]);
                    col = end;
                }
                alpha => {
                    let c = [line[s], line[s + 1], line[s + 2], alpha];
                    canvas.composite_at(at(col), c, 1.0, f32::from(alpha) / 255.0);
                    col += 1;
                }
            }
        }
    }
    *destination = canvas.px;
    true
}

/// Rasterize only `region` of a full retained tray.
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
    with_fonts(|fonts| {
        cv.glyphs = std::mem::take(&mut fonts.glyphs);
        cv.glyphs.begin();
        let mut frames: Vec<(usize, Rc<aterm_render::Frame>)> = Vec::new();
        match occluding_specimen(prims, scale, &cv, &mut frames) {
            Some((cover, covered)) => {
                let tile = cv.device_bounds();
                for (x0, y0, x1, y1) in tile_around(tile, covered) {
                    let (clips, fades, subpixel) = (cv.clip.len(), cv.fade.len(), cv.subpixel);
                    cv.push_clip(x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32);
                    let mut skipped = 0;
                    replay_tray(
                        &mut cv,
                        fonts,
                        prims,
                        0..cover,
                        scale,
                        true,
                        &frames,
                        &mut skipped,
                    );
                    while cv.fade.len() > fades {
                        cv.pop_fade();
                    }
                    cv.clip.truncate(clips);
                    cv.subpixel = subpixel;
                }
                let mut skipped = 0;
                replay_tray(
                    &mut cv,
                    fonts,
                    prims,
                    0..cover,
                    scale,
                    false,
                    &frames,
                    &mut skipped,
                );
                replay_tray(
                    &mut cv,
                    fonts,
                    prims,
                    cover..prims.len(),
                    scale,
                    true,
                    &frames,
                    &mut skipped,
                );
            }
            None => {
                let mut skipped = 0;
                replay_tray(
                    &mut cv,
                    fonts,
                    prims,
                    0..prims.len(),
                    scale,
                    true,
                    &frames,
                    &mut skipped,
                );
            }
        }
        while !cv.fade.is_empty() {
            cv.pop_fade();
        }
        fonts.glyphs = std::mem::take(&mut cv.glyphs);
        fonts.glyphs.trim();
        cv.px
    })
}

#[cfg(test)]
thread_local! {
    /// How many prims [`replay_tray`] drew on this thread: a test's evidence that prims an opaque
    /// specimen covers are not drawn under it.
    pub(crate) static DRAWN_PRIMS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

type DeviceRect = (i32, i32, i32, i32);

/// The prims as GPU tray prims, and the composite law the GPU reads (`tray_gpu_prims.rs`).
#[path = "tray_gpu_prims.rs"]
pub(crate) mod gpu_prims;

fn device_clip_rect(x: f32, y: f32, w: f32, h: f32) -> DeviceRect {
    (
        x.floor() as i32,
        y.floor() as i32,
        (x + w).ceil() as i32,
        (y + h).ceil() as i32,
    )
}

fn occluding_specimen(
    prims: &[DrawPrim],
    scale: f32,
    cv: &Canvas,
    frames: &mut Vec<(usize, Rc<aterm_render::Frame>)>,
) -> Option<(usize, DeviceRect)> {
    let tile = cv.device_bounds();
    let tile_area = i64::from(tile.2 - tile.0) * i64::from(tile.3 - tile.1);
    if tile_area <= 0 {
        return None;
    }
    let intersect =
        |a: DeviceRect, b: DeviceRect| (a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3));
    let mut clips: Vec<DeviceRect> = Vec::new();
    let mut skipped = 0_u32;
    let mut fades = 0_usize;
    let mut best: Option<(usize, DeviceRect, i64)> = None;
    for (index, prim) in prims.iter().enumerate() {
        match prim {
            DrawPrim::ClipPush { .. } if skipped > 0 => skipped += 1,
            DrawPrim::ClipPush { x, y, w, h } => {
                let mut rect = device_clip_rect(x * scale, y * scale, w * scale, h * scale);
                if let Some(&top) = clips.last() {
                    rect = intersect(rect, top);
                }
                clips.push(rect);
                let meets =
                    rect.2 > tile.0 && rect.3 > tile.1 && rect.0 < tile.2 && rect.1 < tile.3;
                if !meets {
                    skipped = 1;
                }
            }
            DrawPrim::ClipPop if skipped > 1 => skipped -= 1,
            DrawPrim::ClipPop if skipped == 1 => {
                skipped = 0;
                clips.pop();
            }
            DrawPrim::ClipPop => {
                clips.pop();
            }
            _ if skipped > 0 => {}
            DrawPrim::FadePush { .. } => fades += 1,
            DrawPrim::FadePop => fades = fades.saturating_sub(1),
            DrawPrim::TerminalSpecimen { x, y, spec } if fades == 0 => {
                let Some(frame) = specimen_frame(spec, scale) else {
                    continue;
                };
                let rows = frame.pixels.len().checked_div(frame.width).unwrap_or(0);
                let (fx, fy) = ((x * scale).round() as i32, (y * scale).round() as i32);
                let mut rect = (
                    fx,
                    fy,
                    fx.saturating_add(i32::try_from(frame.width).unwrap_or(i32::MAX)),
                    fy.saturating_add(i32::try_from(rows).unwrap_or(i32::MAX)),
                );
                if let Some(&top) = clips.last() {
                    rect = intersect(rect, top);
                }
                rect = intersect(rect, tile);
                frames.push((index, frame));
                if rect.0 >= rect.2 || rect.1 >= rect.3 {
                    continue;
                }
                let area = i64::from(rect.2 - rect.0) * i64::from(rect.3 - rect.1);
                if best.is_none_or(|(_, _, most)| area >= most) {
                    best = Some((index, rect, area));
                }
            }
            _ => {}
        }
    }
    best.filter(|(_, _, area)| area * 2 >= tile_area)
        .map(|(index, rect, _)| (index, rect))
}

/// The device rect the occluding specimen covers of `region` ([`occluding_specimen`]), for a test
/// that rasters exactly that rect.
#[cfg(test)]
pub(crate) fn occluded_rect_for_test(
    prims: &[DrawPrim],
    scale: f32,
    region: RasterRect,
) -> Option<DeviceRect> {
    let cv = Canvas::with_origin(
        region.x as i32,
        region.y as i32,
        region.width,
        region.height,
        [0, 0, 0, 0],
    );
    let mut frames = Vec::new();
    occluding_specimen(prims, scale, &cv, &mut frames).map(|(_, rect)| rect)
}

fn tile_around(tile: DeviceRect, covered: DeviceRect) -> impl Iterator<Item = DeviceRect> {
    [
        (tile.0, tile.1, tile.2, covered.1),
        (tile.0, covered.3, tile.2, tile.3),
        (tile.0, covered.1, covered.0, covered.3),
        (covered.2, covered.1, tile.2, covered.3),
    ]
    .into_iter()
    .filter(|rect| rect.0 < rect.2 && rect.1 < rect.3)
}

/// The device pixels the first opaque [`DrawPrim::Panel`] after `prims[at]`, in the unbroken run
/// of panels that follows it within `..end`, fills whole: its rect inset past its corners
/// (`r + 2`), where [`Canvas::round_rect`] paints each row's inner span opaque, replacing what is
/// under it. A translucent panel under it (a card's drop shadow) need not ink them: whatever it
/// inks there is overwritten before anything else draws.
fn covered_by_a_later_panel(
    prims: &[DrawPrim],
    at: usize,
    end: usize,
    s: f32,
    grid: bool,
) -> Option<DeviceRect> {
    for p in prims.iter().take(end).skip(at + 1) {
        let DrawPrim::Panel {
            x,
            y,
            w,
            h,
            radius,
            fill,
        } = p
        else {
            return None;
        };
        if fill[3] < u8::MAX {
            continue;
        }
        let (x, y, w, h) = panel_in_device_px((*x, *y, *w, *h), s, grid);
        let m = (radius * s).min(w / 2.0).min(h / 2.0).max(0.0) + 2.0;
        let hole = (
            (x + m - 0.5).ceil() as i32,
            (y + m - 0.5).ceil() as i32,
            (x + w - m - 0.5).floor() as i32 + 1,
            (y + h - m - 0.5).floor() as i32 + 1,
        );
        return (hole.0 < hole.2 && hole.1 < hole.3).then_some(hole);
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn replay_tray(
    cv: &mut Canvas,
    fonts: &mut ChromeFonts,
    prims: &[DrawPrim],
    range: std::ops::Range<usize>,
    scale: f32,
    draw: bool,
    frames: &[(usize, Rc<aterm_render::Frame>)],
    skipped: &mut u32,
) {
    let s = scale;
    let mut skipped_clip_depth = *skipped;
    for (index, p) in prims.iter().enumerate().take(range.end).skip(range.start) {
        match p {
            DrawPrim::SubpixelPush => {
                cv.subpixel = cv.subpixel.saturating_add(1);
                continue;
            }
            DrawPrim::SubpixelPop => {
                cv.subpixel = cv.subpixel.saturating_sub(1);
                continue;
            }
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
            DrawPrim::FadePush { y, h, from, to } => {
                cv.push_fade(y * s, (y + h) * s, *from, *to, draw);
                continue;
            }
            DrawPrim::FadePop => {
                cv.pop_fade();
                continue;
            }
            _ => {}
        }
        if !draw {
            continue;
        }
        #[cfg(test)]
        DRAWN_PRIMS.with(|drawn| drawn.set(drawn.get() + 1));
        match p {
            DrawPrim::Panel {
                x,
                y,
                w,
                h,
                radius,
                fill,
                ..
            } => {
                let grid = cv.subpixel == 0;
                let (x, y, w, h) = panel_in_device_px((*x, *y, *w, *h), s, grid);
                let hole = (fill[3] < u8::MAX)
                    .then(|| covered_by_a_later_panel(prims, index, range.end, s, grid))
                    .flatten();
                cv.round_rect_under(x, y, w, h, radius * s, *fill, hole);
            }
            DrawPrim::Ring {
                cx,
                cy,
                r_outer,
                thickness,
                track,
                start,
                frac,
                fill,
            } => {
                let r_mid = (r_outer - thickness / 2.0) * s;
                let th = thickness * s;
                cv.arc(cx * s, cy * s, r_mid, th, 0.0, 1.0, *track);
                cv.arc(cx * s, cy * s, r_mid, th, *start, *frac, *fill);
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
            DrawPrim::Dot { cx, cy, r, color } => cv.disc(cx * s, cy * s, r * s, *color),
            DrawPrim::HsvDisk { cx, cy, r, value } => {
                cv.hsv_disk(cx * s, cy * s, r * s, *value);
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
                fonts,
                x * scale,
                baseline * scale,
                txt,
                px * scale,
                *weight,
                *face,
                *color,
            ),
            DrawPrim::TerminalSpecimen { x, y, spec } => {
                match frames.iter().find(|(at, _)| *at == index) {
                    Some((_, frame)) => {
                        // The occluding specimen's frame, prepared once for the whole raster: a
                        // cursor scene card blitted here is the one a capture shows, as
                        // [`Canvas::terminal_specimen`] notes its own.
                        let at = ((x * scale).round() as i32, (y * scale).round() as i32);
                        cv.blit_terminal_frame(frame, at.0, at.1);
                        crate::settings_preview::note_scene_card_painted(spec, scale, at);
                    }
                    None => cv.terminal_specimen(*x, *y, spec, scale),
                }
            }
            DrawPrim::Stroke {
                x,
                y,
                w,
                h,
                radius,
                width,
                color,
            } => {
                let (r, rect) = (radius * s, (x * s, y * s, w * s, h * s));
                let (x, y, w, h, width) = if cv.subpixel == 0 {
                    rim_on_grid(rect, r, width * s)
                } else {
                    (rect.0, rect.1, rect.2, rect.3, width * s)
                };
                cv.stroke_rect(x, y, w, h, r, width, *color);
            }
            DrawPrim::Line {
                x1,
                y1,
                x2,
                y2,
                width,
                color,
            } => cv.line_segment((x1 * s, y1 * s), (x2 * s, y2 * s), width * s, *color),
            DrawPrim::Arc {
                cx,
                cy,
                r,
                width,
                from,
                to,
                color,
            } => cv.arc_band((cx * s, cy * s), (r * s, width * s), (*from, *to), *color),
            DrawPrim::Dashed {
                x,
                y,
                w,
                h,
                width,
                on,
                off,
                color,
            } => cv.dashed(
                (x * s, y * s, w * s, h * s),
                width * s,
                (on * s, off * s),
                *color,
            ),
            DrawPrim::BandIcon {
                x,
                y,
                size,
                icon,
                color,
            } => cv.band_icon(x * s, y * s, size * s, *icon, *color),
            DrawPrim::Image {
                x,
                y,
                w,
                h,
                art,
                alpha,
            } => cv.image((x * s, y * s, w * s, h * s), *art, *alpha),
            DrawPrim::ClipPush { .. }
            | DrawPrim::ClipPop
            | DrawPrim::FadePush { .. }
            | DrawPrim::FadePop
            | DrawPrim::SubpixelPush
            | DrawPrim::SubpixelPop => unreachable!("handled above"),
        }
    }
    *skipped = skipped_clip_depth;
}

/// Copy a tightly packed regional raster into an existing full retained RGBA8 surface.
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
#[path = "tray_raster_tests.rs"]
mod tests;
