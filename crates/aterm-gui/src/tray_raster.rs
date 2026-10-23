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

/// The first slot (`regular`, `bold italic`, `emoji`, …) a [`SemanticFontResolution::Fallback`] status names as unresolved, so the specimen's status names the font that failed rather than the row that has focus.
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

struct SemanticPrewarmQueue {
    semantic_job: Mutex<Option<SemanticPrewarmJob>>,
    ready: Condvar,
}

struct SemanticPrewarmWorker {
    queue: Arc<SemanticPrewarmQueue>,
    results: std::sync::mpsc::Receiver<SemanticPrewarmResult>,
}

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
        let queue = Arc::new(SemanticPrewarmQueue {
            semantic_job: Mutex::new(None),
            ready: Condvar::new(),
        });
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

/// How the chrome turns a char into a glyph of a [`ChromeFont`]: through the face's UNICODE cmap subtables ([`ChromeFont::unicode_glyph_index`]), as the terminal grid draws its primary face, never through `lookup_glyph_index`.
pub(crate) trait UnicodeChars {
    /// Whether the face maps `ch` to a real (non-`.notdef`) glyph.
    fn maps(&self, ch: char) -> bool;
    /// The layout metrics of the glyph `ch` maps to, at `px`.
    fn char_metrics(&self, ch: char, px: f32) -> ChromeMetrics;
    /// The metrics and coverage mask of the glyph `ch` maps to, at `px`.
    fn char_raster(&self, ch: char, px: f32) -> (ChromeMetrics, Vec<u8>);
    /// [`Self::char_raster`] with the pen `dx` px (`0.0..1.0`) right of a whole pixel; `xmin` counts from that whole pixel.
    fn char_raster_at(&self, ch: char, px: f32, dx: f32) -> (ChromeMetrics, Vec<u8>);
    /// The `kern` adjustment between the glyphs `left` and `right` map to.
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

    const SUB: usize = 4;

    /// `ch` at `px`: its coverage bitmap and placement, in the convention every glyph the pen blits uses (`xmin`/`ymin` from the pen and the baseline, rows top-down), or `None` for a char the pen does not draw.
    pub(super) fn rasterize(ch: char, px: f32) -> Option<(ChromeMetrics, Vec<u8>)> {
        let d = design(ch)?;
        let xmin = (d.left * px).floor() as i32;
        let ymin = (d.bottom * px).floor() as i32;
        let top = (d.top * px).ceil() as i32;
        let width = ((d.right * px).ceil() as i32 - xmin).max(0) as usize;
        let height = (top - ymin).max(0) as usize;
        let metrics = ChromeMetrics {
            xmin,
            ymin,
            width,
            height,
            advance_width: d.advance * px,
        };
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

/// Whether `ch` is in one of the East Asian blocks ([`ui_cjk_script`]): text of them breaks between its characters, with no hyphen.
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

/// The chrome's OWN fallback for East Asian text in a UI-face run (Segoe UI's labels: the tab strip's titles, Settings), at the run's weight: the faces Windows itself falls back to from Segoe UI ([`UI_CJK_FILES`]).
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
}

#[derive(Default)]
struct MeasureMemo {
    widths: aterm_hash::FxHashMap<(u16, u32), HeldMeasures<f32>>,
    wraps: aterm_hash::FxHashMap<(u32, u32), HeldMeasures<WrapLines>>,
    lines: aterm_hash::FxHashMap<(LinesRule, u16, u32, u32), HeldMeasures<Vec<String>>>,
    entries: usize,
}

type WrapLines = (Vec<std::ops::Range<usize>>, usize);

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

    fn lines(&self, at: LinesAt, font: &Arc<ChromeFont>, text: &str) -> Option<Vec<String>> {
        let (held, kept) = self.lines.get(&at)?;
        Arc::ptr_eq(held, font)
            .then(|| kept.get(text).cloned())
            .flatten()
    }

    fn keep_lines(&mut self, at: LinesAt, font: &Arc<ChromeFont>, text: &str, lines: &[String]) {
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
        if kept.insert(text.into(), lines.to_vec()).is_none() {
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
}

type LinesAt = (LinesRule, u16, u32, u32);

/// THE LINES `rule` LAYS `text` ON in `face` at `px` in a box `width` wide, as [`MeasureMemo`] keeps a measure: what `lay_out` lays out — which must measure only in `face` at `px` — kept only where the face maps every char of `text`, so the lines depend on that font, `px` and `width` alone.
pub(crate) fn kept_lines(
    rule: LinesRule,
    face: TextFace,
    text: &str,
    px: f32,
    width: f32,
    lay_out: impl FnOnce() -> Vec<String>,
) -> Vec<String> {
    let at = (rule, face_code(face), px.to_bits(), width.to_bits());
    let font = {
        let fonts = lock_fonts();
        let Some(font) = fonts.ui_font(face) else {
            drop(fonts);
            return lay_out();
        };
        if let Some(lines) = fonts.measures.lines(at, font, text) {
            return lines;
        }
        Arc::clone(font)
    };
    let lines = lay_out();
    if text.chars().all(|character| font.maps(character)) {
        lock_fonts().measures.keep_lines(at, &font, text, &lines);
    }
    lines
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

/// Of [`measure_walks`], the strings the UI face mapped every char of: the ones [`MeasureMemo`] can keep, so a layout that finds them kept walks none of them again (test-only).
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

    /// Pure geometry from the already prepared face; no request, worker poll, font lock, fork, or rasterization is needed while laying out a specimen.
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

    /// The `(width, height)` cell a specimen paints at `px`: pure geometry, like [`Self::specimen_cell_height`].
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
fn lock_fonts() -> std::sync::MutexGuard<'static, ChromeFonts> {
    chrome_fonts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
thread_local! {
    static FONT_STORE_VISITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The font-store entries this thread has made so far (test-only; see `FONT_STORE_VISITS`): a render's text work is the difference across it.
#[cfg(test)]
pub(crate) fn font_store_visits() -> u64 {
    FONT_STORE_VISITS.with(std::cell::Cell::get)
}

#[cfg(test)]
thread_local! {
    static LINEAR_COMPOSITES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The linear-light composites this thread has run so far (test-only; see `LINEAR_COMPOSITES`): a raster's blend work is the difference across it.
#[cfg(test)]
pub(crate) fn linear_composites() -> u64 {
    LINEAR_COMPOSITES.with(std::cell::Cell::get)
}

#[cfg(test)]
fn lock_fonts() -> std::sync::MutexGuard<'static, ChromeFonts> {
    thread_local! {
        static FONTS: &'static Mutex<ChromeFonts> =
            Box::leak(Box::new(Mutex::new(default_chrome_fonts())));
    }
    FONT_STORE_VISITS.with(|visits| visits.set(visits.get() + 1));
    FONTS
        .with(|fonts| *fonts)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Install the renderer-resolved chrome faces — `(bytes, collection index)` of the PRIMARY terminal face and its real BOLD sibling (from `Renderer::chrome_primary_face` / `chrome_bold_face`) — so the tray text renders in the user's font.
pub(crate) fn set_chrome_fonts(
    primary: Option<(std::sync::Arc<[u8]>, u32)>,
    bold: Option<(std::sync::Arc<[u8]>, u32)>,
    semantic: Option<Renderer>,
) {
    let (p, b) = (chrome_face_for_slot(&primary), chrome_face_for_slot(&bold));
    reload_chrome_fonts_locked(&mut lock_fonts(), p, b, semantic);
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
    reload_chrome_fonts_locked(
        &mut lock_fonts(),
        prepared.primary,
        prepared.bold,
        prepared.semantic,
    );
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
            warm,
        );
    }
}

fn chrome_coverage_warm(fonts: &ChromeFonts) -> Option<impl FnOnce() -> usize + Send + 'static> {
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
}

/// Test-only: install `renderer` as this test thread's settled semantic chrome font.
#[cfg(test)]
pub(crate) fn install_settled_chrome_fonts_for_test(mut renderer: Renderer) -> u64 {
    renderer.prepare_semantic_typography(
        "Input→present λ π √ ✓ 你好世界 日本語 한글 🚀 😀 🐈\u{200d}⬛ e\u{301}",
    );
    let mut fonts = lock_fonts();
    install_chrome_faces_locked(&mut fonts, None, None, Some(renderer));
    fonts.semantic_prewarm_ms = Some(0);
    fonts.semantic_worker = None;
    fonts.semantic_ready_epoch
}

/// Drive the real request/result channel without a racing worker.
#[cfg(test)]
pub(crate) fn install_pending_chrome_fonts_for_test(
    renderer: Renderer,
) -> PendingChromeFontsForTest {
    let queue = Arc::new(SemanticPrewarmQueue {
        semantic_job: Mutex::new(None),
        ready: Condvar::new(),
    });
    let (tx, results) = std::sync::mpsc::channel();
    let mut fonts = lock_fonts();
    install_chrome_faces_locked(&mut fonts, None, None, Some(renderer));
    fonts.semantic_worker = Some(SemanticPrewarmWorker {
        queue: queue.clone(),
        results,
    });
    PendingChromeFontsForTest { queue, tx }
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
        let mut fonts = lock_fonts();
        if fonts
            .semantic_worker
            .as_ref()
            .is_some_and(|worker| Arc::ptr_eq(&worker.queue, &self.queue))
        {
            fonts.semantic_worker = None;
            fonts.semantic_pending = None;
        }
    }
}

/// Test-only twin of [`install_settled_chrome_fonts_for_test`] that installs `renderer` as production's [`set_chrome_fonts`] does (dormant, no warmup) but parks no worker, so nothing lands through this store later: for a test whose subject is a chain face landing outside the store (the live terminal parsing a cell the seed shares).
#[cfg(test)]
pub(crate) fn install_dormant_chrome_fonts_for_test(renderer: Renderer) -> u64 {
    let mut fonts = lock_fonts();
    install_chrome_faces_locked(&mut fonts, None, None, Some(renderer));
    fonts.semantic_worker = None;
    fonts.semantic_ready_epoch
}

/// The broad CJK face the host's built-in fallback chain leads with, where it is installed — a file `aterm_render` parses, so a test draws with a real ideograph-bearing face rather than a stub.
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

/// Install a SETTLED semantic chrome font whose chain holds the host's CJK face, already parsed — the state the store reaches once the terminal has drawn an ideograph — plus the host UI faces.
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

/// Install, DORMANT, a semantic seed whose chain holds the host's CJK face UNPARSED — a fresh window's state before anything drew an ideograph: the window's own order (configure, seal, fork the seed) with the cell shared between the sealed generation and the seed — plus the host UI faces.
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

/// Whether THIS test thread's chrome cascade draws `ch` from a face it holds parsed (the terminal stack and the coverage rung, [`ChromeFonts::face_for`]), read without parsing anything.
#[cfg(test)]
pub(crate) fn chrome_cascade_reaches_for_test(ch: char) -> bool {
    lock_fonts().face_for(TextWeight::Regular, ch).is_some()
}

/// Whether THIS test thread's chrome draws `ch` in a run set in `face` from a face it holds parsed: the UI faces' own East Asian fallback at `face`'s weight, then the terminal cascade ([`ChromeFonts::ui_miss_face`]), read without parsing anything.
#[cfg(all(test, not(target_os = "macos")))]
pub(crate) fn chrome_ui_reaches_for_test(face: TextFace, ch: char) -> bool {
    lock_fonts()
        .ui_miss_face(face, TextWeight::Regular, ch)
        .is_some()
}

/// Test-only generation token for the current libtest worker's chrome-font context.
#[cfg(test)]
pub(crate) fn chrome_font_epoch_for_test() -> u64 {
    lock_fonts().semantic_ready_epoch
}

/// Direct semantic-view tests build no renderer backend, so they install the immutable proportional faces backend startup would, keeping [`default_chrome_fonts`] cold without replacing an installed semantic renderer.
#[cfg(test)]
pub(crate) fn prepare_ui_fonts_for_direct_view_test() {
    let ui = prepared_ui_font_assets();
    let mut fonts = lock_fonts();
    fonts.ui_regular = ui.regular.clone();
    fonts.ui_semibold = ui.semibold.clone();
    if fonts.ui_cjk.is_none() {
        fonts.set_ui_cjk(ui_cjk_store_for_install());
    }
}

/// Replace THIS test thread's East Asian UI fallback ([`UiCjkFaces`]): `None` for a test whose subject is the terminal chain's rung under it (the state of a host with no such faces), or a cold store of its own.
#[cfg(all(test, not(target_os = "macos")))]
pub(crate) fn set_ui_cjk_faces_for_test(store: Option<Arc<UiCjkFaces>>) {
    lock_fonts().set_ui_cjk(store);
}

/// A cold East Asian UI fallback store of this host's faces, nothing parsed; `None` where the host has none (every host but Windows).
#[cfg(all(test, windows))]
pub(crate) fn host_ui_cjk_faces_for_test() -> Option<Arc<UiCjkFaces>> {
    UiCjkFaces::host().map(Arc::new)
}

/// The inverse seam: return this test thread's chrome stack to the cold, UI-face-less store.
#[cfg(test)]
pub(crate) fn clear_ui_fonts_for_test() {
    let mut fonts = lock_fonts();
    fonts.ui_regular = None;
    fonts.ui_semibold = None;
    fonts.set_ui_cjk(None);
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
    semantic_font_snapshot_locked(&lock_fonts(), candidate)
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
    let mut fonts = lock_fonts();
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

/// The width of `s` at `px` in the chrome stack: real advances (per-char face pick and fixed-point accumulation, the arithmetic the raster pen uses).
pub(crate) fn measure_text(s: &str, px: f32, weight: TextWeight) -> f32 {
    let mut fonts = lock_fonts();
    let mut pen_q: i64 = 0;
    for ch in s.chars() {
        if let Some(face) = fonts.face_for(weight, ch) {
            pen_q += px_to_q(face.advance_em(ch) * px);
        } else if let Some(advance) = synthesized::advance(ch, px) {
            pen_q += px_to_q(advance);
        }
    }
    q_to_px(pen_q)
}

fn chrome_cap_ratio() -> f32 {
    let fonts = lock_fonts();
    fonts
        .primary
        .as_ref()
        .or(fonts.fallback.as_ref())
        .map_or(0.7, |f| f.cap_ratio)
}

/// The cap-height-centred baseline for a `size`-px run in the row `y0, y0 + row_h)`: `row_center + cap_height/2` (the leading-trim rule), via the machine-checked fixed-point law `aterm_render::chrome_metrics::baseline_in_row_q`; top and bottom gaps balance within 1px for any `(row_h, cap)` with `cap <= row_h`.
pub(crate) fn row_baseline(y0: f32, row_h: f32, size: f32) -> f32 {
    let cap_q = px_to_q(size * chrome_cap_ratio());
    q_to_px(baseline_in_row_q(px_to_q(y0), px_to_q(row_h), cap_q))
}

#[derive(Clone, Default)]
struct UiFontAssets {
    regular: Option<Arc<ChromeFont>>,
    semibold: Option<Arc<ChromeFont>>,
    #[cfg(not(target_os = "macos"))]
    variable_semibold: Option<UiVariableSemibold>,
}

/// The variable UI face instanced at semibold, for the one painter that instances a variation itself (the Windows strip band's active label, [`crate::tab_bar::pixel_band`]).
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

/// Install, as this test thread's UI faces, the first pair of the Linux ladder ([`LINUX_UI_FONT_PAIRS`]) whose files both parse, so a test can judge a layout's fit with Linux metrics on any host.
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
        let mut fonts = lock_fonts();
        fonts.ui_regular = Some(regular_face);
        fonts.ui_semibold = Some(semibold_face);
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

/// Populate the two process-global font `OnceLock`s [`set_chrome_fonts`] would otherwise initialise on whichever thread calls it first: the system UI face parse ([`prepared_ui_font_assets`]) and the embedded default store ([`chrome_fonts`]).
#[cfg(not(test))]
pub(crate) fn warm_chrome_font_assets() {
    let _ = prepared_ui_font_assets();
    drop(lock_fonts());
}

/// Tests keep chrome fonts per thread, so a process-global warm means nothing there.
#[cfg(test)]
pub(crate) fn warm_chrome_font_assets() {}

/// The pixel tab strip's font-readiness fingerprint (the Windows + Linux band, [`crate::tab_bar::pixel_band`]): the chrome-face install epoch with the coverage rung's size and the UI regular's presence folded in.
pub(crate) fn strip_band_font_epoch() -> u64 {
    let mut fonts = lock_fonts();
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
    lock_fonts().ui_regular.is_some()
}

/// The variable semibold instance for the strip band's active label, or `None` when the host's UI regular is static (the caller then draws `TextFace::UiBold` through the `ChromeFont` path: the static `seguisb.ttf` sibling, or the regular).
#[cfg(not(target_os = "macos"))]
pub(crate) fn strip_band_variable_semibold() -> Option<UiVariableSemibold> {
    let installed = lock_fonts().ui_regular.clone()?;
    prepared_ui_font_assets()
        .variable_semibold
        .clone()
        .filter(|variable| Arc::ptr_eq(&variable.cmap, &installed))
}

/// Can the chrome stack draw EVERY char of `s` as real ink — the UI face first, then the terminal cascade ([`select_chrome_face`]: real bold sibling / user primary / embedded DejaVu), then the renderer's already-parsed chain faces ([`ChromeFonts::coverage`]), then the pen's own two glyphs ([`synthesized`])? This is exactly the per-char routing [`Canvas::text`]'s proportional arm performs, asked ahead of time: a char that fails ALL of them is one the pen would silently skip (advance-only), which for a tab TITLE means a hole where a glyph should be.
pub(crate) fn strip_band_run_coverable(s: &str) -> bool {
    strip_band_run_coverable_in(TextFace::Ui, s)
}

/// [`strip_band_run_coverable`] for a label set in `face`: the faces parsed for its East Asian text are `face`'s weight's ([`UiCjkWeight::of`]).
pub(crate) fn strip_band_run_coverable_in(face: TextFace, s: &str) -> bool {
    let (ui_cjk, coverage, wanted) = {
        let mut fonts = lock_fonts();
        if fonts.ui_regular.is_none() {
            return false;
        }
        let misses = fonts.label_misses(face, s);
        if misses.is_empty() {
            return true;
        }
        if !misses.iter().copied().all(chain_may_draw_in_a_label) {
            return false;
        }
        (
            fonts.ui_cjk.clone(),
            fonts.semantic.as_ref().map(Renderer::fallback_coverage),
            misses.into_iter().collect::<String>(),
        )
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
    lock_fonts().label_misses(face, s).is_empty()
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

/// One colour emoji the chrome draws inline in a label (the tab strip's titles, `tab_bar::pixel_band`): the colour face's own straight-alpha RGBA, `width × height`, at the size it was asked for.
#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug)]
pub(crate) struct ChromeEmoji {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) rgba: Vec<u8>,
}

/// The colour glyph of the emoji grapheme `cluster` at `px` device px, from the chrome stack's semantic renderer: the terminal's own font set, its colour face included, so a tab title draws the emoji the grid draws.
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
    let generation = lock_fonts().semantic_generation;
    MEMO.with_borrow_mut(|memo| {
        if memo
            .as_ref()
            .is_none_or(|(held, bits, _, _)| *held != generation || *bits != px.to_bits())
        {
            let fork = lock_fonts()
                .semantic
                .as_ref()?
                .fork_semantic_surface(px, aterm_render::Theme::default())?;
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

/// Slight negative tracking (em) per glyph advance of the UI face, which reads a touch loose at panel sizes.
pub(crate) const UI_TRACKING_EM: f32 = -0.008;

const PEN_PHASES: f32 = 4.0;

fn pen_phase(pen: f32) -> (i32, u8) {
    let quarters = (pen * PEN_PHASES).round();
    let whole = (quarters / PEN_PHASES).floor();
    let phase = (quarters - whole * PEN_PHASES).clamp(0.0, PEN_PHASES - 1.0);
    (whole as i32, phase as u8)
}

/// Width of `s` at `px` in the UI face: real advances plus `horizontal_kern` plus the [`UI_TRACKING_EM`] the raster pen applies.
pub(crate) fn ui_text_width(s: &str, px: f32) -> f32 {
    ui_text_width_for(TextFace::Ui, s, px)
}

fn ui_text_wrap_ranges_impl(
    text: &str,
    px: f32,
    maximum_width: f32,
) -> (Vec<std::ops::Range<usize>>, usize) {
    let maximum_width = maximum_width.max(1.0);
    {
        let fonts = lock_fonts();
        if let Some(font) = fonts.ui_font(TextFace::Ui)
            && let Some(lines) = fonts.measures.wrap(font, px, maximum_width, text)
        {
            return lines;
        }
    }
    let (lines, font) = ui_text_wrap_walk(text, px, maximum_width);
    if let Some(font) = font {
        lock_fonts()
            .measures
            .keep_wrap(&font, px, maximum_width, text, &lines);
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

    let mut fonts = lock_fonts();
    let font = fonts.ui_font(TextFace::Ui).cloned();
    let ascii_slot = |character: char| {
        u8::try_from(character)
            .ok()
            .filter(u8::is_ascii)
            .map(usize::from)
    };
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
            Some(_) => (
                fonts
                    .ui_miss_face(TextFace::Ui, TextWeight::Regular, character)
                    .map_or_else(
                        || synthesized::advance(character, px).unwrap_or(px * 0.6),
                        |cface| cface.font.char_metrics(character, px).advance_width,
                    ),
                false,
            ),
        };
        match ascii_slot(character) {
            Some(slot) => ascii[slot] = Some((advance, ui_covers)),
            None => {
                advances.insert(character, (advance, ui_covers));
            }
        }
    }
    drop(fonts);
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

/// Exact, whitespace-preserving UI-face line breaks: after white space, or between CJK characters with kinsoku kept ([`aterm_grapheme::line_breaks`]), else inside a token too long for the line.
pub(crate) fn ui_text_wrap_ranges(
    text: &str,
    px: f32,
    maximum_width: f32,
) -> Vec<std::ops::Range<usize>> {
    ui_text_wrap_ranges_impl(text, px, maximum_width).0
}

/// Face-aware proportional measure.
pub(crate) fn ui_text_width_for(face: TextFace, s: &str, px: f32) -> f32 {
    let mut fonts = lock_fonts();
    if let Some(font) = fonts.ui_font(face)
        && let Some(width) = fonts.measures.width(face, font, px, s)
    {
        return width;
    }
    let font = fonts.ui_font(face).cloned();
    let (width, _, whole) = ui_text_width_walk(&mut fonts, face, s, px, 0.0);
    if whole && let Some(font) = font.as_ref() {
        fonts.measures.keep_width(face, font, px, s, width);
    }
    width
}

/// The installed font [`ui_text_width_for`] measures `face` in, or `None` with no UI face installed (each scalar then measures 0.6 em).
pub(crate) fn ui_font_of(face: TextFace) -> Option<Arc<ChromeFont>> {
    lock_fonts().ui_font(face).cloned()
}

/// Whether every scalar in any substring advances at least `minimum` pixels under the same routing, tracking and kerning used by the proportional pen.
pub(crate) fn ui_text_advances_at_least(face: TextFace, s: &str, px: f32, minimum: f32) -> bool {
    ui_text_width_with_floor(&mut lock_fonts(), face, s, px, minimum).1
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
        let advance = fonts
            .ui_miss_face(face, TextWeight::Regular, ch)
            .map_or_else(
                || synthesized::advance(ch, px).unwrap_or(px * 0.6),
                |cface| cface.font.char_metrics(ch, px).advance_width,
            );
        self.w += advance;
        fits(advance, 0.0)
    }
}

/// What [`ui_text_width_for`] measures `text[..cut] + tail` at, for each of `cuts` (ascending byte offsets into `text`, each on a char boundary), in ONE pass of the pen: an elider that measured each shorter candidate afresh paid a whole measure per grapheme it shed.
pub(crate) fn ui_text_widths_at(
    face: TextFace,
    text: &str,
    cuts: &[usize],
    tail: &str,
    px: f32,
) -> Vec<f32> {
    let mut fonts = lock_fonts();
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
                pen.push(&mut fonts, &f, face, ch, px, &any);
                chars.next();
            }
            let mut tailed = pen;
            for ch in tail.chars() {
                tailed.push(&mut fonts, &f, face, ch, px, &any);
            }
            tailed.w
        })
        .collect()
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

fn linear_to_srgb_u8(l: f32) -> u8 {
    const BUCKETS: usize = 8_192;
    struct Quantizer {
        thresholds: [f32; 255],
        coarse: [u8; BUCKETS + 1],
    }

    let l = l.clamp(0.0, 1.0);
    static QUANTIZER: OnceLock<Quantizer> = OnceLock::new();
    let quantizer = QUANTIZER.get_or_init(|| {
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

type GlyphCacheKey = (usize, u32, char, u8);
type GlyphCacheEntry = (ChromeMetrics, std::sync::Arc<[u8]>);

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
    glyphs: std::collections::HashMap<GlyphCacheKey, GlyphCacheEntry>,
    clip: Vec<(i32, i32, i32, i32)>,
    glyph_remap: (u32, u32, [f32; 256]),
    fade: Vec<FadeLayer>,
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

    fn with_origin(origin_x: i32, origin_y: i32, w: u32, h: u32, backdrop: [u8; 4]) -> Self {
        let mut px = vec![0u8; (w * h * 4) as usize];
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
            glyph_remap: (u32::MAX, u32::MAX, [f32::NAN; 256]),
            fade: Vec::new(),
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
        font: &ChromeFont,
        character: char,
        px: f32,
    ) -> (ChromeMetrics, std::sync::Arc<[u8]>) {
        self.raster_glyph_at(font, character, px, 0)
    }

    fn raster_glyph_at(
        &mut self,
        font: &ChromeFont,
        character: char,
        px: f32,
        phase: u8,
    ) -> (ChromeMetrics, std::sync::Arc<[u8]>) {
        let key = (
            font as *const ChromeFont as usize,
            px.to_bits(),
            character,
            phase,
        );
        if let Some((metrics, bitmap)) = self.glyphs.get(&key) {
            return (*metrics, std::sync::Arc::clone(bitmap));
        }
        let (metrics, bitmap) = if phase == 0 {
            font.char_raster(character, px)
        } else {
            font.char_raster_at(character, px, f32::from(phase) / PEN_PHASES)
        };
        let bitmap: std::sync::Arc<[u8]> = bitmap.into();
        self.glyphs
            .insert(key, (metrics, std::sync::Arc::clone(&bitmap)));
        (metrics, bitmap)
    }

    fn raster_synthesized(
        &mut self,
        character: char,
        px: f32,
    ) -> Option<(ChromeMetrics, std::sync::Arc<[u8]>)> {
        let key = (0, px.to_bits(), character, 0);
        if let Some((metrics, bitmap)) = self.glyphs.get(&key) {
            return Some((*metrics, std::sync::Arc::clone(bitmap)));
        }
        let (metrics, bitmap) = synthesized::rasterize(character, px)?;
        let bitmap: std::sync::Arc<[u8]> = bitmap.into();
        self.glyphs
            .insert(key, (metrics, std::sync::Arc::clone(&bitmap)));
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
        // A fully covered opaque source replaces the destination exactly. The
        // general linear-light source-over equation reduces to this copy, so do
        // not pay three lookup/searches for the large opaque surfaces used by
        // native views and modal cards.
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
                let input = scene.decorate(input, renderer.cell_size());
                let mut frame = renderer.try_render_input(&input).ok()?;
                scene.post_process(&mut frame, &input).ok()?;
                frame
            }
            None => renderer.try_render_input(input).ok()?,
        })
    })
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

/// One specimen frame as [`Canvas::terminal_specimen`] renders it, bypassing the frame memo: the shipping renderer's own output for `spec` at `scale` (on this thread's configured fork), the cursor scene played on that renderer's cells, for tests that compare the tray blit against it.
#[cfg(test)]
pub(crate) fn render_specimen_frame_for_test(
    spec: &TerminalSpecimenSpec,
    scale: f32,
) -> Option<aterm_render::Frame> {
    render_specimen_frame(spec, scale)
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
                self.blend_span(start, end, py, c, 1.0, &mut rim);
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
        let side = n as f32;
        let cap = (side * 0.68).round();
        let metrics = aterm_render::procedural::IconMetrics {
            baseline: (side * 0.84).round(),
            cap,
            stem: (side / 9.0).round().max(1.0),
            head: (side * 0.08).round(),
            disc: cap,
        };
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
                    let a = dx.atan2(-dy).rem_euclid(TAU) / TAU;
                    let d_frac = if a <= frac {
                        // Distinct branch.
                        -(a.min(frac - a)) // inside (negative)
                    } else {
                        // Distinct branch.
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
                    None => match self.raster_synthesized(ch, px) {
                        Some(own) => own,
                        None => continue,
                    },
                };
                let (m, bitmap) = glyph;
                if m.width > 0 && m.height > 0 {
                    let gx = q_round_to_px(pen_q) as i32 + m.xmin;
                    let gy = baseline_px - (m.height as i32 + m.ymin);
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
            if let Some(font) = ui.as_deref().filter(|font| font.maps(ch)) {
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
        font: &ChromeFont,
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
        let rounded = r > 0.5 && w > sw * 2.0 + 1.0 && h > sw * 2.0 + 1.0;
        if !rounded {
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

/// Composite one retained straight-alpha RGBA surface over another in the same linear-light source-over space as tray primitive rasterization.
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
        glyph_remap: (u32::MAX, u32::MAX, [f32::NAN; 256]),
        fade: Vec::new(),
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
    let mut fonts = lock_fonts();
    let mut frames: Vec<(usize, Rc<aterm_render::Frame>)> = Vec::new();
    match occluding_specimen(prims, scale, &cv, &mut frames) {
        Some((cover, covered)) => {
            let tile = cv.device_bounds();
            for (x0, y0, x1, y1) in tile_around(tile, covered) {
                let (clips, fades) = (cv.clip.len(), cv.fade.len());
                cv.push_clip(x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32);
                let mut skipped = 0;
                replay_tray(
                    &mut cv,
                    &mut fonts,
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
            }
            let mut skipped = 0;
            replay_tray(
                &mut cv,
                &mut fonts,
                prims,
                0..cover,
                scale,
                false,
                &frames,
                &mut skipped,
            );
            replay_tray(
                &mut cv,
                &mut fonts,
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
                &mut fonts,
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
    cv.px
}

#[cfg(test)]
thread_local! {
    /// How many prims [`replay_tray`] drew on this thread: a test's evidence that the prims an opaque specimen covers were not drawn under it.
    pub(crate) static DRAWN_PRIMS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

type DeviceRect = (i32, i32, i32, i32);

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

/// The device rect the occluding specimen covers of `region` ([`occluding_specimen`]), for a test that rasters exactly that rect.
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

#[allow(clippy::too_many_arguments)]
fn replay_tray(
    cv: &mut Canvas,
    fonts: &mut std::sync::MutexGuard<'static, ChromeFonts>,
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
            } => cv.round_rect(x * s, y * s, w * s, h * s, radius * s, *fill),
            DrawPrim::Ring {
                cx,
                cy,
                r_outer,
                thickness,
                track,
                frac,
                fill,
            } => {
                let r_mid = (r_outer - thickness / 2.0) * s;
                let th = thickness * s;
                cv.arc(cx * s, cy * s, r_mid, th, 1.0, *track);
                cv.arc(cx * s, cy * s, r_mid, th, *frac, *fill);
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
                    Some((_, frame)) => cv.blit_terminal_frame(
                        frame,
                        (x * scale).round() as i32,
                        (y * scale).round() as i32,
                    ),
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
            } => cv.stroke_rect(x * s, y * s, w * s, h * s, radius * s, width * s, *color),
            DrawPrim::Line {
                x1,
                y1,
                x2,
                y2,
                width,
                color,
            } => cv.line_segment((x1 * s, y1 * s), (x2 * s, y2 * s), width * s, *color),
            DrawPrim::BandIcon {
                x,
                y,
                size,
                icon,
                color,
            } => cv.band_icon(x * s, y * s, size * s, *icon, *color),
            DrawPrim::ClipPush { .. }
            | DrawPrim::ClipPop
            | DrawPrim::FadePush { .. }
            | DrawPrim::FadePop => unreachable!("handled above"),
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
mod tests {
    use super::*;
    use crate::widget::DrawPrim;
    use aterm_render::Theme;

    #[test]
    fn a_span_blends_byte_for_byte_as_each_of_its_pixels() {
        let ground = |w: u32, h: u32| {
            let mut px = Vec::with_capacity((w * h * 4) as usize);
            for i in 0..w * h {
                let run = (i / 7) % 5;
                px.extend_from_slice(&match run {
                    0 => [0, 0, 0, 0],
                    1 => [30, 40, 50, 255],
                    2 => [200, 10, 90, 128],
                    3 => [(i % 251) as u8, (i % 13) as u8 * 19, 7, (i % 256) as u8],
                    _ => [255, 255, 255, 64],
                });
            }
            px
        };
        let colours = [
            [10, 20, 30, 255],
            [250, 250, 250, 40],
            [30, 40, 50, 128],
            [0, 0, 0, 1],
            [123, 45, 67, 0],
        ];
        let (w, h) = (64_u32, 6_u32);
        for origin in [(0, 0), (-5, -2), (17, 3)] {
            for clip in [
                None,
                Some((origin.0 + 9, origin.1 + 1, origin.0 + 50, origin.1 + 5)),
            ] {
                for (c, coverage) in colours
                    .into_iter()
                    .flat_map(|c| [1.0_f32, 0.999, 0.4, 0.0].map(|coverage| (c, coverage)))
                {
                    for (x0, x1) in [(0, 64), (-10, 30), (12, 13), (40, 90), (30, 30)] {
                        let fresh = || {
                            let mut cv = Canvas::with_origin(origin.0, origin.1, w, h, [0; 4]);
                            cv.px = ground(w, h);
                            if let Some(clip) = clip {
                                cv.clip.push(clip);
                            }
                            cv
                        };
                        let (mut span, mut each) = (fresh(), fresh());
                        for y in origin.1 - 1..origin.1 + h as i32 + 1 {
                            span.blend_span(
                                origin.0 + x0,
                                origin.0 + x1,
                                y,
                                c,
                                coverage,
                                &mut RimMemo::default(),
                            );
                            for x in origin.0 + x0..origin.0 + x1 {
                                each.blend(x, y, c, coverage);
                            }
                        }
                        assert!(
                            span.px == each.px,
                            "origin {origin:?} clip {clip:?} colour {c:?} at {coverage} span {x0}..{x1}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_rounded_box_draws_the_bytes_its_pixels_drew_one_by_one() {
        fn reference(cv: &mut Canvas, x: f32, y: f32, w: f32, h: f32, r: f32, c: [u8; 4]) {
            let (cx, cy, hx, hy) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
            let r = r.min(hx).min(hy);
            let (x0, y0, x1, y1) = cv.clipped_bounds(
                (x - 1.0).floor() as i32,
                (y - 1.0).floor() as i32,
                (x + w + 1.0).ceil() as i32,
                (y + h + 1.0).ceil() as i32,
            );
            let inset = 1.0_f32;
            let (inner_w, inner_h) = (w - inset * 2.0, h - inset * 2.0);
            let inner_r = (r - inset).max(0.0).min(inner_w / 2.0).min(inner_h / 2.0);
            let (inner_x, inner_y) = (x + inset, y + inset);
            for py in y0..y1 {
                let pyc = py as f32 + 0.5;
                let span =
                    (inner_w > 0.0 && inner_h > 0.0 && pyc >= inner_y && pyc <= inner_y + inner_h)
                        .then(|| {
                            let (left, right) = if inner_r <= 0.0
                                || (pyc >= inner_y + inner_r && pyc <= inner_y + inner_h - inner_r)
                            {
                                (inner_x, inner_x + inner_w)
                            } else {
                                let corner = if pyc < inner_y + inner_r {
                                    inner_y + inner_r
                                } else {
                                    inner_y + inner_h - inner_r
                                };
                                let dy = (pyc - corner).abs();
                                let e = (inner_r * inner_r - dy * dy).max(0.0).sqrt();
                                (inner_x + inner_r - e, inner_x + inner_w - inner_r + e)
                            };
                            let start = ((left - 0.5).ceil() as i32).clamp(x0, x1);
                            let end =
                                (((right - 0.5).floor() as i32).saturating_add(1)).clamp(x0, x1);
                            (start < end).then_some((start, end))
                        })
                        .flatten();
                for px in x0..x1 {
                    let inside = span.is_some_and(|(start, end)| px >= start && px < end);
                    let coverage = if inside {
                        1.0
                    } else {
                        cov(sd_round_box(px as f32 + 0.5, pyc, cx, cy, hx, hy, r))
                    };
                    cv.blend(px, py, c, coverage);
                }
            }
        }
        let (w, h) = (90_u32, 60_u32);
        let ground = |origin: (i32, i32)| {
            let mut cv = Canvas::with_origin(origin.0, origin.1, w, h, [0; 4]);
            for (n, pixel) in cv.px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let band = (n as u32 / w) / 9 + (n as u32 % w) / 23;
                *pixel = match band % 4 {
                    0 => [0, 0, 0, 0],
                    1 => [24, 28, 36, 255],
                    2 => [(n % 256) as u8, 90, (n % 7) as u8 * 30, 200],
                    _ => [240, 240, 245, 96],
                };
            }
            cv
        };
        let boxes = [
            (3.3_f32, 2.6_f32, 70.4_f32, 40.2_f32, 9.5_f32),
            (-4.5, 10.0, 50.0, 30.0, 0.0),
            (20.25, -3.75, 30.5, 70.0, 15.0),
            (10.0, 10.0, 2.5, 2.5, 1.0),
            (40.7, 30.1, 60.0, 8.0, 4.0),
            (5.0, 20.3, 80.0, 1.0, 0.0),
            (2.5, 33.6, 70.0, 2.0, 1.0),
            (-3.2, 44.5, 100.0, 0.5, 0.25),
            (30.0, 5.0, 1.0, 50.0, 0.0),
        ];
        let colours = [[250, 250, 250, 153], [12, 14, 16, 255], [90, 160, 255, 30]];
        for origin in [(0, 0), (-6, 4), (11, -3)] {
            for clip in [
                None,
                Some((origin.0 + 7, origin.1 + 5, origin.0 + 70, origin.1 + 52)),
            ] {
                for &(x, y, bw, bh, r) in &boxes {
                    for c in colours {
                        let fresh = || {
                            let mut cv = ground(origin);
                            if let Some(clip) = clip {
                                cv.clip.push(clip);
                            }
                            cv
                        };
                        let (mut fast, mut slow) = (fresh(), fresh());
                        let (x, y) = (x + origin.0 as f32, y + origin.1 as f32);
                        fast.round_rect(x, y, bw, bh, r, c);
                        reference(&mut slow, x, y, bw, bh, r, c);
                        assert!(
                            fast.px == slow.px,
                            "origin {origin:?} clip {clip:?} box {:?} colour {c:?}",
                            (x, y, bw, bh, r)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_translucent_panel_over_a_flat_one_composites_a_handful_of_times() {
        let panel = |x: f32, y: f32, w: f32, h: f32, fill: [u8; 4]| DrawPrim::Panel {
            x,
            y,
            w,
            h,
            radius: 10.0,
            fill,
        };
        let prims = [
            panel(0.0, 0.0, 640.0, 440.0, [40, 44, 52, 230]),
            panel(20.0, 20.0, 600.0, 400.0, [250, 250, 250, 153]),
        ];
        let before = linear_composites();
        let rgba = rasterize_tray_pixels(&prims, 640, 440, 1.0, [0, 0, 0, 0]);
        let spent = linear_composites() - before;
        assert_eq!(rgba.len(), 640 * 440 * 4);
        assert!(
            spent < 100,
            "{spent} linear composites for two panels of 280,000 and 240,000 pixels"
        );
        let mut cv = Canvas::new(1, 1, [0; 4]);
        cv.blend(0, 0, [40, 44, 52, 230], 1.0);
        cv.blend(0, 0, [250, 250, 250, 153], 1.0);
        let middle = ((220 * 640 + 320) * 4) as usize;
        assert_eq!(rgba[middle..middle + 4], cv.px[..4]);
    }

    #[test]
    fn a_word_the_ui_face_draws_whole_is_measured_once() {
        prepare_ui_fonts_for_direct_view_test();
        assert!(
            ui_font_of(TextFace::Ui).is_some(),
            "a UI face to measure in"
        );
        let (word, px) = ("Searchable scrollback lines", 13.0);
        let first = ui_text_width_for(TextFace::Ui, word, px);
        let walks = measure_walks();
        assert_eq!(ui_text_width_for(TextFace::Ui, word, px), first);
        assert_eq!(measure_walks(), walks, "kept");
        let paragraph = "Find searches this many recent lines; 0 searches only the screen.";
        let lines = ui_text_wrap_ranges(paragraph, px, 120.0);
        let walks = measure_walks();
        assert_eq!(ui_text_wrap_ranges(paragraph, px, 120.0), lines);
        assert_eq!(measure_walks(), walks, "kept");
        let mixed = "\u{8a2d}\u{5b9a} Settings";
        let _ = ui_text_width_for(TextFace::Ui, mixed, px);
        let walks = measure_walks();
        let _ = ui_text_width_for(TextFace::Ui, mixed, px);
        assert_eq!(measure_walks(), walks + 1, "never kept");
        let held = lock_fonts().ui_regular.clone();
        let other = ChromeFace::from_bytes(aterm_render::embedded_font(), 0)
            .map(|face| face.font)
            .expect("the embedded face");
        lock_fonts().ui_regular = Some(other);
        let walks = measure_walks();
        let _ = ui_text_width_for(TextFace::Ui, word, px);
        assert_eq!(measure_walks(), walks + 1, "a new font measures afresh");
        lock_fonts().ui_regular = held;
    }

    #[test]
    fn the_settings_preview_resolves_the_bundled_default_family() {
        use aterm_render::bundled;
        let mut families = Vec::new();
        if bundled::ACTIVE {
            families.push((
                bundled::PRIMARY_FAMILY,
                bundled::PRIMARY_ID,
                bundled::PRIMARY_BOLD_ID,
            ));
            families.push((
                bundled::JETBRAINS_FAMILY,
                bundled::JETBRAINS_ID,
                bundled::JETBRAINS_BOLD_ID,
            ));
        }
        if bundled::CASCADIA_ACTIVE {
            families.push((
                bundled::CASCADIA_FAMILY,
                bundled::CASCADIA_ID,
                bundled::CASCADIA_BOLD_ID,
            ));
        }
        if bundled::IOSEVKA_ACTIVE {
            families.push((
                bundled::IOSEVKA_FAMILY,
                bundled::IOSEVKA_ID,
                bundled::IOSEVKA_BOLD_ID,
            ));
        }
        let mut cases = Vec::new();
        for (family, id, bold_id) in families {
            if aterm_render::resolve_font_family(family, aterm_render::FaceTarget::REGULAR)
                .is_none()
            {
                cases.push((family.to_string(), id));
                cases.push((format!("{family} Bold"), bold_id));
            }
        }
        let mut cache = CandidateFaceCache::new();
        for (family, id) in cases {
            let family = family.as_str();
            let face =
                resolve_candidate_face(family, aterm_render::FaceTarget::REGULAR, &mut cache)
                    .unwrap_or_else(|error| panic!("{family}: {error}"));
            assert_eq!(face.face.path, id);
            assert_eq!(
                &face.bytes[..],
                aterm_render::bundled::face_for_id(id).unwrap()
            );
        }
    }

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
        let debian_regular =
            std::path::Path::new("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf");
        let debian_bold = std::path::Path::new("/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf");
        if debian_regular.exists() && debian_bold.exists() {
            let assets = resolve_ui_font_assets();
            assert!(
                assets.regular.is_some(),
                "Noto regular resolves on a host carrying the Debian core cuts"
            );
            assert!(
                assets.semibold.is_some(),
                "a heavier companion resolves beside the regular"
            );
            let first_complete = candidates
                .iter()
                .find(|candidate| {
                    candidate.regular_path.exists() && candidate.semibold_path.exists()
                })
                .expect("some pair resolves on a host with fonts installed");
            assert!(
                file(&first_complete.regular_path).contains("noto"),
                "the first complete pair is Noto, never the DejaVu fallback: {:?}",
                first_complete.regular_path
            );
        }
    }

    #[test]
    fn ui_font_assets_admit_each_distinct_file_once() {
        let mut files = UiFontFiles::default();
        let assets = resolve_ui_font_assets_with(&mut files);
        assert_eq!(
            files.reads,
            files.admitted.len(),
            "every distinct path was admitted exactly once"
        );
        let Some(pair) = ui_font_candidates()
            .into_iter()
            .find(|c| c.regular_path.is_file() && c.semibold_path.is_file())
        else {
            aterm_spec::host::could_not_run(
                "no complete UI face pair (regular and semibold) on this host",
            );
        };
        assert!(assets.regular.is_some() && assets.semibold.is_some());
        if pair.regular_path == pair.semibold_path {
            assert_eq!(
                files.reads,
                1,
                "regular and semibold live in ONE file ({}); it must be admitted once, \
                 not once per face",
                pair.regular_path.display()
            );
        }
        for (path, admitted) in &files.admitted {
            let Some(bytes) = admitted else { continue };
            assert_eq!(
                bytes.is_mapped(),
                cfg!(any(unix, windows)) && aterm_render::font_file::maps_in_place(path),
                "{}: a UI face is a file MAPPING exactly when it lives on the system font \
                 volume, and a copy otherwise",
                path.display()
            );
        }
    }

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
            aterm_spec::host::could_not_run("no system UI face here for the strip band");
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
    }

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
            "RINGS".to_string(),
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
                frac: 0.3 + i as f32 * 0.2,
                fill: good,
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
        });
        prims
    }

    #[test]
    fn chrome_face_gate_exhaustive() {
        let (mut fallback_seen, mut bold_seen, mut primary_seen) = (false, false, false);
        for bits in 0u32..8 {
            let b = |i: u32| (bits >> i) & 1 == 1;
            let (bold_run, bold_has, primary_has) = (b(0), b(1), b(2));
            let pick = select_chrome_face(bold_run, bold_has, primary_has);
            if pick == ChromeFacePick::Fallback {
                fallback_seen = true;
                assert!(
                    !(primary_has || (bold_run && bold_has)),
                    "DejaVu chosen although a user face covers the glyph \
                     (bold_run={bold_run}, bold_has={bold_has}, primary_has={primary_has})"
                );
            }
            if bold_run && bold_has {
                bold_seen = true;
                assert_eq!(
                    pick,
                    ChromeFacePick::Bold,
                    "covered bold run lost its weight"
                );
            }
            if pick == ChromeFacePick::Primary {
                primary_seen = true;
                assert!(primary_has, "primary chosen without coverage");
            }
        }
        assert!(
            fallback_seen && bold_seen && primary_seen,
            "gate must reach all three faces (fallback={fallback_seen}, \
             bold={bold_seen}, primary={primary_seen})"
        );
    }

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
        let (row_h, size) = (20.0_f32, 14.0_f32);
        let old_b = (row_h - size) * 0.5 + size;
        let cap = size * ratio;
        assert!(
            ((old_b - cap) - (row_h - old_b)).abs() > 1.5,
            "the pre-fix baseline must be detectably unbalanced"
        );
    }

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

    const MONO_GLYPH_BASELINE: f32 = 40.0;
    const MONO_GLYPH_CANVAS: (u32, u32) = (64, 56);
    const MONO_GLYPH_PX: f32 = 26.0;

    fn mono_run_image(primary: &Arc<[u8]>, s: &str) -> Vec<u8> {
        set_chrome_fonts(Some((Arc::clone(primary), 0)), None, None);
        let prims = vec![crate::widget::text_prim(
            4.0,
            MONO_GLYPH_BASELINE,
            s.to_string(),
            crate::type_scale::TypeStep::Body.px(MONO_GLYPH_PX),
            TextWeight::Regular,
            TextFace::Mono,
            [255, 255, 255, 255],
        )];
        let (width, height) = MONO_GLYPH_CANVAS;
        rasterize_tray(&prims, width, height, 1.0, [0, 0, 0, 255]).0
    }

    fn primary_draws(ch: char) -> bool {
        let mut fonts = lock_fonts();
        let Some(primary) = fonts.primary.as_ref().map(|face| Arc::clone(&face.font)) else {
            return false;
        };
        fonts
            .face_for(TextWeight::Regular, ch)
            .is_some_and(|face| Arc::ptr_eq(&face.font, &primary))
    }

    #[test]
    fn a_mono_run_draws_latin_1_by_the_unicode_cmap() {
        let shipped: Arc<[u8]> = Arc::from(aterm_render::embedded_font());
        let moved: Arc<[u8]> = aterm_render::font::with_cmap_record_last(&shipped, 1, 0)
            .expect("DejaVu Sans Mono carries a Mac Roman (1,0) subtable")
            .into();
        for (ch, mac) in [
            ('\u{B7}', '\u{2211}'),
            ('\u{D7}', '\u{25CA}'),
            ('\u{B0}', '\u{221E}'),
            ('\u{E9}', '\u{C8}'),
        ] {
            let drawn = mono_run_image(&moved, &ch.to_string());
            assert!(
                primary_draws(ch),
                "the reordered face draws {ch} itself, not the fallback under it"
            );
            let (texels, _) = drawn.as_chunks::<4>();
            assert!(texels.iter().any(|texel| texel[0] > 0), "{ch} inks");
            assert!(
                drawn == mono_run_image(&shipped, &ch.to_string()),
                "{ch} draws as the shipped face draws it"
            );
            assert!(
                drawn != mono_run_image(&shipped, &mac.to_string()),
                "{ch} is not drawn as {mac}, the Mac Roman glyph of its byte"
            );
        }
        set_chrome_fonts(None, None, None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_mono_run_on_monaco_draws_the_middle_dot_as_a_dot() {
        let path = "/System/Library/Fonts/Monaco.ttf";
        let Ok(monaco) = std::fs::read(path) else {
            aterm_spec::host::could_not_run(format!("no {path} on this Mac"));
        };
        let monaco: Arc<[u8]> = monaco.into();
        let (width, height) = MONO_GLYPH_CANVAS;
        let ink = |image: &[u8]| {
            let mut count = 0u32;
            let mut bounds = (u32::MAX, u32::MAX, 0, 0);
            for y in 0..height {
                for x in 0..width {
                    if image[((y * width + x) * 4) as usize] > 0 {
                        count += 1;
                        bounds = (
                            bounds.0.min(x),
                            bounds.1.min(y),
                            bounds.2.max(x + 1),
                            bounds.3.max(y + 1),
                        );
                    }
                }
            }
            (count, bounds)
        };
        let dot_image = mono_run_image(&monaco, "\u{B7}");
        assert!(
            primary_draws('\u{B7}'),
            "Monaco draws · itself, not the fallback under it"
        );
        let (dot, (x0, y0, x1, y1)) = ink(&dot_image);
        let (sum, _) = ink(&mono_run_image(&monaco, "\u{2211}"));
        assert!(dot > 0 && dot * 4 < sum, "· inks {dot} texels, ∑ {sum}");
        let side = MONO_GLYPH_PX * 0.25;
        assert!(
            ((x1 - x0) as f32) < side && ((y1 - y0) as f32) < side,
            "· inks a small box: {x0}..{x1} × {y0}..{y1}"
        );
        let raised = MONO_GLYPH_BASELINE - (y0 + y1) as f32 / 2.0;
        assert!(
            (MONO_GLYPH_PX * 0.15..MONO_GLYPH_PX * 0.5).contains(&raised),
            "· sits {raised} px above the baseline"
        );
        let dejavu: Arc<[u8]> = Arc::from(aterm_render::embedded_font());
        assert!(
            dot_image != mono_run_image(&dejavu, "\u{B7}"),
            "· is Monaco's dot, not the embedded DejaVu's"
        );
        let times = mono_run_image(&monaco, "\u{D7}");
        assert!(
            primary_draws('\u{D7}'),
            "Monaco draws × itself, not the fallback under it"
        );
        assert!(
            times != mono_run_image(&monaco, "\u{25CA}"),
            "× is not drawn as ◊"
        );
        set_chrome_fonts(None, None, None);
    }

    fn ink_clusters(buf: &[u8], pw: u32, ph: u32) -> Vec<(u32, u32, u32, u32)> {
        let mut clusters: Vec<(u32, u32, u32, u32)> = Vec::new();
        let mut open = false;
        for x in 0..pw {
            let mut column: Option<(u32, u32)> = None;
            for y in 0..ph {
                if buf[((y * pw + x) * 4) as usize] > 0 {
                    column = Some(column.map_or((y, y + 1), |(a, b)| (a.min(y), b.max(y + 1))));
                }
            }
            match column {
                Some((y0, y1)) if open => {
                    let last = clusters.last_mut().expect("an open cluster");
                    last.1 = last.1.min(y0);
                    last.2 = x + 1;
                    last.3 = last.3.max(y1);
                }
                Some((y0, y1)) => {
                    clusters.push((x, y0, x + 1, y1));
                    open = true;
                }
                None => open = false,
            }
        }
        clusters
    }

    #[test]
    fn a_chain_face_char_draws_on_the_run_baseline_not_its_own_ascent() {
        if !install_chrome_fonts_with_cjk_chain_for_test() {
            aterm_spec::host::could_not_run("no CJK chain face here to install");
        }
        let px = 16.0_f32;
        let row_h = 28.0_f32;
        let baseline = row_baseline(0.0, row_h, px);
        {
            let mut fonts = lock_fonts();
            assert_eq!(
                select_chrome_face(false, false, false),
                ChromeFacePick::Fallback,
                "with no user face the gate lands on DejaVu"
            );
            assert!(
                !fonts.fallback.as_ref().expect("embedded DejaVu").has('日'),
                "negative control: the gate's own pick cannot draw 日"
            );
            assert!(
                fonts.face_for(TextWeight::Regular, '日').is_some(),
                "the coverage rung reaches 日"
            );
        }
        let prims = vec![crate::widget::text_prim(
            2.0,
            baseline,
            "H日".to_string(),
            crate::type_scale::TypeStep::Body.px(px),
            TextWeight::Regular,
            TextFace::Ui,
            [255, 255, 255, 255],
        )];
        let (buf, pw, ph) = rasterize_tray(&prims, 72, row_h as u32, 1.0, [0, 0, 0, 255]);
        let clusters = ink_clusters(&buf, pw, ph);
        assert!(
            clusters.len() >= 2,
            "H and 日 both ink, apart: {clusters:?}"
        );
        let h = clusters[0];
        let ideo = *clusters.last().expect("clusters");
        let baseline_i = q_round_to_px(px_to_q(baseline)) as f32;
        assert!(
            (ideo.3 as f32 - 1.0) <= baseline_i + 0.2 * px,
            "日 hangs below the baseline: ink {ideo:?}, baseline {baseline_i}"
        );
        assert!(
            ideo.1 as f32 >= baseline_i - px,
            "日 reaches above an em over the baseline: ink {ideo:?}, baseline {baseline_i}"
        );
        let centre = |c: (u32, u32, u32, u32)| (c.1 + c.3) as f32 * 0.5;
        assert!(
            (centre(ideo) - centre(h)).abs() <= 2.0,
            "日 centre {} vs H centre {} (ink {ideo:?} / {h:?})",
            centre(ideo),
            centre(h)
        );
        clear_ui_fonts_for_test();
    }

    #[test]
    fn ui_measure_agrees_with_the_pen_for_a_chain_face_char() {
        if !install_chrome_fonts_with_cjk_chain_for_test() || !strip_band_ui_ready() {
            clear_ui_fonts_for_test();
            aterm_spec::host::could_not_run("no system UI face here for the strip band");
        }
        let s = "A日B";
        let px = 13.0_f32;
        let measured = ui_text_width_for(TextFace::Ui, s, px);
        let notdef_measure = {
            let fonts = lock_fonts();
            let f = fonts.ui_regular.clone().expect("UI regular");
            let mut w = 0.0;
            let mut prev: Option<char> = None;
            for ch in s.chars() {
                if let Some(p) = prev {
                    w += f.char_kern(p, ch, px).unwrap_or(0.0);
                }
                w += f.char_metrics(ch, px).advance_width + UI_TRACKING_EM * px;
                prev = Some(ch);
            }
            w
        };
        assert!(
            measured - notdef_measure > 2.0,
            "the `.notdef` measure {notdef_measure} must under-measure the pen's {measured}"
        );
        let w = (measured.ceil() as u32) + 6;
        let prims = vec![crate::widget::text_prim(
            1.0,
            14.0,
            s.to_string(),
            crate::type_scale::TypeStep::Body.px(px),
            TextWeight::Regular,
            TextFace::Ui,
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
        assert!(
            (ink_right as f32) >= 1.0 + measured - 2.0,
            "ink right edge {ink_right} stops short of measured width {measured}"
        );
        clear_ui_fonts_for_test();
    }

    #[test]
    fn coverage_rung_is_consulted_only_after_the_embedded_fallback_misses() {
        if !install_chrome_fonts_with_cjk_chain_for_test() {
            aterm_spec::host::could_not_run("no CJK chain face here to install");
        }
        let mut fonts = lock_fonts();
        let dejavu = fonts
            .fallback
            .as_ref()
            .expect("embedded DejaVu parses")
            .font
            .clone();
        fonts.sync_coverage();
        let chain = fonts
            .coverage
            .first()
            .expect("the parsed CJK face is admitted")
            .font
            .clone();
        assert!(chain.maps('日'));
        let shared = ['\u{2713}', 'λ', 'π', '√', 'A']
            .into_iter()
            .find(|&ch| dejavu.maps(ch) && chain.maps(ch))
            .expect("Latin at least is in both");
        let face = fonts
            .face_for(TextWeight::Regular, shared)
            .expect("covered");
        assert!(
            Arc::ptr_eq(&face.font, &dejavu),
            "{shared:?} stays on the embedded fallback"
        );
        for weight in [TextWeight::Regular, TextWeight::Bold] {
            let face = fonts.face_for(weight, '日').expect("the rung covers 日");
            assert!(
                Arc::ptr_eq(&face.font, &chain),
                "日 at {weight:?} draws from the chain face"
            );
        }
        assert!(
            fonts.face_for(TextWeight::Regular, '\u{10FFFD}').is_none(),
            "a char no loaded face carries still resolves to nothing"
        );
        drop(fonts);
        clear_ui_fonts_for_test();
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn strip_band_font_epoch_moves_when_a_chain_face_lands() {
        let Some(path) = cjk_chain_face_path_for_test() else {
            aterm_spec::host::could_not_run("no CJK chain face here");
        };
        let theme = aterm_render::Theme::default();
        let mut live = Renderer::from_bytes(aterm_render::embedded_font(), 14.0, theme)
            .expect("embedded renderer");
        live.set_runtime_font_discovery(false);
        live.set_config_fallback_fonts(&[path.to_string()]);
        let _ = live.seal_admitted_font_sources();
        let seed = live
            .fork_semantic_surface(14.0, theme)
            .expect("a sealed generation forks");
        install_dormant_chrome_fonts_for_test(seed);
        prepare_ui_fonts_for_direct_view_test();
        let before = strip_band_font_epoch();
        {
            let mut fonts = lock_fonts();
            assert!(
                fonts.face_for(TextWeight::Regular, '日').is_none(),
                "an unparsed chain face is not admitted"
            );
        }
        assert_eq!(
            strip_band_font_epoch(),
            before,
            "a re-read is not a landing"
        );
        live.prepare_semantic_text("日");
        let after = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(5),
            || {
                let now = strip_band_font_epoch();
                (now != before).then_some(now)
            },
        )
        .unwrap_or_else(strip_band_font_epoch);
        assert_ne!(after, before, "the band key witnesses the landing");
        {
            let mut fonts = lock_fonts();
            assert!(
                fonts.face_for(TextWeight::Regular, '日').is_some(),
                "the rung now reaches 日"
            );
        }
        if strip_band_ui_ready() {
            assert!(strip_band_run_coverable("日本語 shell"));
        }
        clear_ui_fonts_for_test();
    }

    fn ready_chain_faces() -> usize {
        lock_fonts()
            .semantic
            .as_ref()
            .map_or(0, |seed| seed.ready_fallback_faces().count())
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_cjk_label_has_its_chain_face_parsed_when_the_band_asks() {
        let Some(_live) = install_unparsed_cjk_chain_for_test() else {
            aterm_spec::host::could_not_run("no CJK fallback chain could be installed here");
        };
        set_ui_cjk_faces_for_test(None);
        if !strip_band_ui_ready() {
            clear_ui_fonts_for_test();
            aterm_spec::host::could_not_run("no system UI face here for the strip band");
        }
        assert!(strip_band_run_coverable("alpha"));
        assert_eq!(ready_chain_faces(), 0, "a Latin label parses nothing");
        assert!(
            !chrome_cascade_reaches_for_test('日'),
            "CONTROL: nothing has parsed the CJK face"
        );
        assert!(!strip_band_run_coverable("logs \u{0378}"));
        assert_eq!(ready_chain_faces(), 0, "no face maps U+0378: none parsed");
        assert!(
            strip_band_run_coverable("日本語 notes"),
            "the CJK label is coverable on its first ask"
        );
        assert!(ready_chain_faces() >= 1);
        assert!(chrome_cascade_reaches_for_test('日'));
        clear_ui_fonts_for_test();
    }

    #[test]
    fn the_startup_warm_parses_the_cjk_and_hangul_faces_only() {
        let Some(live) = install_unparsed_cjk_chain_for_test() else {
            aterm_spec::host::could_not_run("no CJK fallback chain could be installed here");
        };
        assert_eq!(ready_chain_faces(), 0, "CONTROL: nothing parsed yet");
        assert!(warm_chrome_coverage(Some(&live.fallback_coverage()), None) >= 1);
        assert!(chrome_cascade_reaches_for_test('日'));
        let parsed: Vec<Arc<ChromeFont>> = lock_fonts()
            .semantic
            .as_ref()
            .map(|seed| seed.ready_fallback_faces().cloned().collect())
            .unwrap_or_default();
        assert!(!parsed.is_empty());
        for face in &parsed {
            assert!(
                CHROME_COVERAGE_WARM.chars().any(|ch| face.maps(ch)),
                "the warm parsed a face that maps none of {CHROME_COVERAGE_WARM:?}"
            );
        }
        assert_eq!(
            warm_chrome_coverage(Some(&live.fallback_coverage()), None),
            0,
            "a second warm parses nothing"
        );
        clear_ui_fonts_for_test();
    }

    #[test]
    fn every_install_warms_the_faces_a_cjk_label_draws_with() {
        let Some(_live) = install_unparsed_cjk_chain_for_test() else {
            aterm_spec::host::could_not_run("no CJK fallback chain could be installed here");
        };
        let label = "設定を開く";
        assert!(
            !chrome_cascade_reaches_for_test('設'),
            "CONTROL: nothing has parsed the CJK face"
        );
        let warm = chrome_coverage_warm(&lock_fonts()).expect("an install warms its faces");
        assert!(warm() >= 1, "the warm parses the chain's CJK face");
        assert!(chrome_cascade_reaches_for_test('設'));
        assert_eq!(
            lock_fonts().label_misses(TextFace::Ui, label),
            Vec::<char>::new(),
            "{label} draws in ink"
        );
        clear_ui_fonts_for_test();
    }

    #[test]
    fn east_asian_text_splits_into_hangul_and_han_and_nothing_else_is_asked() {
        for ch in [
            '日', '中', '简', 'の', 'カ', 'ｶ', '、', '「', 'Ａ', '㈱', '㌀', 'ㄅ', '𠀋',
        ] {
            assert_eq!(ui_cjk_script(ch), Some(UiCjkScript::Han), "{ch:?}");
        }
        for ch in ['한', '국', 'ㄱ', 'ᄀ', '㈀', '㉠', 'ￂ'] {
            assert_eq!(ui_cjk_script(ch), Some(UiCjkScript::Hangul), "{ch:?}");
        }
        for ch in [
            'a',
            'é',
            'λ',
            'Ж',
            '→',
            '★',
            '※',
            '✓',
            'क',
            'ก',
            '①',
            '\u{1F680}',
            ' ',
            '\u{3000}',
        ] {
            let asked = ui_cjk_script(ch);
            assert_eq!(asked.is_some(), ch == '\u{3000}', "{ch:?}: {asked:?}");
        }
    }

    #[cfg(windows)]
    fn install_bold_cjk_chain_and_cold_ui_cjk_for_test() -> Option<Arc<UiCjkFaces>> {
        let root = std::path::PathBuf::from(
            std::env::var_os("WINDIR").unwrap_or_else(|| r"C:\Windows".into()),
        )
        .join("Fonts");
        let mut renderer = Renderer::from_bytes(
            aterm_render::embedded_font(),
            14.0,
            aterm_render::Theme::default(),
        )
        .ok()?;
        renderer.set_runtime_font_discovery(false);
        for file in ["msyhbd.ttc", "malgunbd.ttf"] {
            let bytes = std::fs::read(root.join(file)).ok()?;
            renderer.add_fallback_bytes(&bytes).ok()?;
        }
        install_settled_chrome_fonts_for_test(renderer);
        prepare_ui_fonts_for_direct_view_test();
        let store = host_ui_cjk_faces_for_test()?;
        set_ui_cjk_faces_for_test(Some(Arc::clone(&store)));
        strip_band_ui_ready().then_some(store)
    }

    #[cfg(windows)]
    fn label_ink(s: &str, px: f32, face: TextFace) -> (f32, usize) {
        let prims = vec![DrawPrim::Text {
            x: 4.0,
            baseline: px * 1.4,
            s: s.to_string(),
            px,
            color: [255, 255, 255, 255],
            weight: TextWeight::Regular,
            face,
        }];
        let (w, h) = ((px * 12.0) as u32, (px * 2.0) as u32);
        let (pixels, _, _) = rasterize_tray(&prims, w, h, 1.0, [0, 0, 0, 255]);
        let lit: Vec<f32> = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from(c[0]) / 255.0)
            .filter(|&t| t > 0.05)
            .collect();
        (lit.iter().sum::<f32>() / lit.len().max(1) as f32, lit.len())
    }

    #[cfg(windows)]
    #[test]
    fn a_quiet_labels_east_asian_text_inks_like_its_latin_and_the_selected_ones_is_semibold() {
        let Some(store) = install_bold_cjk_chain_and_cold_ui_cjk_for_test() else {
            clear_ui_fonts_for_test();
            aterm_spec::host::could_not_run(
                "no Microsoft YaHei Bold, Malgun Gothic Bold or UI face here to install",
            );
        };
        let px = 26.0;
        for (text, latin) in [("日本語", "notes"), ("한국어", "logs")] {
            assert!(strip_band_run_coverable_in(TextFace::Ui, text));
            assert!(strip_band_run_coverable_in(TextFace::UiBold, text));
            let (cjk, cjk_n) = label_ink(text, px, TextFace::Ui);
            let (semibold, semibold_n) = label_ink(text, px, TextFace::UiBold);
            let (latin, _) = label_ink(latin, px, TextFace::Ui);
            eprintln!(
                "{text}: regular {cjk:.3} ({cjk_n} px), semibold {semibold:.3} ({semibold_n} px), latin {latin:.3}"
            );
            assert!(
                (cjk - latin).abs() <= 0.05,
                "{text} in a quiet label inks {cjk:.3}, its Latin {latin:.3}"
            );
            assert!(
                semibold_n as f32 > cjk_n as f32 * 1.08,
                "{text} in the selected label is heavier: {semibold_n} lit px against {cjk_n}"
            );
            let ch = text.chars().next().unwrap();
            let mut fonts = lock_fonts();
            for (face, weight) in [
                (TextFace::Ui, UiCjkWeight::Regular),
                (TextFace::UiBold, UiCjkWeight::Semibold),
            ] {
                let drawn = Arc::clone(
                    &fonts
                        .ui_miss_face(face, TextWeight::Regular, ch)
                        .expect("a face")
                        .font,
                );
                assert!(
                    store
                        .rung(weight)
                        .iter()
                        .any(|entry| entry.parsed().is_some_and(|font| Arc::ptr_eq(font, &drawn))),
                    "{ch:?} in {face:?} draws from the {weight:?} rung"
                );
            }
            drop(fonts);
            for face in [TextFace::Ui, TextFace::UiBold] {
                let mut fonts = lock_fonts();
                let pen: f32 = text
                    .chars()
                    .map(|ch| {
                        fonts
                            .ui_miss_face(face, TextWeight::Regular, ch)
                            .expect("a face")
                            .font
                            .char_metrics(ch, px)
                            .advance_width
                    })
                    .sum();
                drop(fonts);
                assert_eq!(ui_text_width_for(face, text, px), pen, "{text} in {face:?}");
            }
        }
        set_ui_cjk_faces_for_test(None);
        let (bold, _) = label_ink("日本語", px, TextFace::Ui);
        let (latin, _) = label_ink("notes", px, TextFace::Ui);
        eprintln!("control: chain 日本語 {bold:.3}, latin {latin:.3}");
        assert!(
            bold - latin > 0.05,
            "CONTROL: the chain's Bold inks {bold:.3} against the Latin's {latin:.3}"
        );
        clear_ui_fonts_for_test();
    }

    #[cfg(windows)]
    #[test]
    fn a_semibold_run_never_borrows_the_regular_east_asian_face() {
        let Some(store) = host_ui_cjk_faces_for_test() else {
            aterm_spec::host::could_not_run("no host UI CJK faces here");
        };
        prepare_ui_fonts_for_direct_view_test();
        set_ui_cjk_faces_for_test(Some(Arc::clone(&store)));
        store.materialise_for(UiCjkWeight::Regular, "日の");
        let mut fonts = lock_fonts();
        assert!(
            fonts
                .ui_miss_face(TextFace::Ui, TextWeight::Regular, '日')
                .is_some(),
            "CONTROL: the Regular run reaches the Regular face"
        );
        assert!(
            fonts
                .ui_miss_face(TextFace::UiBold, TextWeight::Regular, '日')
                .is_none(),
            "the Semibold run does not borrow it"
        );
        assert_eq!(fonts.label_misses(TextFace::UiBold, "日"), ['日']);
        assert!(fonts.label_misses(TextFace::Ui, "日").is_empty());
        drop(fonts);
        store.materialise_for(UiCjkWeight::Semibold, "の");
        let advance = |weight: UiCjkWeight| {
            store.rung(weight)[0]
                .parsed()
                .expect("Yu Gothic UI")
                .char_metrics('の', 1000.0)
                .advance_width
                .round()
        };
        assert_eq!(
            (
                advance(UiCjkWeight::Regular),
                advance(UiCjkWeight::Semibold)
            ),
            (816.0, 836.0)
        );
        clear_ui_fonts_for_test();
    }

    #[cfg(windows)]
    #[test]
    fn a_cjk_label_has_its_weights_own_ui_faces_parsed_when_the_band_asks() {
        let Some(_live) = install_unparsed_cjk_chain_for_test() else {
            aterm_spec::host::could_not_run("no CJK fallback chain could be installed here");
        };
        let Some(store) = host_ui_cjk_faces_for_test() else {
            clear_ui_fonts_for_test();
            aterm_spec::host::could_not_run("no host UI CJK faces here");
        };
        set_ui_cjk_faces_for_test(Some(Arc::clone(&store)));
        if !strip_band_ui_ready() {
            clear_ui_fonts_for_test();
            aterm_spec::host::could_not_run("no system UI face here for the strip band");
        }
        let parsed = |weight: UiCjkWeight, script: UiCjkScript| {
            store
                .rung(weight)
                .iter()
                .filter(|face| face.script == script && face.parsed().is_some())
                .count()
        };
        assert_eq!(store.parsed_count(), 0, "CONTROL: a cold store");
        assert!(
            lock_fonts()
                .ui_miss_face(TextFace::Ui, TextWeight::Regular, '日')
                .is_none(),
            "CONTROL: nothing reaches 日"
        );
        assert!(strip_band_run_coverable("alpha"));
        assert_eq!(store.parsed_count(), 0, "a Latin label parses nothing");
        let before = strip_band_font_epoch();
        assert!(strip_band_run_coverable_in(TextFace::Ui, "日本語 notes"));
        assert_eq!(parsed(UiCjkWeight::Regular, UiCjkScript::Han), 1);
        assert_eq!(
            store.parsed_count(),
            1,
            "one face: the one that maps 日本語"
        );
        let quiet = strip_band_font_epoch();
        assert_ne!(quiet, before, "the band key witnesses the landing");
        assert!(strip_band_run_coverable_in(TextFace::UiBold, "한국어"));
        assert_eq!(parsed(UiCjkWeight::Semibold, UiCjkScript::Hangul), 1);
        assert_eq!(parsed(UiCjkWeight::Regular, UiCjkScript::Hangul), 0);
        assert_eq!(store.parsed_count(), 2);
        assert_ne!(strip_band_font_epoch(), quiet);
        assert_eq!(ready_chain_faces(), 0, "the chain is not asked");
        clear_ui_fonts_for_test();
    }

    #[cfg(windows)]
    #[test]
    fn the_startup_warm_parses_the_ui_faces_east_asian_faces_at_both_weights() {
        let Some(store) = host_ui_cjk_faces_for_test() else {
            aterm_spec::host::could_not_run("no host UI CJK faces here");
        };
        assert_eq!(store.parsed_count(), 0, "CONTROL: a cold store");
        assert_eq!(warm_chrome_coverage(None, Some(&store)), 4);
        for weight in [UiCjkWeight::Regular, UiCjkWeight::Semibold] {
            let rung = store.rung(weight);
            let han: Vec<bool> = rung
                .iter()
                .filter(|face| face.script == UiCjkScript::Han)
                .map(|face| face.parsed().is_some())
                .collect();
            assert_eq!(
                han,
                [true, false],
                "{weight:?}: the Japanese face, not the Chinese"
            );
            assert!(
                rung.iter()
                    .filter(|face| face.script == UiCjkScript::Hangul)
                    .all(|face| face.parsed().is_some()),
                "{weight:?}: the Hangul face"
            );
        }
        assert_eq!(warm_chrome_coverage(None, Some(&store)), 0);
    }

    #[test]
    fn a_label_asks_the_chain_for_text_only() {
        for ch in ['日', 'の', '한', 'क', 'ก', 'é', '\u{3000}', '∑'] {
            assert!(chain_may_draw_in_a_label(ch), "{ch:?} is text");
        }
        for ch in [
            '\u{1F680}',
            '\u{2600}',
            '\u{2764}',
            '\u{FE0F}',
            '\u{200D}',
            '\u{E0A0}',
            '\u{F0001}',
            '\u{10FFFD}',
            '\u{7}',
        ] {
            assert!(!chain_may_draw_in_a_label(ch), "{ch:?} is not asked");
        }
    }

    #[test]
    fn set_chrome_fonts_bad_bytes_keep_fallback_rendering() {
        let garbage: std::sync::Arc<[u8]> = std::sync::Arc::from(&b"not a font"[..]);
        set_chrome_fonts(Some((garbage.clone(), 0)), Some((garbage, 3)), None);
        let w = measure_text("abc", 12.0, TextWeight::Regular);
        assert!(w > 0.0, "fallback still measures after a bad injection");
        let bytes: std::sync::Arc<[u8]> = std::sync::Arc::from(aterm_render::embedded_font());
        set_chrome_fonts(Some((bytes, 0)), None, None);
        let w2 = measure_text("abc", 12.0, TextWeight::Bold);
        assert!(
            (w2 - w).abs() < 1e-3,
            "bold with no bold face downgrades to primary"
        );
        set_chrome_fonts(None, None, None);
    }

    #[test]
    fn the_chrome_keeps_the_persons_font_under_a_display_layer() {
        let own = aterm_render::embedded_font();
        let mut renderer = Renderer::from_bytes(own, 14.0, aterm_render::Theme::default()).unwrap();
        renderer.set_display_faces(&["engraved"]).unwrap();
        let primary = renderer.chrome_primary_face();
        assert_eq!(
            primary
                .as_ref()
                .map(|(bytes, index)| (bytes.as_ref(), *index)),
            Some((own, 0))
        );
        assert!(chrome_face_for_slot(&primary).is_some());
    }

    fn specimen_input(source: &str, rows: usize, cols: usize) -> aterm_render::RenderInput {
        let mut terminal = aterm_core::terminal::Terminal::new(rows as u16, cols as u16);
        terminal.process(source.as_bytes());
        let mut input = terminal.cell_frame(rows, cols);
        input.cursor_visible = false;
        input
    }

    fn specimen_spec(
        renderer: Renderer,
        identity: crate::widget::SemanticFontCandidate,
        input: aterm_render::RenderInput,
    ) -> crate::widget::TerminalSpecimenSpec {
        crate::widget::TerminalSpecimenSpec {
            input: Arc::new(input),
            input_fingerprint: 1,
            motion: None,
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
            wallpaper: None,
        }
    }

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
        assert_ne!(
            with_unsupported, with_space,
            "the probe scalar must reach the renderer's miss path"
        );
    }

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

    fn await_semantic_landing(fonts: &mut ChromeFonts) {
        const HANG: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;
        let started = std::time::Instant::now();
        while fonts.semantic_pending.is_some() && started.elapsed() < HANG {
            fonts.poll_semantic_renderer();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        fonts.poll_semantic_renderer();
        assert!(
            fonts.semantic_pending.is_none(),
            "the semantic worker never landed its candidate ({:?})",
            started.elapsed()
        );
        eprintln!("semantic candidate landed after {:?}", started.elapsed());
    }

    #[test]
    fn a_landed_semantic_font_wakes_the_host() {
        static WAKES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        set_semantic_ready_wake(|| {
            WAKES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let gate: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
        let mut fonts = default_chrome_fonts();
        fonts.semantic_worker =
            SemanticPrewarmWorker::spawn_serialized_by(gate, crate::qos::Role::Responsive);
        fonts.semantic_generation = 1;
        let mut renderer = Renderer::from_bytes(
            aterm_render::embedded_font(),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("embedded renderer");
        renderer.set_runtime_font_discovery(false);
        fonts.semantic = Some(renderer);
        fonts.semantic_identity = Some(crate::widget::SemanticFontCandidate::default());
        assert!(fonts.ensure_semantic_candidate(&crate::widget::SemanticFontCandidate::default()));
        await_semantic_landing(&mut fonts);
        let started = std::time::Instant::now();
        while WAKES.load(std::sync::atomic::Ordering::SeqCst) == 0
            && started.elapsed() < std::time::Duration::from_secs(60)
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(WAKES.load(std::sync::atomic::Ordering::SeqCst) > 0);
    }

    #[test]
    fn a_resolving_candidate_still_draws_the_committed_specimen() {
        let mut fonts = default_chrome_fonts();
        let mut renderer = Renderer::from_bytes(
            aterm_render::embedded_font(),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("embedded renderer");
        renderer.set_runtime_font_discovery(false);
        fonts.semantic_generation = 1;
        fonts.semantic = Some(renderer);
        fonts.semantic_identity = Some(crate::widget::SemanticFontCandidate::default());
        fonts.semantic_prewarm_ms = Some(0);
        let (_results_tx, results) = std::sync::mpsc::channel();
        fonts.semantic_worker = Some(SemanticPrewarmWorker {
            queue: Arc::new(SemanticPrewarmQueue {
                semantic_job: Mutex::new(None),
                ready: Condvar::new(),
            }),
            results,
        });
        fonts.semantic_worker_generation = Some(1);
        let candidate = crate::widget::SemanticFontCandidate {
            regular: Some("Iosevka Term".to_string()),
            ..crate::widget::SemanticFontCandidate::default()
        };
        assert!(fonts.ensure_semantic_candidate(&candidate), "pending");
        assert!(
            fonts
                .semantic_renderer_fork_for(&candidate, 14.0, aterm_render::Theme::default())
                .is_some(),
            "the committed cascade draws while the candidate resolves"
        );
    }

    #[test]
    fn a_resolving_candidate_still_draws_the_named_face_it_replaced() {
        let mut fonts = default_chrome_fonts();
        let mut renderer = Renderer::from_bytes(
            aterm_render::embedded_font(),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("embedded renderer");
        renderer.set_runtime_font_discovery(false);
        let saved = crate::widget::SemanticFontCandidate {
            regular: Some("Menlo".to_string()),
            ..crate::widget::SemanticFontCandidate::default()
        };
        fonts.semantic_generation = 1;
        fonts.semantic = Some(renderer);
        fonts.semantic_identity = Some(saved);
        fonts.semantic_prewarm_ms = Some(0);
        let (_results_tx, results) = std::sync::mpsc::channel();
        fonts.semantic_worker = Some(SemanticPrewarmWorker {
            queue: Arc::new(SemanticPrewarmQueue {
                semantic_job: Mutex::new(None),
                ready: Condvar::new(),
            }),
            results,
        });
        fonts.semantic_worker_generation = Some(1);
        let typed = crate::widget::SemanticFontCandidate {
            regular: Some("NoSuchFontXYZ".to_string()),
            ..crate::widget::SemanticFontCandidate::default()
        };
        assert!(fonts.ensure_semantic_candidate(&typed), "pending");
        assert!(
            fonts
                .semantic_renderer_fork_for(&typed, 14.0, aterm_render::Theme::default())
                .is_some(),
            "Menlo draws while the typed name resolves"
        );
    }

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
        let gate: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
        let serial = gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut fonts = default_chrome_fonts();
        fonts.semantic_worker =
            SemanticPrewarmWorker::spawn_serialized_by(gate, crate::qos::Role::Responsive);
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

        await_semantic_landing(&mut fonts);
        assert_eq!(fonts.semantic_generation, 2);
        assert!(fonts.semantic_pending.is_none(), "land disarms");
        assert!(fonts.semantic.is_some(), "latest exact fork installs");
        assert!(fonts.semantic_prewarm_ms.is_some());
    }

    #[test]
    fn chrome_font_reload_conforms_to_the_generation_queue_reset() {
        let model = aterm_models_misc::semantic_prewarm_generation_model();
        let mut fonts = default_chrome_fonts();
        fonts.semantic_generation = 1;
        let queue = Arc::new(SemanticPrewarmQueue {
            semantic_job: Mutex::new(Some(SemanticPrewarmJob {
                generation: 1,
                request: 1,
                candidate: crate::widget::SemanticFontCandidate::default(),
                base: None,
            })),
            ready: Condvar::new(),
        });
        let (_results_tx, results) = std::sync::mpsc::channel();
        fonts.semantic_worker = Some(SemanticPrewarmWorker {
            queue: Arc::clone(&queue),
            results,
        });
        let observe = |fonts: &ChromeFonts| {
            let mut state = model.init_state();
            state.insert(
                "current",
                i64::try_from(fonts.semantic_generation).expect("small generation"),
            );
            let queued = queue
                .semantic_job
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .map_or(0, |job| job.generation);
            state.insert("queued", i64::try_from(queued).expect("small generation"));
            state
        };
        let before = observe(&fonts);
        let mut requested = model.init_state();
        assert!(model.fire("Request", &mut requested));
        assert_eq!(before, requested, "one job queued for the live generation");

        reload_chrome_fonts_locked(&mut fonts, None, None, None);
        let after = observe(&fonts);
        assert_eq!(
            model.successors("Reload", &before),
            vec![after.clone()],
            "the shipping reload is the model's Reload"
        );
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, &after),
                "{}",
                invariant.name
            );
        }

        let mut kept = after;
        kept.insert("queued", before["queued"]);
        assert!(!model.successors("Reload", &before).contains(&kept));
        assert!(!model.check_invariant("QueueContainsOnlyCurrent", &kept));
    }

    #[test]
    fn semantic_prewarm_poll_conforms_to_the_handshake_slot_effects() {
        let model = aterm_models_misc::semantic_prewarm_handshake_model();
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
        let requested = crate::widget::SemanticFontCandidate {
            regular: Some("Requested Candidate 51A7".to_string()),
            ..crate::widget::SemanticFontCandidate::default()
        };
        let older = crate::widget::SemanticFontCandidate {
            regular: Some("Older Candidate 0B3E".to_string()),
            ..crate::widget::SemanticFontCandidate::default()
        };
        let mut rows = 0;
        let mut failed_closed_over_active = 0;
        for generation_matches in [false, true] {
            for request_matches in [false, true] {
                for candidate_matches in [false, true] {
                    for renderer_ready in [false, true] {
                        for (active_before, active_before_latest) in
                            [(false, false), (true, false), (true, true)]
                        {
                            let mut fonts = default_chrome_fonts();
                            fonts.semantic_generation = 2;
                            fonts.semantic_request = 2;
                            fonts.semantic_requested = requested.clone();
                            fonts.semantic_prewarm_ms = Some(1);
                            if active_before {
                                fonts.semantic = Some(renderer());
                                fonts.semantic_identity = Some(if active_before_latest {
                                    requested.clone()
                                } else {
                                    older.clone()
                                });
                            }
                            let (results_tx, results) = std::sync::mpsc::channel();
                            fonts.semantic_worker = Some(SemanticPrewarmWorker {
                                queue: Arc::new(SemanticPrewarmQueue {
                                    semantic_job: Mutex::new(None),
                                    ready: Condvar::new(),
                                }),
                                results,
                            });
                            results_tx
                                .send(SemanticPrewarmResult {
                                    generation: if generation_matches { 2 } else { 1 },
                                    request: if request_matches { 2 } else { 1 },
                                    candidate: if candidate_matches {
                                        requested.clone()
                                    } else {
                                        older.clone()
                                    },
                                    renderer: renderer_ready.then(renderer),
                                    resolution: SemanticFontResolution::Ready,
                                    elapsed_ms: 0,
                                })
                                .expect("stub worker channel");
                            fonts.poll_semantic_renderer();

                            let mut before = model.init_state();
                            for (on, action) in [
                                (generation_matches, "MarkGenerationCurrent"),
                                (request_matches, "MarkRequestCurrent"),
                                (candidate_matches, "MarkCandidateCurrent"),
                                (renderer_ready, "MarkRendererReady"),
                                (active_before && !active_before_latest, "MarkActiveBefore"),
                                (active_before_latest, "MarkActiveBeforeLatest"),
                            ] {
                                if on {
                                    assert!(model.fire(action, &mut before), "{action}");
                                }
                            }
                            let modeled = model.successors("DecideResult", &before)[0].clone();
                            let active_after = fonts.semantic.is_some();
                            let latest = active_after
                                && fonts.semantic_identity.as_ref() == Some(&requested);
                            assert_eq!(
                                (modeled["active_after"], modeled["active_after_latest"]),
                                (i64::from(active_after), i64::from(latest)),
                                "the poll's active slot diverged from DecideResult at \
                                 generation={generation_matches} request={request_matches} \
                                 candidate={candidate_matches} ready={renderer_ready} \
                                 active={active_before} latest={active_before_latest}"
                            );
                            if modeled["decision"] == 3 && active_before {
                                failed_closed_over_active += 1;
                            }
                            rows += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(rows, 48);
        assert_eq!(
            failed_closed_over_active, 2,
            "the fail-closed arm is judged over a live renderer, not only an empty slot"
        );
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
        let gate: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
        let mut fonts = default_chrome_fonts();
        fonts.semantic_worker =
            SemanticPrewarmWorker::spawn_serialized_by(gate, crate::qos::Role::Responsive);
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

        await_semantic_landing(&mut fonts);
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

        fonts.semantic_cache.remove(&first);
        assert!(fonts.ensure_semantic_candidate(&first));
        assert!(
            fonts.semantic.is_none(),
            "B cannot paint A while A resolves"
        );
        await_semantic_landing(&mut fonts);
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
        for slot in 0..4 {
            faces.insert(
                (
                    family.to_ascii_lowercase(),
                    aterm_render::FaceTarget::for_slot(slot),
                ),
                Ok(ResolvedCandidateFace {
                    face: aterm_render::FaceRef::new("jetbrains-mono.ttf", 0),
                    bytes: Arc::new(fixture.to_vec()),
                }),
            );
        }
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
            display: Vec::new(),
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
    fn a_font_candidate_keeps_the_compiled_in_symbol_face() {
        const ONLY_BUNDLED: char = '\u{F09B}';
        let fixture = include_bytes!("../../aterm-render/tests/fixtures/jetbrains-mono.ttf");
        let family = "aterm deterministic JetBrains fixture";
        let mut faces = CandidateFaceCache::new();
        faces.insert(
            (
                family.to_ascii_lowercase(),
                aterm_render::FaceTarget::REGULAR,
            ),
            Ok(ResolvedCandidateFace {
                face: aterm_render::FaceRef::new("jetbrains-mono.ttf", 0),
                bytes: Arc::new(fixture.to_vec()),
            }),
        );
        let candidate = crate::widget::SemanticFontCandidate {
            regular: Some(family.to_string()),
            ..crate::widget::SemanticFontCandidate::default()
        };
        let mut base = Renderer::from_bytes(
            aterm_render::embedded_font(),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("embedded renderer");
        let _ = base.seal_admitted_font_sources();
        let (renderer, resolution) = build_semantic_candidate(&base, &candidate, &mut faces);
        assert_eq!(resolution, SemanticFontResolution::Ready);
        let mut renderer = renderer.expect("candidate renderer");
        assert!(renderer.admitted_font_sources_sealed());
        assert_ne!(
            renderer.glyph_key(ONLY_BUNDLED).source,
            aterm_render::FaceId::Primary,
            "the candidate drew the symbol as .notdef"
        );
        let (closed, _) = build_semantic_candidate(&base, &candidate, &mut faces);
        let mut closed = closed.expect("candidate renderer");
        closed.set_runtime_font_discovery(false);
        assert_eq!(
            closed.glyph_key(ONLY_BUNDLED).source,
            aterm_render::FaceId::Primary
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
        assert!(ui_text_width("Cursor", 20.0) > ui_text_width("Cursor", 10.0));

        let (_, narrow) = aterm_render::shared_parsed_face(
            include_bytes!("../../aterm-render/assets/bundled/NotoSansMath-Regular.ttf"),
            0,
        )
        .expect("the committed math face parses");
        let mut fonts = default_chrome_fonts();
        fonts.ui_regular = Some(narrow);
        let px = 10.0;
        let floor = 0.2 * px;
        let measure = |fonts: &mut ChromeFonts, query: &str| {
            ui_text_width_with_floor(fonts, TextFace::Ui, query, px, floor)
        };
        assert!(measure(&mut fonts, "Appearance Wavy 17").1);
        let thin = "\u{200a}".repeat(100);
        assert!(fonts.label_misses(TextFace::Ui, &thin).is_empty());
        let (width, bounded) = measure(&mut fonts, &thin);
        assert!(width > 0.0 && width < thin.chars().count() as f32 * floor);
        assert!(!bounded, "coverage alone cannot bound the scalar count");
        assert!(fonts.label_misses(TextFace::Ui, "\u{200b}").is_empty());
        assert!(
            !measure(&mut fonts, "\u{200b}").1,
            "zero advance refuses too"
        );
        fonts.ui_regular = None;
        assert!(measure(&mut fonts, "ordinary fallback").1);

        let (_, kerned) = aterm_render::shared_parsed_face(
            include_bytes!("../../aterm-render/assets/game/Chewy-Regular.ttf"),
            0,
        )
        .expect("the committed kerned face parses");
        let left = kerned.char_metrics('A', px).advance_width + UI_TRACKING_EM * px;
        let right = kerned.char_metrics('U', px).advance_width + UI_TRACKING_EM * px;
        let kern = kerned.char_kern('A', 'U', px).expect("the pair is kerned");
        let pair_floor = (left + (left + kern)) * 0.5;
        assert!(kern < 0.0 && left >= pair_floor && right + kern >= pair_floor);
        fonts.ui_regular = Some(kerned);
        assert!(ui_text_width_with_floor(&mut fonts, TextFace::Ui, "A", px, pair_floor).1);
        assert!(ui_text_width_with_floor(&mut fonts, TextFace::Ui, "U", px, pair_floor).1);
        assert!(
            !ui_text_width_with_floor(&mut fonts, TextFace::Ui, "AU", px, pair_floor).1,
            "removing the left scalar must not reverse the width ordering"
        );
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
    fn a_lines_trailing_space_never_ends_it_a_word_early() {
        let px = 13.0;
        let text = "alpha beta gamma";
        let space = ui_text_width(" ", px);
        assert!(space > 1.0, "premise: a space has width");
        let width = ui_text_width("alpha beta", px) + space * 0.5;
        let lines: Vec<&str> = ui_text_wrap_ranges(text, px, width)
            .into_iter()
            .map(|range| &text[range])
            .collect();
        assert_eq!(lines.concat(), text, "the ranges cover the text");
        assert_eq!(lines[0].trim_end(), "alpha beta", "{lines:?}");
        assert_eq!(lines[1], "gamma", "{lines:?}");
        let short = ui_text_width("alpha bet", px);
        let lines: Vec<&str> = ui_text_wrap_ranges(text, px, short)
            .into_iter()
            .map(|range| &text[range])
            .collect();
        assert_eq!(lines[0].trim_end(), "alpha", "{lines:?}");
        let ended = "alpha beta ";
        assert_eq!(
            ui_text_wrap_ranges(ended, px, width),
            vec![0..ended.len()],
            "one line, its trailing space with it"
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
    fn ui_text_wrap_keeps_a_no_break_space_as_glue() {
        for glue in ['\u{a0}', '\u{202f}', '\u{2007}'] {
            let source = format!("aaa 50{glue}% bbb");
            let maximum_width = ui_text_width(&format!("aaa 50{glue}"), 13.0) + 0.1;
            assert!(ui_text_width(&format!("aaa 50{glue}%"), 13.0) > maximum_width);
            let lines: Vec<&str> = ui_text_wrap_ranges(&source, 13.0, maximum_width)
                .into_iter()
                .map(|range| &source[range])
                .collect();
            assert_eq!(lines.concat(), source);
            assert!(
                lines.iter().all(|line| !line.starts_with('%')),
                "{glue:?}: {lines:?}"
            );
            assert_eq!(lines.first().copied(), Some("aaa "), "{glue:?}: {lines:?}");
        }
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

    #[test]
    fn ui_faces_rasterize_and_bold_is_no_lighter() {
        set_chrome_fonts(None, None, None);
        let text = |face: TextFace| {
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
    fn a_terminal_frame_blits_as_an_opaque_blend_would() {
        let frame = aterm_render::Frame {
            width: 9,
            height: 7,
            pixels: (0..63_u32)
                .map(|i| 0x0010_2030 ^ (i * 0x0003_0507))
                .collect(),
        };
        let backdrop = [40, 50, 60, 255];
        let reference = |clip: Option<(f32, f32, f32, f32)>, at: (i32, i32)| {
            let mut canvas = Canvas::with_origin(4, 3, 12, 10, backdrop);
            if let Some((x, y, w, h)) = clip {
                canvas.push_clip(x, y, w, h);
            }
            for (row, pixels) in frame.pixels.chunks(frame.width).enumerate() {
                for (column, pixel) in pixels.iter().enumerate() {
                    canvas.blend(
                        at.0 + column as i32,
                        at.1 + row as i32,
                        [(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8, 255],
                        1.0,
                    );
                }
            }
            canvas.px
        };
        let blitted = |clip: Option<(f32, f32, f32, f32)>, at: (i32, i32)| {
            let mut canvas = Canvas::with_origin(4, 3, 12, 10, backdrop);
            if let Some((x, y, w, h)) = clip {
                canvas.push_clip(x, y, w, h);
            }
            canvas.blit_terminal_frame(&frame, at.0, at.1);
            canvas.px
        };
        for clip in [
            None,
            Some((6.0, 5.0, 5.0, 4.0)),
            Some((30.0, 30.0, 2.0, 2.0)),
        ] {
            for at in [(4, 3), (0, 0), (10, 8), (13, 9), (-20, -20)] {
                assert_eq!(blitted(clip, at), reference(clip, at), "{clip:?} at {at:?}");
            }
        }
        let untouched = Canvas::with_origin(4, 3, 12, 10, backdrop).px;
        assert_ne!(blitted(None, (4, 3)), untouched);
    }

    #[test]
    fn blend_composites_in_linear_light() {
        let mut cv = Canvas::new(1, 1, [0, 0, 0, 255]);
        cv.blend(0, 0, [255, 255, 255, 255], 0.5);
        assert!(
            (185..=190).contains(&cv.px[0]),
            "linear-light midpoint expected ~188, got {}",
            cv.px[0]
        );
    }

    #[test]
    fn glyph_texels_blend_like_terminal_text() {
        let ink = [0x1f, 0x23, 0x28, 0xff];
        let ground = [0xf1, 0xf1, 0xf1, 0xff];
        let pack = |p: [u8; 4]| u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2]);
        for coverage in [1_u8, 64, 128, 200, 254] {
            let mut cv = Canvas::new(1, 1, ground);
            cv.blend_glyph_texel(0, 0, ink, coverage);
            let terminal =
                aterm_render::blend_text(pack(ground), pack(ink), pack(ground), coverage, true);
            let want = [
                (terminal >> 16) as u8,
                (terminal >> 8) as u8,
                terminal as u8,
            ];
            for (k, (&got, &want)) in cv.px.iter().zip(&want).enumerate() {
                assert!(
                    got.abs_diff(want) <= 1,
                    "coverage {coverage} channel {k}: chrome {got} vs terminal {want}"
                );
            }
        }
        let mut plain = Canvas::new(1, 1, ground);
        plain.blend(0, 0, ink, 128.0 / 255.0);
        let mut glyph = Canvas::new(1, 1, ground);
        glyph.blend_glyph_texel(0, 0, ink, 128);
        assert!(
            plain.px[1] > glyph.px[1] + 30,
            "linear {} vs corrected {}",
            plain.px[1],
            glyph.px[1]
        );
        let mut full = Canvas::new(1, 1, ground);
        full.blend_glyph_texel(0, 0, ink, 255);
        assert_eq!(full.px, ink);
        let mut none = Canvas::new(1, 1, ground);
        none.blend_glyph_texel(0, 0, ink, 0);
        assert_eq!(none.px, ground);
        let mut bare = Canvas::new(1, 1, [0, 0, 0, 0]);
        let mut bare_plain = Canvas::new(1, 1, [0, 0, 0, 0]);
        bare.blend_glyph_texel(0, 0, ink, 128);
        bare_plain.blend(0, 0, ink, 128.0 / 255.0);
        assert_eq!(bare.px, bare_plain.px);
    }

    #[test]
    fn mono_text_is_as_heavy_dark_on_light_as_light_on_dark() {
        set_chrome_fonts(None, None, None);
        let run = |ink: [u8; 4], ground: [u8; 4]| {
            let prims = vec![crate::widget::text_prim(
                2.0,
                row_baseline(0.0, 20.0, 13.0),
                "Linen Dracula".to_string(),
                crate::type_scale::TypeStep::Body.px(13.0),
                TextWeight::Regular,
                TextFace::Mono,
                ink,
            )];
            rasterize_tray(&prims, 140, 20, 1.0, ground).0
        };
        let dark_on_light = run([0, 0, 0, 255], [255, 255, 255, 255]);
        let light_on_dark = run([255, 255, 255, 255], [0, 0, 0, 255]);
        let inked = light_on_dark.iter().step_by(4).filter(|&&v| v > 0).count();
        assert!(inked > 50, "the run inks: {inked} texels");
        let worst = dark_on_light
            .iter()
            .step_by(4)
            .zip(light_on_dark.iter().step_by(4))
            .map(|(&d, &l)| (255 - d).abs_diff(l))
            .max()
            .unwrap_or(0);
        assert!(
            worst <= 2,
            "dark-on-light departs from the mirror by {worst}"
        );
    }

    #[test]
    fn the_ui_pen_places_glyphs_at_quarter_pixels() {
        assert_eq!(pen_phase(10.0), (10, 0));
        assert_eq!(pen_phase(10.2), (10, 1));
        assert_eq!(pen_phase(10.5), (10, 2));
        assert_eq!(pen_phase(10.74), (10, 3));
        assert_eq!(pen_phase(10.9), (11, 0));
        assert_eq!(pen_phase(-0.3), (-1, 3));
        let font = ChromeFont::from_bytes(
            aterm_render::embedded_font(),
            aterm_render::font::FontSettings::default(),
        )
        .expect("embedded font parses");
        let edge = |dx: f32| {
            let (m, cov) = font.char_raster_at('l', 13.0, dx);
            let row = &cov[(m.height / 2) * m.width..][..m.width];
            let first = row.iter().position(|&c| c > 0).expect("the stem is inked");
            f64::from(m.xmin) + first as f64 + 1.0 - f64::from(row[first]) / 255.0
        };
        let at0 = edge(0.0);
        for phase in 1..4u8 {
            let dx = f32::from(phase) / PEN_PHASES;
            let moved = edge(dx) - at0;
            assert!(
                (moved - f64::from(dx)).abs() < 0.01,
                "phase {phase}: the stem moved {moved}, not {dx}"
            );
        }
        let (m0, c0) = font.char_raster('l', 13.0);
        let (m1, c1) = font.char_raster_at('l', 13.0, 1.0);
        assert_eq!((m1.xmin, m1.width, &c1), (m0.xmin + 1, m0.width, &c0));
        assert_eq!(
            font.char_raster_at('l', 13.0, 0.0),
            font.char_raster('l', 13.0)
        );
    }

    #[test]
    fn glyph_rasterization_is_memoized_for_one_retained_surface() {
        let font = ChromeFont::from_bytes(
            aterm_render::embedded_font(),
            aterm_render::font::FontSettings::default(),
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

    #[test]
    fn a_fade_eases_what_it_holds_out_over_its_band() {
        let panel = |x: f32| DrawPrim::Panel {
            x,
            y: 0.0,
            w: 4.0,
            h: 40.0,
            radius: 0.0,
            fill: [0, 255, 0, 255],
        };
        let prims = vec![
            DrawPrim::FadePush {
                y: 10.0,
                h: 20.0,
                from: 1.0,
                to: 0.0,
            },
            panel(0.0),
            DrawPrim::FadePop,
            panel(8.0),
        ];
        let (px, pw, _ph) = rasterize_tray(&prims, 12, 40, 1.0, [0, 0, 0, 255]);
        let green = |x: u32, y: u32| px[((y * pw + x) * 4 + 1) as usize];
        assert_eq!(green(1, 5), 255, "above the band, untouched");
        assert!(green(1, 10) >= 240, "no step where the band starts");
        let quarter = green(1, 15);
        let half = green(1, 20);
        assert!(
            quarter < 230 && half + 20 < quarter && half > 40,
            "eased out: {quarter} at a quarter, {half} at half"
        );
        assert!(green(1, 31) <= 2, "nothing at and past the band's end");
        assert_eq!(green(9, 31), 255, "what follows the pop is whole");
    }

    #[test]
    fn every_unresolved_slot_is_read_off_a_font_status() {
        let status = "candidate fallback active: unresolved regular \"No Such\": \"No Such\" \
                      does not resolve to a font file, bold italic \"Q \\\"x\\\"\": missing; \
                      requested regular \"No Such\", emoji \"E\"";
        assert_eq!(
            unresolved_slots(status),
            vec![
                ("regular", "No Such".to_string()),
                ("bold italic", "Q \"x\"".to_string())
            ]
        );
        assert!(unresolved_slots("candidate renderer ready: regular \"A\"").is_empty());
    }

    #[test]
    fn a_fade_fades_what_it_holds_as_one_picture() {
        let ground = [40, 29, 78, 255];
        let surface = [17, 19, 24, 255];
        let frame = aterm_render::Frame {
            width: 6,
            height: 30,
            pixels: vec![0x0011_1318; 180],
        };
        let raster = |fade: bool| {
            let mut cv = Canvas::new(24, 40, ground);
            cv.push_clip(0.0, 0.0, 24.0, 36.0);
            if fade {
                cv.push_fade(10.0, 36.0, 1.0, 0.0, true);
            }
            cv.round_rect(0.0, 0.0, 20.0, 36.0, 0.0, surface);
            cv.round_rect(1.0, 0.0, 12.0, 33.0, 0.0, surface);
            cv.blit_terminal_frame(&frame, 2, 0);
            if fade {
                cv.pop_fade();
            }
            cv.pop_clip();
            cv.px
        };
        let at = |px: &[u8], x: usize, y: usize| {
            let i = (y * 24 + x) * 4;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        let whole = raster(false);
        assert_eq!(at(&whole, 4, 20), surface, "CONTROL: the frame blits whole");
        let faded = raster(true);
        assert_eq!(at(&faded, 4, 5), surface, "above the band, untouched");
        let mut dimmed = 0;
        for y in 10..36 {
            let (frame_px, inner, outer) = (at(&faded, 4, y), at(&faded, 10, y), at(&faded, 16, y));
            assert_eq!(frame_px, outer, "row {y}: the frame is the panel past it");
            assert_eq!(inner, outer, "row {y}: a panel over a panel is one panel");
            dimmed += usize::from(outer != surface);
        }
        assert!(dimmed > 20, "the band fades: {dimmed} rows dimmed");
        assert_eq!(
            at(&faded, 4, 35)[..3],
            at(&faded, 22, 35)[..3],
            "gone at the band's end"
        );
    }

    #[test]
    fn hsv_disk_center_edge_and_outside() {
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
        let half = raster(0.5);
        assert_eq!(
            rgb(&half, 10, 10),
            [128, 128, 128],
            "centre = white×0.5 at value 0.5"
        );
        let edge = rgb(&full, 18, 10); // 3 o'clock: hue 0.25 = full-sat green-ish
        assert!(
            edge[1] > 0 && edge[1] < 255,
            "rim is anti-aliased, got {edge:?}"
        );
        assert_eq!(rgb(&full, 0, 0), [0, 0, 0], "far corner untouched");
        assert_eq!(rgb(&full, 20, 10), [0, 0, 0], "just past the rim untouched");
    }

    #[test]
    fn hairline_stroke_snaps_to_one_device_row() {
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
                },
                DrawPrim::Panel {
                    x: 30.0,
                    y: 20.0,
                    w: 50.0,
                    h: 30.0,
                    radius: 6.0,
                    fill,
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
    fn every_admitted_message_glyph_inks_in_the_cold_store() {
        let mut fonts = default_chrome_fonts();
        assert!(
            fonts.face_for(TextWeight::Regular, '\u{2139}').is_none(),
            "the pin's premise: no cold face carries \u{2139}"
        );
        for &ch in aterm_messages::Glyph::ALLOWED {
            let faced = fonts.face_for(TextWeight::Regular, ch).is_some();
            let own = synthesized::covers(ch);
            assert!(faced || own, "{ch:?} (U+{:04X}) has no ink", u32::from(ch));
            assert!(
                !(faced && own),
                "{ch:?} is the pen's own only where every face misses"
            );
            if !own {
                continue;
            }
            for px in [11.0, 14.0, 28.0, 44.0] {
                let (m, bitmap) = synthesized::rasterize(ch, px).expect("drawn");
                assert_eq!(bitmap.len(), m.width * m.height, "{ch:?} at {px}px");
                assert!(
                    bitmap.contains(&255),
                    "{ch:?} at {px}px has solid ink, not only edges"
                );
                assert_eq!(
                    m.advance_width,
                    synthesized::advance(ch, px).unwrap(),
                    "the pen advances by what the measure charges"
                );
                assert_eq!(
                    measure_text(&ch.to_string(), px, TextWeight::Regular),
                    q_to_px(px_to_q(m.advance_width)),
                    "{ch:?} at {px}px measures as it is drawn"
                );
                assert!(m.ymin >= -1, "{ch:?} at {px}px sinks under the baseline");
                assert!(
                    (m.ymin + m.height as i32) as f32 <= 0.72 * px + 1.0,
                    "{ch:?} at {px}px rises past the cap height"
                );
                assert!(
                    (m.xmin + m.width as i32) as f32 <= m.advance_width + 1.0,
                    "{ch:?} at {px}px overhangs its advance"
                );
            }
        }
    }

    #[test]
    fn the_pen_inks_the_info_glyph_no_face_carries() {
        let px = 28.0_f32;
        let (x, baseline) = (4.0_f32, 40.0_f32);
        for face in [TextFace::Ui, TextFace::Mono] {
            let prims = vec![DrawPrim::Text {
                x,
                baseline,
                s: "\u{2139}".to_string(),
                px,
                color: [255, 255, 255, 255],
                weight: TextWeight::Regular,
                face,
            }];
            let (pixels, w, _) = rasterize_tray(&prims, 48, 48, 1.0, [0, 0, 0, 255]);
            let lit: Vec<(usize, usize)> = pixels
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .filter(|(_, c)| c[0] >= 128)
                .map(|(i, _)| (i % w as usize, i / w as usize))
                .collect();
            assert!(!lit.is_empty(), "{face:?}: the pen inked nothing");
            let (left, right) = (x as usize, (x + 0.76 * px).ceil() as usize);
            let (top, bottom) = (
                (baseline - 0.69 * px).floor() as usize,
                baseline as usize + 1,
            );
            for &(lx, ly) in &lit {
                assert!(
                    (left..=right).contains(&lx) && (top..=bottom).contains(&ly),
                    "{face:?}: ink at ({lx}, {ly}) is outside the glyph's box"
                );
            }
            let (cx, cy) = ((x + 0.38 * px) as usize, (baseline - 0.36 * px) as usize);
            let lit_at = |px_x: usize, px_y: usize| lit.contains(&(px_x, px_y));
            assert!(
                lit_at(cx, (baseline - 0.66 * px) as usize)
                    && lit_at(cx, (baseline - 0.06 * px) as usize),
                "{face:?}: the ring's top and bottom are lit"
            );
            assert!(
                !lit_at(cx + (0.15 * px) as usize, cy),
                "{face:?}: the ring is hollow beside the stem"
            );
        }
    }
}
