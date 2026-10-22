// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A small anti-aliased CPU rasterizer for the widget tray's [`DrawPrim`] list. It
//! renders the prims into an RGBA8 buffer with analytic SDF anti-aliasing (rounded
//! panel, ring, capsule, dot) and real glyphs — the USER'S
//! terminal face (+ its real bold sibling), injected via [`set_chrome_fonts`] from
//! the renderer's resolved primary font, with the bundled DejaVu strictly as a
//! per-char COVERAGE fallback (symbols like ⌘⇧⌃✓ the terminal face lacks; see
//! [`select_chrome_face`]). It is the SHARED renderer core: the CPU/softbuffer
//! backend composites this buffer directly, and the GPU backend uploads it as one
//! overlay texture — so the tray looks IDENTICAL on both paths and stays WYSIWYG
//! for the `image` introspection. Pure pixels; no window/GPU state.
//!
//! Text prims are positioned by baseline: row-centred sites use the cap-height rule
//! ([`row_baseline`], backed by `aterm_render::chrome_metrics::baseline_in_row_q`), and the pen
//! accumulates advances in 26.6 fixed point with per-glyph rounding (placement error ≤ 0.5 px,
//! no drift; proven in `chrome_metrics`).

use std::collections::HashMap;
use std::f32::consts::TAU;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use aterm_grapheme::GraphemeClusters;
// The chrome's face type: the same first-party parsed faces the terminal renderer draws with, so
// the tray takes no font dependency of its own.
use aterm_render::font::{Font as ChromeFont, Metrics as ChromeMetrics};

/// Serializes the rare heavyweight Unicode-font warmups. A hot reload can replace a pending
/// semantic fork (its dropped receiver prevents stale installation); this gate keeps old and new
/// workers from parsing two temporary 100–370 MB copies at once before the process-wide `Arc`
/// interner converges.
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

/// The first slot (`regular`, `bold italic`, `emoji`, …) a
/// [`SemanticFontResolution::Fallback`] status names as unresolved, so the
/// specimen's status names the font that failed rather than the row that
/// has focus.
/// Every slot a semantic font status names unresolved, with the family it
/// asked for (`candidate fallback active: unresolved regular "Menlo X",
/// bold "Y"; requested …`), in the order the status names them.
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
        // The family as `{:?}` wrote it: up to the first unescaped quote.
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
        // The next slot after this one's reason, before what was requested.
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

/// Resolved candidate faces by (lowercased family, the style it serves): one
/// family resolves to a different face per slot (`Menlo` and its Bold).
type CandidateFaceCache =
    HashMap<(String, aterm_render::FaceTarget), Result<ResolvedCandidateFace, String>>;

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

/// How the worker tells the host a result is waiting: the event loop wakes
/// and repaints the specimens that asked for it, so a status never sticks
/// on "Preparing" until some unrelated input. Unset (tests), nothing wakes.
static SEMANTIC_READY_WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Install [`SEMANTIC_READY_WAKE`]; the first install wins.
pub(crate) fn set_semantic_ready_wake(wake: impl Fn() + Send + Sync + 'static) {
    let _ = SEMANTIC_READY_WAKE.set(Box::new(wake));
}

impl SemanticPrewarmWorker {
    fn spawn() -> Option<Self> {
        Self::spawn_serialized_by(&SEMANTIC_PREWARM_GATE, crate::qos::Role::Background)
    }

    /// `gate` serializes heavyweight warmups: [`SEMANTIC_PREWARM_GATE`] in production; worker
    /// unit tests pass a private gate so their staged windows never queue behind another
    /// test's warmup.
    ///
    /// `role` is the worker's class: [`crate::qos::Role::Background`] in
    /// production, where nothing waits on a warmup. Those tests wait on it and
    /// pass [`crate::qos::Role::Responsive`]: the job asks a few CPU-ms, and at
    /// UTILITY a loaded machine left it unscheduled for seconds (2026-10-20,
    /// load 245-280, alone: the three tests 1.3-12 s each, 0.1-0.4 s at
    /// Responsive; in the test binary the replacement test was the last to
    /// finish in five runs of six, 1.9-16 s, its CPU 0.06 s).
    fn spawn_serialized_by(gate: &'static Mutex<()>, role: crate::qos::Role) -> Option<Self> {
        let queue = Arc::new(SemanticPrewarmQueue {
            semantic_job: Mutex::new(None),
            ready: Condvar::new(),
        });
        let worker_queue = Arc::clone(&queue);
        let (tx, results) = std::sync::mpsc::channel();
        crate::qos::spawn_lane_thread("aterm-semantic-font-prewarm", role, move || {
            // QoS (port of 61a6c8b62): cosmetic — it warms a PRIVATE renderer
            // clone. The process-global font interns it publishes into parse
            // OUTSIDE their locks (`intern_parsed_font_keyed`), the job queue
            // mutex is dropped before any work, and `SEMANTIC_PREWARM_GATE`
            // serialises only prewarm workers; the UI thread contends none of
            // them for longer than a scan, so demotion is safe here.
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
        // If the worker has not yet taken the generation-install job, a replacement carries
        // its non-cloneable base forward (otherwise the worker owns it and `pending` is empty),
        // so a Renderer is never cloned or built on the UI thread.
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
    // `read_resolved_font`, never a bare file read: a bundled face
    // (`JetBrains Mono` on non-macOS, `bundled:…`) resolves to compiled-in
    // bytes — the same interception every other resolution path performs.
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

/// Worker-only candidate construction: family resolution, file reads, face parsing,
/// styled-face injection and specimen warmup all happen here; the UI thread only swaps the
/// finished renderer into its bounded cache.
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
    // The display layer the candidate draws over its font, as the terminal
    // does (`native_font_catalog::prepare`); the fork carries the host's, so
    // the candidate's replaces it (an empty one clears it).
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
    // Candidate resolution is complete on this parked worker. Once the fork
    // reaches semantic compile/paint it must be a closed immutable cascade:
    // no missing glyph may trigger system resolver or filesystem work. The
    // seal settles the new faces here and closes pathname discovery, yet
    // keeps the compiled-in symbol face; switching runtime discovery off
    // instead closed that face too, so `❯` drew as nothing in any specimen
    // set in a font of the person's choosing.
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

/// How the chrome turns a char into a glyph of a [`ChromeFont`]: through the
/// face's UNICODE cmap subtables ([`ChromeFont::unicode_glyph_index`]), as the
/// terminal grid draws its primary face, never through `lookup_glyph_index`.
/// That map is fontdue's enumeration, where a Macintosh subtable listed after
/// the Unicode one wins: Monaco's last is Mac Central European, so every Mono
/// run drew `·` as `∑` and `×` as `◊`, and nearly all of Latin-1 wrong. Every
/// char the chrome asks a face about (coverage, advance, kern, raster) is
/// asked through these — here and in the strip band's variable-semibold tab
/// label pen ([`crate::tab_bar`]), which resolves its glyph ids and kern pairs
/// through the same answer — so what is measured is what is drawn.
pub(crate) trait UnicodeChars {
    /// Whether the face maps `ch` to a real (non-`.notdef`) glyph.
    fn maps(&self, ch: char) -> bool;
    /// The layout metrics of the glyph `ch` maps to, at `px`.
    fn char_metrics(&self, ch: char, px: f32) -> ChromeMetrics;
    /// The metrics and coverage mask of the glyph `ch` maps to, at `px`.
    fn char_raster(&self, ch: char, px: f32) -> (ChromeMetrics, Vec<u8>);
    /// [`Self::char_raster`] with the pen `dx` px (`0.0..1.0`) right of a
    /// whole pixel; `xmin` counts from that whole pixel.
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

/// One loaded chrome face: the parsed fontdue font plus its cap-height ratio
/// (OS/2 `sCapHeight`, measured-'H' fallback) and a per-char em-advance memo
/// for [`measure_text`].
struct ChromeFace {
    /// Shared with the terminal renderer's parse of the same bytes
    /// (`aterm_render::shared_parsed_face`), so the chrome holds no second copy of a face it
    /// already has (~6.9 MB of heap each).
    font: std::sync::Arc<ChromeFont>,
    /// Cap height as a fraction of the em (drives [`row_baseline`]).
    cap_ratio: f32,
    /// Memoized advance-per-em by char (advances scale linearly with px).
    advances: std::collections::HashMap<char, f32>,
}

/// Reference size for the em-advance memo (large enough that f32 metric quantization is
/// negligible).
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

    /// A coverage face, one the renderer's broad chain already parsed
    /// ([`ChromeFonts::coverage`]). It draws only chars the primary, the bold sibling and the
    /// embedded DejaVu all lack, and never sets a baseline ([`chrome_cap_ratio`] reads the
    /// primary, else DejaVu), so its cap ratio is never read; the Latin norm stands in.
    fn from_font(font: std::sync::Arc<ChromeFont>) -> Self {
        Self {
            font,
            cap_ratio: 0.7,
            advances: std::collections::HashMap::new(),
        }
    }

    /// Whether this face has a real (non-`.notdef`) glyph for `ch`.
    fn has(&self, ch: char) -> bool {
        self.font.maps(ch)
    }

    /// The advance of `ch` per em (memoized; multiply by the run's px).
    fn advance_em(&mut self, ch: char) -> f32 {
        if let Some(&a) = self.advances.get(&ch) {
            return a;
        }
        let a = self.font.char_metrics(ch, ADVANCE_REF_PX).advance_width / ADVANCE_REF_PX;
        self.advances.insert(ch, a);
        a
    }
}

/// The chrome's own ink for the admitted message glyphs no cold face carries: `ℹ` (U+2139)
/// and `⏸` (U+23F8), which DejaVu Sans Mono, Menlo and SF Pro all lack (the chain face that has
/// them is parsed lazily by the live terminal). They are drawn analytically at the pen's size
/// (a ring with a dotted stem; two bars) on the cap box, below [`ChromeFonts::face_for`]'s
/// last rung, so no char a face covers changes face and every host gets the same ink.
/// [`synthesized::advance`] is the pen's own advance, so a run carrying one measures as drawn.
mod synthesized {
    use super::ChromeMetrics;

    /// Whether the pen draws `ch` itself when no loaded face covers it.
    pub(super) const fn covers(ch: char) -> bool {
        matches!(ch, '\u{2139}' | '\u{23f8}')
    }

    /// One glyph's design, in em: its extent (`left..right` from the pen,
    /// `bottom..top` from the baseline, up positive), its advance and its
    /// shape as a point test `(x, y, px)` — `px` so a stroke can floor at
    /// one device pixel.
    struct Design {
        left: f32,
        right: f32,
        bottom: f32,
        top: f32,
        advance: f32,
        inside: fn(f32, f32, f32) -> bool,
    }

    /// `ℹ`: a ring 0.66 em across centred 0.36 em up (the cap box's middle, where
    /// [`super::row_baseline`] centres a run), with a dotted stem. Strokes floor at one device
    /// pixel and the stem snaps to the pixel grid, as hinting would, so an 11 px glyph keeps a
    /// legible ring and a solid spine.
    const INFO: Design = Design {
        left: 0.05,
        right: 0.71,
        bottom: 0.03,
        top: 0.69,
        advance: 0.76,
        inside: info_inside,
    };

    /// `⏸`: two bars 0.16 em wide and 0.56 em tall, 0.14 em apart, on the
    /// mono stack's 0.6 em advance.
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

    fn design(ch: char) -> Option<&'static Design> {
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

    /// Sub-samples per pixel edge: 16 per pixel, so an edge's coverage lands
    /// in sixteenths, anti-aliased like a rasterized outline's.
    const SUB: usize = 4;

    /// `ch` at `px`: its coverage bitmap and placement, in the convention
    /// every glyph the pen blits uses (`xmin`/`ymin` from the pen and the
    /// baseline, rows top-down), or `None` for a char the pen does not draw.
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
            // Row 0 is the bitmap's top: `top` px above the baseline.
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

/// The weight a UI-face run's East Asian text is drawn at ([`UiCjkFaces`]):
/// the weight of the UI face the run is set in. `TextFace::UiBold` (Segoe UI
/// Semibold, the selected tab's label, a heading) is `Semibold`; every other
/// face is `Regular`.
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

    /// The rung's slot in [`UiCjkFaces::rungs`] and `ChromeFonts::ui_cjk_held`.
    const fn slot(self) -> usize {
        match self {
            Self::Regular => 0,
            Self::Semibold => 1,
        }
    }
}

/// Which of the chrome's East Asian faces a char is asked of: Hangul goes to
/// the Korean face, every other CJK char (ideographs, kana, bopomofo, CJK
/// punctuation, the full- and halfwidth forms) to the Japanese then the
/// Chinese face — the split Windows' own fallback for Segoe UI makes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UiCjkScript {
    Han,
    Hangul,
}

/// Whether `ch` is in one of the East Asian blocks ([`ui_cjk_script`]): text
/// of them breaks between its characters, with no hyphen.
pub(crate) fn is_east_asian(ch: char) -> bool {
    ui_cjk_script(ch).is_some()
}

/// The [`UiCjkScript`] of `ch`, or `None` for a char outside the East Asian
/// blocks: such a char keeps the terminal cascade it always had
/// ([`ChromeFonts::face_for`]), so no symbol, arrow or other script changes face.
fn ui_cjk_script(ch: char) -> Option<UiCjkScript> {
    match u32::from(ch) {
        // Hangul Jamo, Compatibility Jamo, Jamo Extended-A, syllables, Jamo
        // Extended-B, the parenthesized and circled Hangul, halfwidth Hangul.
        0x1100..=0x11FF
        | 0x3130..=0x318F
        | 0x3200..=0x321E
        | 0x3260..=0x327E
        | 0xA960..=0xA97F
        | 0xAC00..=0xD7FF
        | 0xFFA0..=0xFFDC => Some(UiCjkScript::Hangul),
        // CJK radicals, Kangxi, description characters, CJK symbols and
        // punctuation, kana, bopomofo, kanbun, strokes, enclosed CJK and CJK
        // compatibility, Extension A, the unified ideographs, compatibility
        // ideographs, vertical and compatibility forms, the full- and halfwidth
        // forms, the kana supplements and the supplementary ideographic planes.
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

/// The chrome's own East Asian faces, per weight, in the order Windows' own
/// fallback for Segoe UI picks them: `(file under %WINDIR%\Fonts, collection
/// index, script)`.
///
/// MEASURED 2026-10-18 on Windows 11 26200 through DirectWrite's system
/// fallback (`IDWriteFactory2::GetSystemFontFallback` → `MapCharacters`, base
/// family Segoe UI Variable, locale en-US and none): `日本語`, `中文`, kana, CJK
/// punctuation and the fullwidth forms map to Yu Gothic UI, the simplified
/// ideographs it lacks (`简`, `们`) to Microsoft JhengHei UI, and Hangul to
/// Malgun Gothic; asked at weight 400 they map to Yu Gothic UI Regular (400),
/// JhengHei UI (400) and Malgun Gothic (400), at weight 600 to Yu Gothic UI
/// Semibold (600), JhengHei UI Bold (700) and Malgun Gothic Bold (700). The
/// faces, read from each file's name and OS/2 tables: `YuGothM.ttc` face 1 is
/// Yu Gothic UI Regular, `YuGothB.ttc` face 2 Yu Gothic UI Semibold,
/// `msjh.ttc`/`msjhbd.ttc` face 1 Microsoft JhengHei UI and its Bold.
///
/// The terminal's chain takes Microsoft YaHei Bold and Malgun Gothic Bold
/// instead (`aterm_render`'s weight-matched siblings, chosen beside Iosevka
/// Term SemiBold): right for the grid, and what drew a quiet tab's `日本語`
/// and `한국어` bolder than the Segoe UI Regular `notes` beside them (mean
/// ink coverage 0.635 against 0.540, the 2026-10-18 judges).
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

/// macOS draws no pixel band and Linux's chain face is already the Regular
/// (`NotoSansCJK-Regular.ttc` beside Noto Sans Regular), so neither has a
/// rung of its own: their East Asian text keeps the terminal cascade.
#[cfg(not(windows))]
const UI_CJK_FILES: [&[(&str, u32, UiCjkScript)]; 2] = [&[], &[]];

/// One face of [`UiCjkFaces`]: its file, face index and script, and the
/// parse, made on first need and shared by every holder of the store.
struct UiCjkFace {
    path: std::path::PathBuf,
    index: u32,
    script: UiCjkScript,
    /// `None` inside: the file is absent or the parse refused it, cached.
    font: OnceLock<Option<Arc<ChromeFont>>>,
}

impl UiCjkFace {
    /// The parsed face, if a parse has landed. Never parses.
    fn parsed(&self) -> Option<&Arc<ChromeFont>> {
        self.font.get().and_then(Option::as_ref)
    }

    /// The parsed face, parsing it on this thread first if no parse has been
    /// tried; the flag says whether THIS call's parse is the one that landed.
    /// The parse runs outside the cell and a racer's duplicate is dropped
    /// (`OnceLock::get_or_init` would make the UI thread wait on the startup
    /// warm's parse, which it must never do).
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

/// The chrome's OWN fallback for East Asian text in a UI-face run (Segoe UI's
/// labels: the tab strip's titles, Settings), at the run's weight: the faces
/// Windows itself falls back to from Segoe UI ([`UI_CJK_FILES`]). A Regular
/// label takes Yu Gothic UI and Malgun Gothic, the selected tab's Semibold
/// label their Semibold and Bold. A face is read and parsed only when a label
/// needs it ([`Self::materialise_for`]: the startup warm, or a band label on
/// the frame that first draws it), and never under the font lock.
pub(crate) struct UiCjkFaces {
    /// Per [`UiCjkWeight::slot`], the faces in fallback order.
    rungs: [Vec<UiCjkFace>; 2],
}

impl UiCjkFaces {
    /// This host's store, nothing read yet; `None` where the host has no
    /// entries ([`UI_CJK_FILES`]).
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

    /// For each char of `text` in an East Asian block ([`ui_cjk_script`]),
    /// have the first face of the `weight` rung for its script that maps it
    /// parsed: the faces before it are parsed and found not to map it, the
    /// faces after it are left alone. Whitespace and controls are skipped.
    /// Answers how many parses this call landed, and the chars the rung cannot
    /// place (outside the blocks, or no face of its script maps them), which
    /// the caller asks the terminal's chain for.
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

    /// Whether the `weight` rung has placed `ch` for good: `Some(true)` when
    /// a parsed face of its script maps it with no unparsed face before it,
    /// `Some(false)` when every face of its script is settled (parsed or
    /// absent) and none maps it, `None` while a face that might map it is
    /// still unparsed. Never parses.
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

    /// How many of the store's faces are parsed: the band key's witness of a
    /// landing no install announces ([`strip_band_font_epoch`]).
    fn parsed_count(&self) -> usize {
        self.rungs
            .iter()
            .flatten()
            .filter(|face| face.parsed().is_some())
            .count()
    }
}

/// The process's one East Asian fallback store: the faces are system files
/// that no reload changes, so a reload keeps every parse ([`install_chrome_faces_locked`]).
#[cfg(not(test))]
fn ui_cjk_store_for_install() -> Option<Arc<UiCjkFaces>> {
    static CJK_STORE: OnceLock<Option<Arc<UiCjkFaces>>> = OnceLock::new();
    CJK_STORE
        .get_or_init(|| UiCjkFaces::host().map(Arc::new))
        .clone()
}

/// Tests: a cold store per install, as a fresh process has, so a test that
/// counts parses or landings never sees another test's (libtest reuses
/// threads, and a process-wide store would be shared between them).
#[cfg(test)]
fn ui_cjk_store_for_install() -> Option<Arc<UiCjkFaces>> {
    UiCjkFaces::host().map(Arc::new)
}

/// The chrome font stack: the user's terminal face + its real bold sibling
/// (both injected by [`set_chrome_fonts`]) over the embedded DejaVu coverage
/// fallback, over the renderer's already-parsed chain faces
/// ([`Self::coverage`]). `primary`/`bold` are `None` until the renderer
/// resolves them (and in unit tests), leaving the deterministic DejaVu-only
/// stack.
struct ChromeFonts {
    primary: Option<ChromeFace>,
    bold: Option<ChromeFace>,
    fallback: Option<ChromeFace>,
    /// The COVERAGE rung under the embedded DejaVu: the committed semantic
    /// renderer's broad-chain faces that are ALREADY parsed
    /// (`Renderer::ready_fallback_faces`), in chain order — NotoSansCJK on
    /// Linux, YaHei and Malgun Gothic on Windows. A char none of the three
    /// chrome faces carry used to fall out of the chrome entirely:
    /// advance-only in a Settings label, and a whole tab title handed to the
    /// cell lane, drawn a half-lip low on the cell baseline and cut by the
    /// seam rule. A memo of `ChromeFace`s over the renderer's own shared
    /// `Arc`s, refreshed by [`Self::sync_coverage`]; nothing is parsed by the
    /// rung itself. The parses land from the terminal drawing the char, from
    /// the startup warm ([`warm_chrome_coverage`]), or from a band label that
    /// needs the face on the frame that draws it ([`strip_band_run_coverable`]).
    coverage: Vec<ChromeFace>,
    /// The UI faces' own East Asian fallback ([`UiCjkFaces`]), installed with
    /// them; `None` before the install and where the host has none.
    ui_cjk: Option<Arc<UiCjkFaces>>,
    /// Per weight and per entry of [`Self::ui_cjk`], a `ChromeFace` over the
    /// entry's parse once it has landed (a memo, refreshed by
    /// [`Self::sync_ui_cjk`]; reset whenever the store is replaced).
    ui_cjk_held: [Vec<Option<ChromeFace>>; 2],
    /// Host-prepared proportional UI faces. These immutable parsed assets are
    /// installed by `set_chrome_fonts`; measure/compile/raster never probe a
    /// platform path or initialize a font lazily.
    ui_regular: Option<Arc<ChromeFont>>,
    ui_semibold: Option<Arc<ChromeFont>>,
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
    /// The candidate a new request parked: the face that was on screen, which
    /// keeps drawing the specimen while the request resolves, whatever its
    /// family (a saved Menlo as well as the committed default).
    semantic_displaced: Option<crate::widget::SemanticFontCandidate>,
    semantic_ready_epoch: u64,
    /// Measured wall time of the last one-time background specimen warmup.
    /// Retained for Diagnostics/tests; it is not part of paint identity.
    semantic_prewarm_ms: Option<u64>,
    /// What words the UI faces draw whole measured, kept ([`MeasureMemo`]).
    measures: MeasureMemo,
}

/// MEASURES OF WORDS A UI FACE DRAWS WHOLE, kept so a page laid out again
/// measures only what it has not measured before: a Settings search that
/// lists most of the registry re-laid every row's words on every key and
/// redraw, glyph by glyph, the same strings at the same sizes. Only a string
/// every char of which the face itself maps is kept: its width and its line
/// breaks then depend on that one font, its size and the width alone, and
/// the font is HELD with them (an install that replaces it drops them, and
/// cannot reuse its address), while a string that reaches a fallback or
/// coverage face — which land lazily — is measured afresh every time.
/// Bounded: past [`MEASURE_MEMO_ENTRIES`] it starts over.
///
/// A hit is a hash of the string: the texts are keyed
/// ([`aterm_hash::KeyedHashMap`], a peer's message or a session's title is
/// among them), the face and size, which aterm makes, are not.
#[derive(Default)]
struct MeasureMemo {
    /// Per face and size: the font measured in, and each string's width.
    widths: aterm_hash::FxHashMap<(u16, u32), HeldMeasures<f32>>,
    /// Per size and line width: the UI font, and each text's line ranges
    /// and its scan count.
    wraps: aterm_hash::FxHashMap<(u32, u32), HeldMeasures<WrapLines>>,
    /// Per rule, face, size and box width: the font, and each text's lines
    /// ([`kept_lines`]).
    lines: aterm_hash::FxHashMap<(LinesRule, u16, u32, u32), HeldMeasures<Vec<String>>>,
    entries: usize,
}

/// A text's line ranges and the scans that found them
/// ([`ui_text_wrap_ranges_impl`]).
type WrapLines = (Vec<std::ops::Range<usize>>, usize);

/// The font some measures were taken in (held), and the measures.
type HeldMeasures<T> = (Arc<ChromeFont>, aterm_hash::KeyedHashMap<Box<str>, T>);

/// How many measures [`MeasureMemo`] keeps before it starts over.
const MEASURE_MEMO_ENTRIES: usize = 16_384;

/// A [`TextFace`] as a memo key.
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
    /// A setting's name on its row's label lines
    /// (`native_settings::row_name_lines`).
    RowName,
}

/// Where a text's kept lines were laid: by which rule, in which face, at
/// which size, in a box how wide.
type LinesAt = (LinesRule, u16, u32, u32);

/// THE LINES `rule` LAYS `text` ON in `face` at `px` in a box `width` wide,
/// as [`MeasureMemo`] keeps a measure: what `lay_out` lays out — which must
/// measure only in `face` at `px` — kept only where the face maps every char
/// of `text`, so the lines depend on that font, `px` and `width` alone. A
/// page laid out again (every screen of a scrolled list, every redraw) finds
/// its lines here instead of measuring every word again.
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

// TEST-ONLY WORK CENSUS: how many strings THIS thread has measured glyph by
// glyph (a width walk or a wrap's advance table), memoized ones not counted:
// what a re-laid page's text work is ([`MeasureMemo`]); and how many of them
// the UI face mapped every char of, the strings the memo can keep (the rest
// reach a fallback face, and are measured afresh every time by design).
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

/// Of [`measure_walks`], the strings the UI face mapped every char of: the
/// ones [`MeasureMemo`] can keep, so a layout that finds them kept walks
/// none of them again (test-only).
#[cfg(test)]
pub(crate) fn measure_walks_whole() -> u64 {
    WHOLE_WALKS.with(std::cell::Cell::get)
}

fn note_measure_walk() {
    #[cfg(test)]
    MEASURE_WALKS.with(|walks| walks.set(walks.get() + 1));
}

/// The string just walked ([`note_measure_walk`]) is one the UI face mapped
/// whole.
fn note_whole_walk() {
    #[cfg(test)]
    WHOLE_WALKS.with(|walks| walks.set(walks.get() + 1));
}

fn default_chrome_fonts() -> ChromeFonts {
    // The cold store is identical in production and tests: backend construction installs UI
    // assets through `set_chrome_fonts`, and direct-view tests cross that transition explicitly.
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

    /// Pure geometry from the already prepared face; no request, worker poll,
    /// font lock, fork, or rasterization is needed while laying out a specimen.
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

    /// The `(width, height)` cell a specimen paints at `px`: pure geometry,
    /// like [`Self::specimen_cell_height`].
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

/// Lock the chrome font stack, recovering from a poisoned mutex (a panicked
/// test thread must not take every later tray raster down with it).
#[cfg(not(test))]
fn lock_fonts() -> std::sync::MutexGuard<'static, ChromeFonts> {
    chrome_fonts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// Test-only work census: how many times this thread has entered the font store. Every text
// measure enters it once, so the count a render adds is its text work in a unit a loaded
// machine cannot stretch. Read through `font_store_visits`.
#[cfg(test)]
thread_local! {
    static FONT_STORE_VISITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The font-store entries this thread has made so far (test-only; see
/// `FONT_STORE_VISITS`): a render's text work is the difference across it.
#[cfg(test)]
pub(crate) fn font_store_visits() -> u64 {
    FONT_STORE_VISITS.with(std::cell::Cell::get)
}

// TEST-ONLY WORK CENSUS: the linear-light source-over composites this thread
// has run (`Canvas::composite_at`'s general arm, three EOTF lookups and three
// OETF quantizations each), the raster's per-pixel cost in a unit a loaded
// machine cannot stretch. Read through `linear_composites`.
#[cfg(test)]
thread_local! {
    static LINEAR_COMPOSITES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The linear-light composites this thread has run so far (test-only; see
/// `LINEAR_COMPOSITES`): a raster's blend work is the difference across it.
#[cfg(test)]
pub(crate) fn linear_composites() -> u64 {
    LINEAR_COMPOSITES.with(std::cell::Cell::get)
}

/// Unit tests construct independent `App`s in parallel. Each app may install a
/// different resolved terminal face, while layout assertions measure and paint
/// in separate calls. Production has one process-global renderer context, but a
/// process-global test font lets an unrelated worker swap the face between those
/// calls. Keep the production shape and isolate only libtest workers by thread:
/// each thread's chrome fonts are a mutex of their own, created cold on that
/// thread's first lock and, like the production store, never freed.
///
/// ONE MUTEX PER THREAD, not one map of every thread's fonts behind one mutex
/// (test-run speed). The guard of the map was held for the whole of every
/// measure and raster, so every test thread that laid out text waited for
/// every other one, each on fonts it never shared: `sample` across a
/// `-p aterm-gui --lib` run at `RUST_TEST_THREADS=7` found test threads parked
/// on that lock in 14,172 of 253,425 samples (5.6%), the largest single lock
/// the binary waited on. Only the owning thread ever takes
/// its mutex, so each lock is uncontended, and what each thread sees is what
/// its map entry held.
#[cfg(test)]
fn lock_fonts() -> std::sync::MutexGuard<'static, ChromeFonts> {
    thread_local! {
        // Keep the per-thread test store production-cold. Tests that construct
        // a semantic view without backend startup cross the same explicit
        // preparation boundary via `prepare_ui_fonts_for_direct_view_test`.
        static FONTS: &'static Mutex<ChromeFonts> =
            Box::leak(Box::new(Mutex::new(default_chrome_fonts())));
    }
    FONT_STORE_VISITS.with(|visits| visits.set(visits.get() + 1));
    FONTS
        .with(|fonts| *fonts)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Install the renderer-resolved chrome faces — `(bytes, collection index)` of
/// the PRIMARY terminal face and its real BOLD sibling (from
/// `Renderer::chrome_primary_face` / `chrome_bold_face`) — so the tray text
/// renders in the user's font. The primary is always the person's font
/// (`font_family`, or Automatic): a display face is a layer over it, never the
/// primary, so Settings' code text, the palette and the Manual keep that font
/// whatever display faces are on. Called by `App::sync_chrome_fonts` after
/// every backend (re)build; unparseable bytes leave that slot on the fallback.
pub(crate) fn set_chrome_fonts(
    primary: Option<(std::sync::Arc<[u8]>, u32)>,
    bold: Option<(std::sync::Arc<[u8]>, u32)>,
    semantic: Option<Renderer>,
) {
    let (p, b) = (chrome_face_for_slot(&primary), chrome_face_for_slot(&bold));
    reload_chrome_fonts_locked(&mut lock_fonts(), p, b, semantic);
}

/// The chrome face for one terminal-face slot: the face itself, parsed.
fn chrome_face_for_slot(slot: &Option<(std::sync::Arc<[u8]>, u32)>) -> Option<ChromeFace> {
    slot.as_ref()
        .and_then(|(bytes, index)| ChromeFace::from_bytes(bytes, *index))
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

/// One chrome-font reload on the locked context, shared by [`set_chrome_fonts`] and
/// [`set_prepared_chrome_fonts`] (`SemanticPrewarmGeneration`'s `Reload`). Installing the
/// faces bumps the semantic generation and drops the job queued for the superseded one in the
/// same critical section, so the worker never forks a renderer that is gone.
fn reload_chrome_fonts_locked(
    fonts: &mut ChromeFonts,
    primary: Option<ChromeFace>,
    bold: Option<ChromeFace>,
    semantic: Option<Renderer>,
) {
    install_chrome_faces_locked(fonts, primary, bold, semantic);
    // The service is owned by backend initialization, never demand-spawned by a
    // semantic compile or paint. Candidate requests remain lazy, but the one
    // parked worker already exists before a Settings view can ask for one.
    if fonts.semantic_worker.is_none() {
        fonts.semantic_worker = SemanticPrewarmWorker::spawn();
    }
    if let Some(worker) = fonts.semantic_worker.as_ref() {
        worker.cancel_queued();
    }
    // The UI faces' own CJK and Hangul faces and the new generation's chain
    // faces, parsed off this thread before a title needs them
    // ([`chrome_coverage_warm`]).
    #[cfg(not(test))]
    if let Some(warm) = chrome_coverage_warm(fonts) {
        let _ = crate::qos::spawn_lane_thread(
            "aterm-chrome-coverage-warm",
            crate::qos::Role::Background,
            warm,
        );
    }
}

/// THE WARM A CHROME-FONT INSTALL SPAWNS ([`warm_chrome_coverage`]): the
/// installed generation's chain faces and the UI faces' own East Asian faces
/// ([`UiCjkFaces`]), or `None` with neither to warm. Every platform's: the
/// chrome draws a char its UI face lacks only from a face it holds parsed
/// ([`ChromeFonts::face_for`]), and on macOS the terminal's chain faces draw
/// through CoreText and are never parsed for the grid, so nothing else ever
/// parsed one there. Warmed on Windows and Linux only, a macOS Settings or
/// Manual label drew a CJK title as blank advances (a windowed capture of
/// Settings > Messages, 2026-10-21: "…" where a Japanese title stood, the
/// band above drawing the same title through the grid).
fn chrome_coverage_warm(fonts: &ChromeFonts) -> Option<impl FnOnce() -> usize + Send + 'static> {
    let chain = fonts.semantic.as_ref().map(Renderer::fallback_coverage);
    let ui = fonts.ui_cjk.clone();
    (chain.is_some() || ui.is_some())
        .then_some(move || warm_chrome_coverage(chain.as_ref(), ui.as_deref()))
}

/// What the startup warm asks the faces for: Han, kana and Hangul. Their
/// faces are the costly ones to parse (MEASURED 2026-10-18, Windows 11,
/// opt-level 2: `msyhbd.ttc` 30-35 ms, `malgunbd.ttf` ~23 ms) and the
/// commonest titles the UI faces cannot draw. Every other script's chain face
/// costs a few ms at most (`Nirmala.ttc` ~5 ms, `LeelawUI.ttf` under 1 ms),
/// cheap enough for the frame that first needs it.
const CHROME_COVERAGE_WARM: &str = "日の한";

/// Parse, on the calling thread, the faces a Han, kana or Hangul label draws
/// with ([`CHROME_COVERAGE_WARM`]), so a CJK tab title is a band title on its
/// very first frame without that frame paying for the parse: first the UI
/// faces' own Regular faces ([`UiCjkFaces`]: every quiet tab, every Settings
/// label), then their Semibold ones (the selected tab, a heading), then the
/// terminal chain's (the chrome's monospace text, and every char the UI
/// faces' own leave to it). Spawned on a `Background` thread by every production
/// chrome-font install ([`reload_chrome_fonts_locked`]): it starts after the
/// first frame (the install is the first-present hook's), takes no lock the
/// UI thread holds, and a UI thread that needs a face before it lands parses
/// it itself (neither store blocks one holder behind another). The chain's
/// parse lands in cells the live terminal shares; the UI faces' store lives
/// for the process, so a reload parses none of them again. Every platform:
/// a macOS chain face draws through CoreText for the grid and is parsed by
/// nothing else, so this is what lets a Settings or Manual label draw one
/// there ([`chrome_coverage_warm`]). Answers how many faces it parsed.
///
/// COST, MEASURED 2026-10-18 on Windows 11 26200 (the default chain:
/// `msyhbd.ttc`, `malgunbd.ttf`): the two chain parses take 55 ms of one
/// background core and add 1.46 MB of private heap (the faces' cmap, advance
/// and box tables); the mapped files' pages they read are clean, shared and
/// reclaimable. Headless startup to the control socket and to the first
/// rendered image did not move (the debug build without and with the warm,
/// alternated, the four warm-cache pairs of five: socket 46-55 ms against
/// 50-60 ms, first image 745-780 ms against 744-796 ms). The UI faces' own
/// four, MEASURED 2026-10-18 on the same machine, opt-level 2, warm file
/// cache, three rounds: `YuGothM.ttc` face 1 36-37 ms, `malgun.ttf` 22-24 ms,
/// `YuGothB.ttc` face 2 39-43 ms, `malgunbd.ttf` 23-31 ms, about 125 ms of the
/// same background core, and 1.70 MB more private commit
/// (`K32GetProcessMemoryInfo`, before and after the warm of a cold store).
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
    fonts.set_ui_cjk(ui_cjk_store_for_install());
    fonts.semantic_generation = fonts.semantic_generation.wrapping_add(1);
    fonts.semantic = semantic;
    // The rung is a memo over the old renderer's chain; `sync_coverage` refills it from the
    // new one on the first miss that asks.
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

/// Test-only: install `renderer` as this test thread's settled semantic chrome font. The
/// worker's install-generation warmup runs inline and no worker is parked, so nothing lands
/// between two paints of a pixel-identity test (production's warmup lands asynchronously).
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
    // Settled, not dormant: the warmup ran inline above and no parked worker can replace the
    // cascade.
    fonts.semantic_prewarm_ms = Some(0);
    fonts.semantic_worker = None;
    fonts.semantic_ready_epoch
}

/// Drive the real request/result channel without a racing worker. The returned
/// fixture runs the queued host warmup inline and publishes the result;
/// production polling, epoch installation and retained rendering stay real.
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
        // Libtest reuses worker threads. A disconnected manual worker must
        // never prevent a later App from parking its real worker at startup.
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

/// Test-only twin of [`install_settled_chrome_fonts_for_test`] that installs `renderer` as
/// production's [`set_chrome_fonts`] does (dormant, no warmup) but parks no worker, so nothing
/// lands through this store later: for a test whose subject is a chain face landing outside the
/// store (the live terminal parsing a cell the seed shares).
#[cfg(test)]
pub(crate) fn install_dormant_chrome_fonts_for_test(renderer: Renderer) -> u64 {
    let mut fonts = lock_fonts();
    install_chrome_faces_locked(&mut fonts, None, None, Some(renderer));
    fonts.semantic_worker = None;
    fonts.semantic_ready_epoch
}

/// The broad CJK face the host's built-in fallback chain leads with, where it
/// is installed — a file `aterm_render` parses, so a test draws with a real
/// ideograph-bearing face rather than a stub.
///
/// macOS USED to be excluded here on the claim that its chain faces raster
/// through CoreText and so "never materialise a fontdue cell". That claim is
/// true of PRODUCTION discovery and false of this seam: the caller below reads
/// the file and hands the bytes to `add_fallback_bytes`, which never consults
/// CoreText. Measured 2026-09-15 — all three system faces parse. Excluding
/// them cost nothing less than four tests that returned before their first
/// assertion and still reported `ok`, on the one platform this repo is
/// developed on.
///
/// `None` only on a genuinely bare host (a Linux CI image with no CJK package).
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

/// Install a SETTLED semantic chrome font whose chain holds the host's CJK
/// face, already parsed — the state the store reaches once the terminal has
/// drawn an ideograph — plus the host UI faces. `false` means the host has no
/// such face; the caller has nothing to assert and ends COULD NOT RUN.
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

/// Install, DORMANT, a semantic seed whose chain holds the host's CJK face
/// UNPARSED — a fresh window's state before anything drew an ideograph: the
/// window's own order (configure, seal, fork the seed) with the cell shared
/// between the sealed generation and the seed — plus the host UI faces.
/// Answers the sealed generation, which keeps the cells alive beside the seed;
/// `None` when the host has no such face (the caller ends COULD NOT RUN).
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

/// Whether THIS test thread's chrome cascade draws `ch` from a face it holds
/// parsed (the terminal stack and the coverage rung, [`ChromeFonts::face_for`]),
/// read without parsing anything.
#[cfg(test)]
pub(crate) fn chrome_cascade_reaches_for_test(ch: char) -> bool {
    lock_fonts().face_for(TextWeight::Regular, ch).is_some()
}

/// Whether THIS test thread's chrome draws `ch` in a run set in `face` from a
/// face it holds parsed: the UI faces' own East Asian fallback at `face`'s
/// weight, then the terminal cascade ([`ChromeFonts::ui_miss_face`]), read
/// without parsing anything.
#[cfg(all(test, not(target_os = "macos")))]
pub(crate) fn chrome_ui_reaches_for_test(face: TextFace, ch: char) -> bool {
    lock_fonts()
        .ui_miss_face(face, TextWeight::Regular, ch)
        .is_some()
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

/// Direct semantic-view tests build no renderer backend, so they install the immutable
/// proportional faces backend startup would, keeping [`default_chrome_fonts`] cold without
/// replacing an installed semantic renderer.
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

/// Replace THIS test thread's East Asian UI fallback ([`UiCjkFaces`]):
/// `None` for a test whose subject is the terminal chain's rung under it (the
/// state of a host with no such faces), or a cold store of its own.
#[cfg(all(test, not(target_os = "macos")))]
pub(crate) fn set_ui_cjk_faces_for_test(store: Option<Arc<UiCjkFaces>>) {
    lock_fonts().set_ui_cjk(store);
}

/// A cold East Asian UI fallback store of this host's faces, nothing parsed;
/// `None` where the host has none (every host but Windows).
#[cfg(all(test, windows))]
pub(crate) fn host_ui_cjk_faces_for_test() -> Option<Arc<UiCjkFaces>> {
    UiCjkFaces::host().map(Arc::new)
}

/// The inverse seam: return this test thread's chrome stack to the cold, UI-face-less store.
/// Libtest reuses worker threads, so a test of the no-UI-face branch cannot assume a cold one.
#[cfg(test)]
pub(crate) fn clear_ui_fonts_for_test() {
    let mut fonts = lock_fonts();
    fonts.ui_regular = None;
    fonts.ui_semibold = None;
    fonts.set_ui_cjk(None);
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

/// Which face of the chrome stack draws a glyph: the pure policy (no font I/O). A bold run
/// whose real bold face covers the char takes the bold face, else a char the primary covers
/// takes the primary; the embedded DejaVu is only a coverage fallback.
///
/// # Invariant (proven)
/// `Fallback` is returned ONLY when neither user face applies
/// (`EmbeddedOnlyAsCoverageFallback`), and a covered bold run always keeps its
/// weight (`BoldHonoredWhenCovered`). The exhaustive 2^3 enumeration in this
/// module's tests proves both on this shipping fn.
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

/// A bundled display face, parsed once per process from its compiled-in
/// bytes ([`TextFace::Display`]).
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

    /// Resolve `(weight, ch)` to the face that draws it, per
    /// [`select_chrome_face`] — and, for a char the embedded DejaVu lacks too,
    /// the first COVERAGE face that carries it ([`Self::coverage`]). The proven
    /// gate is untouched: the rung sits strictly UNDER its `Fallback` pick and
    /// is consulted only once every face the gate knows has missed, so no char
    /// a chrome face covers changes face. `None` when nothing loaded covers
    /// `ch` (the pen then draws one of its own two glyphs — [`synthesized`] —
    /// or advances without ink; the strip band first has the chain face that
    /// maps it parsed, [`strip_band_run_coverable`], and hands the title to
    /// the cell lane only when no face maps it).
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

    /// Bring [`Self::coverage`] up to date with what the committed semantic
    /// renderer's chain has ALREADY parsed. A chain cell is shared with the
    /// live terminal renderer, which materialises it the first time a cell
    /// draws the char, and with the startup warm's handle — so the rung grows
    /// without any install passing through this store, and this read (a
    /// handful of atomic loads; no parse, no I/O) is what notices. Compared by
    /// identity, not by count: a renderer swap
    /// (a candidate landing, a cache hit) can hand back a same-length chain of
    /// different faces.
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

    /// Install `store` as the UI faces' East Asian fallback, dropping the
    /// memo of the store it replaces (kept when it is the same store).
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

    /// Bring the `weight` memo of [`Self::ui_cjk_held`] up to date with the
    /// parses that have landed in the store, from any thread: a few atomic
    /// loads, no parse.
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

    /// Where the UI faces' own East Asian fallback holds a parsed face for
    /// `ch` at `weight`: the entry in `ui_cjk_held[weight.slot()]` of the
    /// first face of `ch`'s script, in fallback order, that maps it. Never
    /// the other weight's: a face that has not landed leaves the char to the
    /// terminal cascade until it does (Yu Gothic UI's kana are proportional,
    /// so even an advance borrowed across weights would be wrong: `の` is
    /// 0.816 em Regular, 0.836 em Semibold). `None` for a char outside the
    /// East Asian blocks.
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

    /// The face that draws `ch` in a run set in the UI face `face` when that
    /// face lacks it: for East Asian text the UI faces' own fallback at the
    /// run's weight ([`UiCjkFaces`], as Windows falls back from Segoe UI),
    /// else the terminal cascade ([`Self::face_for`] at `weight`). The ONE
    /// routing the pen ([`Canvas::text`]), the measures
    /// ([`ui_text_width_for`], the wrap) and the band's coverage question
    /// ([`Self::label_misses`]) share, so what is measured is what is drawn.
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

    /// The distinct chars of the label `s`, set in `face`, that no face the
    /// chrome holds draws as real ink, in order of first appearance: not the
    /// UI face, not its East Asian fallback, not the terminal cascade with its
    /// parsed chain faces ([`Self::ui_miss_face`]), not the pen's own glyphs
    /// ([`synthesized`]). A space always counts as drawn. Exactly the per-char
    /// routing [`Canvas::text`]'s proportional arm performs — except that an
    /// East Asian char counts as drawn only once the rung at `face`'s OWN
    /// weight has placed it ([`UiCjkFaces::settled`]), so the band has the
    /// right weight's face parsed before it draws, never its sibling's.
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
        // Any entry but the face drawing a resolving specimen.
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
        // Parking the active renderer changes what a fork would contain (`semantic` and
        // `semantic_identity` are now `None`), so the epoch moves here too, keeping it a total
        // key over the fork's inputs (the pre-request park in `ensure_semantic_candidate`
        // bumps nothing itself).
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
            // The parked face keeps drawing the specimen meanwhile.
            self.semantic_displaced = self.semantic_identity.clone();
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
        // A candidate still resolving: the face that was on screen, parked
        // in the cache when the request started, keeps drawing the specimen;
        // else the committed cascade; else any face the cache holds. The box
        // is never blank under its "Loading font" status.
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

/// One captured semantic fork, `(candidate, ready_epoch, fork)`: the memo key and the renderer
/// it produced (`None` when that key resolved to no semantic font).
type SemanticForkMemo = (
    crate::widget::SemanticFontCandidate,
    u64,
    Option<Rc<Renderer>>,
);

thread_local! {
    /// The last semantic fork this thread captured, keyed by `(candidate,
    /// snapshot.ready_epoch)`. A fork re-parses the primary face (and its bold sibling), and
    /// [`prepare_semantic_font`] runs on every native compile and preview tick, so it is
    /// memoized. Sharing the `Rc` is unobservable: the captured renderer is immutable
    /// ([`PreparedSemanticFont::fork`] takes `&self` and each consumer forks its own) and its
    /// identity is never compared. `ready_epoch` folds `semantic_ready_epoch` with the
    /// identity-match and pending bits, so it moves on every change to what a fork contains;
    /// the candidate is in the key because those bits are computed against it. Thread-local
    /// because `Renderer` is thread-bound.
    static SEMANTIC_FORK_MEMO: std::cell::RefCell<Option<SemanticForkMemo>> =
        const { std::cell::RefCell::new(None) };
}

/// Host-only request/poll/capture seam. The returned snapshot owns an isolated
/// fork and is the sole semantic-font input allowed past `ViewCx`.
pub(crate) fn prepare_semantic_font(
    candidate: &crate::widget::SemanticFontCandidate,
) -> PreparedSemanticFont {
    let mut fonts = lock_fonts();
    // The request/poll transition and the snapshot run on every call, memo hit or not: the
    // former drives worker convergence and the latter is paint identity. Only the fork is
    // memoized.
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

/// The width of `s` at `px` in the chrome stack: real advances (per-char face pick and
/// fixed-point accumulation, the arithmetic the raster pen uses).
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

/// The cap-height-centred baseline for a `size`-px run in the row `[y0, y0 + row_h)`:
/// `row_center + cap_height/2` (the leading-trim rule), via the machine-checked fixed-point law
/// [`aterm_render::chrome_metrics::baseline_in_row_q`]; top and bottom gaps balance within 1px
/// for any `(row_h, cap)` with `cap <= row_h`. Mixed-size runs on one visual row share the
/// baseline of the dominant size.
pub(crate) fn row_baseline(y0: f32, row_h: f32, size: f32) -> f32 {
    let cap_q = px_to_q(size * chrome_cap_ratio());
    q_to_px(baseline_in_row_q(px_to_q(y0), px_to_q(row_h), cap_q))
}

/// Real regular/semibold system UI faces.  Native chrome must never synthesize a
/// weight by re-striking pixels: it selects an installed face or honestly falls
/// back to the closest real face in the terminal stack.
#[derive(Clone, Default)]
struct UiFontAssets {
    regular: Option<Arc<ChromeFont>>,
    semibold: Option<Arc<ChromeFont>>,
    /// When the regular is a variable face with a `wght` axis (Segoe UI Variable on Win11),
    /// its bytes and the resolved semibold coords, so a pixel-space painter can instance the
    /// real wght-600 cut through `aterm_render::variation::varied_glyph_raster`. `None` for a
    /// static regular; never kept on macOS, which has no strip band to read it.
    #[cfg(not(target_os = "macos"))]
    variable_semibold: Option<UiVariableSemibold>,
}

/// The variable UI face instanced at semibold, for the one painter that instances a variation
/// itself (the Windows strip band's active label, [`crate::tab_bar::pixel_band`]). `ChromeFont`
/// draws only a variable face's default instance, so `ChromeFonts::ui_semibold` stays the
/// static `seguisb.ttf`; this reaches Win11's "Segoe UI Variable Semibold" without pairing the
/// variable file with itself (two wght-400 faces, no contrast).
#[derive(Clone)]
#[cfg(not(target_os = "macos"))]
pub(crate) struct UiVariableSemibold {
    /// The whole font file (aterm-font borrows it per raster) — the SAME
    /// admission the regular face was parsed from, a file mapping when the
    /// file lives on the system font volume (`font_file::admit_font_file`).
    pub(crate) bytes: aterm_render::font::FaceBytes,
    /// Collection index the regular was parsed at.
    pub(crate) index: u32,
    /// The `(tag, value)` instance coords — `wght` pulled to the Fluent
    /// semibold (600), clamped onto the face's own axis; every other axis
    /// (`opsz`) stays at its default, the Text optical size.
    pub(crate) coords: Vec<(u32, f32)>,
    /// The SAME regular face parsed from `bytes`: its Unicode cmap answer
    /// ([`UnicodeChars`]) resolves glyph ids for the varied raster (identical
    /// glyph numbering — one file), and its `kern` pairs are the static
    /// kerning a weight instance shares.
    pub(crate) cmap: Arc<ChromeFont>,
}

/// The CSS/OpenType weight the chrome's `UiBold` stands for (Fluent's
/// "Semibold" — `seguisb.ttf` is 600 too, so the static and variable paths
/// name the same weight).
#[cfg(not(target_os = "macos"))]
const UI_SEMIBOLD_WGHT: f32 = 600.0;

/// Resolve [`UiVariableSemibold`] for a just-parsed regular, or `None` when the face is static,
/// has no `wght` axis, or cannot reach a weight visibly heavier than the regular (a `wght` that
/// tops out near 400 would give `UiBold` a look-alike).
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
    vec![
        // No SF entry: `/System/Library/Fonts/SFNS.ttf` is one variable face (a plain sfnt, not
        // a `ttcf` collection) whose weights are `fvar` instances, and no static SF semibold
        // exists on disk, so it cannot supply the regular + semibold pair a candidate needs.
        // Helvetica Neue is the macOS chrome UI face (docs/INTROSPECTABLE_SURFACES_DESIGN.md
        // §9); SF Pro would be a deliberate appearance change, drawn through
        // `aterm_render::variation` at a resolved wght.
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
        // The Win11 UI cut. `SegUIVar.ttf`'s default `fvar` instance (wght 400, opsz 10.5) is
        // "Segoe UI Variable Text", the face Win11 sets its own chrome in at 12–23 px, and the
        // default instance is what `ChromeFont` draws, so the regular needs no variation work.
        //
        // The semibold stays the static `seguisb.ttf`: pairing SegUIVar with itself would
        // install two wght-400 faces (`resolve_ui_font_assets` cannot see weights) and every
        // `UiBold` heading would silently lose its contrast. The resolver also keeps the
        // variable file and semibold coords (`UiVariableSemibold`) for the one painter that
        // instances wght 600 itself, the strip band's active label.
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

/// The Linux UI face ladder as `(regular, semibold)` paths, first choice first — the
/// list [`ui_font_candidates`] resolves on Linux, and the one a test measures a layout
/// with on ANY host ([`install_linux_ui_fonts_for_test`]), so a page's fit is not
/// judged only in the face of the machine that runs the tests.
#[cfg(any(target_os = "linux", test))]
const LINUX_UI_FONT_PAIRS: [(&str, &str); 6] = [
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
    // GNOME's own UI face, for a GNOME host without fonts-noto-core (a server
    // or minimal install, NVIDIA's DGX OS image): static Regular and Bold
    // cuts, so headings keep a real bold, and NARROWER than Noto ("Match the
    // terminal" is 8.42 em against Noto's 8.96 and DejaVu's 9.52), so pages
    // authored against Noto fit instead of truncating. Measured on such a
    // host: the 12 native-settings paint audits that fail in DejaVu pass in
    // Cantarell, and none fails that passes in Noto. The desktop's own face
    // is not taken instead: Ubuntu Sans ships only as a variable font, and the
    // chrome draws no variation instance, so every UiBold heading would lose
    // its weight.
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
/// ([`LINUX_UI_FONT_PAIRS`]) whose files both parse, so a test can judge a layout's fit with
/// Linux metrics on any host. With `root`, each file is looked up by file name in that
/// directory instead. Answers the regular face's path, or `None` when no pair is readable (the
/// installed faces are then left alone). Call it after building the test's `ViewCx`, which
/// installs the host's own faces ([`prepare_ui_fonts_for_direct_view_test`]).
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

/// The UI face files one `resolve_ui_font_assets` admitted, each distinct path once, plus how
/// many admissions that took. The macOS candidate names `HelveticaNeue.ttc` for both faces, so
/// it is admitted once, through `font_file::admit_font_file`, which maps a file on the system
/// font volume (pages shared with other processes, not 8.9 MB of heap per window). `reads`
/// lives on the value so a test's count is this resolve's alone.
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

/// Parse one UI face; the file bytes ride back beside it so the resolver can
/// keep them for a variable face ([`variable_semibold_of`]), which re-parses
/// them through `aterm-font` to instance a `wght` axis the face type itself
/// does not expose.
///
/// ONE admission of the file, shared every way — the handle returned here, the
/// parsed face (which reads outlines on demand and so holds the file), the
/// [`UiVariableSemibold`] the resolver may build from it, AND the sibling face
/// parsed from the same file ([`UiFontFiles`]). It read into a `Vec` and then
/// made two more copies before `fontdue` was retired, which was invisible
/// while the parsed face kept no bytes at all; and it read the file once per
/// FACE until the admission cache.
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

/// Cold host-preparation path, reached through [`prepared_ui_font_assets`] at backend install
/// or the startup warm, never from semantic compile, measure or raster. The parsed faces then
/// stay immutable and `Arc`-shared.
fn resolve_ui_font_assets() -> UiFontAssets {
    #[cfg(test)]
    UI_FONT_RESOLVE_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut files = UiFontFiles::default();
    resolve_ui_font_assets_with(&mut files)
}

/// [`resolve_ui_font_assets`] over a caller-owned admission cache, so a test
/// can see exactly which files one resolve admitted and how many times.
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

/// The resolved stack, with the variable semibold instanced from the regular's
/// own bytes where a strip band will read it.
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

/// macOS: no strip band instances a variable face, so the regular's bytes are
/// released here rather than retained.
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

/// Populate the two process-global font `OnceLock`s [`set_chrome_fonts`] would otherwise
/// initialise on whichever thread calls it first: the system UI face parse
/// ([`prepared_ui_font_assets`]) and the embedded default store ([`chrome_fonts`]). Both are
/// idempotent and change nothing installed. Run on a startup thread so the first
/// `sync_chrome_fonts` (on the event loop) pays only the face handoff, not a ~300 ms debug /
/// ~30 ms release stall.
#[cfg(not(test))]
pub(crate) fn warm_chrome_font_assets() {
    let _ = prepared_ui_font_assets();
    drop(lock_fonts());
}

/// Tests keep chrome fonts per thread, so a process-global warm means nothing there.
#[cfg(test)]
pub(crate) fn warm_chrome_font_assets() {}

/// The pixel tab strip's font-readiness fingerprint (the Windows + Linux band,
/// [`crate::tab_bar::pixel_band`]): the chrome-face install epoch with the coverage rung's
/// size and the UI regular's presence folded in. Fonts land asynchronously, after the first
/// frames may have painted, so folding this into the band's cache key makes a landing a cache
/// miss with no bespoke invalidation hook.
///
/// `semantic_ready_epoch` moves on every [`install_chrome_faces_locked`] (and on
/// semantic-cascade landings, which merely cost one harmless band rebuild); the
/// presence bit covers the test seam (`prepare_ui_fonts_for_direct_view_test`)
/// which installs UI faces without bumping the epoch. The coverage count covers
/// the landings NO install announces: a chain face parsed into a cell the
/// chrome's semantic seed shares — by the live terminal drawing the char, by
/// the startup warm ([`warm_chrome_coverage`]), or by a band label that needed
/// it ([`strip_band_run_coverable`]). The UI faces' own East Asian faces
/// ([`UiCjkFaces`]) land the same two ways, the warm and a band label, and
/// their parsed count is folded in beside the chain's. Both counts only grow
/// for one seed and one store, and a seed or store swap moves the epoch, so
/// the fold is a total key; the band then re-rasters once. The worker poll
/// and the rung syncs are a `try_recv` and a few atomic loads.
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

/// Whether a real proportional UI face is installed. The Windows strip band declines
/// ([`crate::tab_bar::pixel_band::raster_band`] returns `None`) until it is, so frames before
/// `set_chrome_fonts` lands keep the cell strip rather than a band with mono labels.
#[cfg(any(not(target_os = "macos"), test))]
pub(crate) fn strip_band_ui_ready() -> bool {
    lock_fonts().ui_regular.is_some()
}

/// The variable semibold instance for the strip band's active label, or `None` when the host's
/// UI regular is static (the caller then draws `TextFace::UiBold` through the `ChromeFont`
/// path: the static `seguisb.ttf` sibling, or the regular). Handed out only while the installed
/// regular is the face these bytes were parsed from (`Arc::ptr_eq`), so one file's cmap is
/// never paired with another's outlines.
#[cfg(not(target_os = "macos"))]
pub(crate) fn strip_band_variable_semibold() -> Option<UiVariableSemibold> {
    let installed = lock_fonts().ui_regular.clone()?;
    prepared_ui_font_assets()
        .variable_semibold
        .clone()
        .filter(|variable| Arc::ptr_eq(&variable.cmap, &installed))
}

/// Can the chrome stack draw EVERY char of `s` as real ink — the UI face first,
/// then the terminal cascade ([`select_chrome_face`]: real bold sibling / user
/// primary / embedded DejaVu), then the renderer's already-parsed chain faces
/// ([`ChromeFonts::coverage`]), then the pen's own two glyphs
/// ([`synthesized`])? This is exactly the per-char routing
/// [`Canvas::text`]'s proportional arm performs, asked ahead of time: a char that
/// fails ALL of them is one the pen would silently skip (advance-only), which for
/// a tab TITLE means a hole where a glyph should be. The strip band asks this
/// per label and honestly falls back to the cell-grid painter for any segment
/// that fails — a colour emoji title (never in the broad chain) then renders
/// through the terminal renderer's emoji machinery (mono-quantised, but REAL),
/// instead of vanishing from a proportional run.
///
/// A CJK, Hangul or other fallback-script title is a band title from its FIRST
/// frame: a char only a fallback face maps has that face parsed here, before
/// the band measures or draws the title ([`chain_may_draw_in_a_label`] says
/// which chars ask). East Asian text asks the UI faces' own fallback at the
/// label's weight first ([`UiCjkFaces`]: Yu Gothic UI and Malgun Gothic for
/// a quiet title, their Semibold and Bold for the selected one), then the
/// terminal's chain for what that leaves. The startup warm has normally
/// parsed the CJK and Hangul faces already ([`warm_chrome_coverage`]), and
/// this parses only what it has not: at most the faces the label needs, the
/// same parse the cell lane's raster used to pay on that frame, with the
/// title on the cell baseline in the terminal's face and its separators gone.
/// The parse runs outside the font lock. A title no face maps keeps the cell
/// lane.
///
/// This asks for the Regular label ([`TextFace::Ui`]); the selected tab's
/// label asks [`strip_band_run_coverable_in`] with its own face.
pub(crate) fn strip_band_run_coverable(s: &str) -> bool {
    strip_band_run_coverable_in(TextFace::Ui, s)
}

/// [`strip_band_run_coverable`] for a label set in `face`: the faces parsed
/// for its East Asian text are `face`'s weight's ([`UiCjkWeight::of`]).
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

/// Whether a char the chrome's own faces miss in a label is one the terminal's
/// fallback chain may be asked to draw there ([`strip_band_run_coverable`]):
/// text, in any script. Not a colour emoji, which the band draws in colour
/// (`tab_bar::pixel_band::emoji_label`) — a chain face's monochrome glyph would
/// take its place; not a private-use icon, which the cell lane routes through
/// the chain's own rules for icons (scoped faces, the bundled Nerd Font); not a
/// joiner, a variation selector or a control, which draw nothing.
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

/// One colour emoji the chrome draws inline in a label (the tab strip's
/// titles, `tab_bar::pixel_band`): the colour face's own straight-alpha RGBA,
/// `width × height`, at the size it was asked for.
#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug)]
pub(crate) struct ChromeEmoji {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) rgba: Vec<u8>,
}

/// The colour glyph of the emoji grapheme `cluster` at `px` device px, from
/// the chrome stack's semantic renderer: the terminal's own font set, its
/// colour face included, so a tab title draws the emoji the grid draws. A
/// title holding one used to go whole to the cell lane, a monospace slab whose
/// row sits a lip low in a real window's band (the 2026-10-18 judges'
/// `emoji-title-tab-slab-in-new-strip`). `None` before that renderer lands and
/// for a cluster its colour face has no glyph for. The fork at `px` and each
/// cluster's raster are memoized per semantic generation on this thread
/// (`Renderer` is thread-bound), so a strip rebuild pays a hash probe.
#[cfg(any(windows, target_os = "linux"))]
pub(crate) fn chrome_colour_emoji(cluster: &str, px: f32) -> Option<Arc<ChromeEmoji>> {
    /// `(semantic generation, px bits, fork, rasters by cluster)`.
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
        // The colour face alone: shaping the cluster there never reaches the
        // fallback chain, whose first CJK face may not be parsed on this thread.
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
/// panel sizes. Applied identically by the raster pen ([`Canvas::text`]), the measure
/// ([`ui_text_width`]) and the strip band's variable-instance pen (`tab_bar::pixel_band`), so
/// none of them drift.
pub(crate) const UI_TRACKING_EM: f32 = -0.008;

/// The fractions of a pixel the UI pen places a glyph at: quarters, so a
/// glyph sits within an eighth of a pixel of its true place.
const PEN_PHASES: f32 = 4.0;

/// The whole pixel at or left of `pen` and the quarter of a pixel past it
/// (`0..4`), rounded to the nearest quarter: where the UI pen draws a glyph
/// whose true place is `pen`.
fn pen_phase(pen: f32) -> (i32, u8) {
    let quarters = (pen * PEN_PHASES).round();
    let whole = (quarters / PEN_PHASES).floor();
    let phase = (quarters - whole * PEN_PHASES).clamp(0.0, PEN_PHASES - 1.0);
    (whole as i32, phase as u8)
}

/// Width of `s` at `px` in the UI face: real advances plus `horizontal_kern` plus the
/// [`UI_TRACKING_EM`] the raster pen applies. Without a UI face it falls back to the mono
/// approximation (0.6 em per char, [`crate::settings::text_w`]), as [`Canvas::text`] does. The
/// settings painter measures every UI-face string with this, so painter and rasterizer agree.
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

/// [`ui_text_wrap_ranges_impl`]'s measure, and the UI font where it mapped
/// every char of `text` (so the breaks depend on that font alone,
/// [`MeasureMemo`]).
fn ui_text_wrap_walk(
    text: &str,
    px: f32,
    maximum_width: f32,
) -> (WrapLines, Option<Arc<ChromeFont>>) {
    note_measure_walk();
    // White space a line may break after: not a no-break space, which the
    // shared rule keeps as glue ([`aterm_grapheme::is_break_space`]).
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

    // Measure each char at the advance the pen draws it with: for a char the UI face lacks,
    // the cascade face's, not the UI face's `.notdef` (an ideograph is ~1 em, not ~0.6, and a
    // wrap charged the placeholder overruns its box). Charged as `ui_text_width_for` charges
    // it: tracking only for a char the UI face covers, and no kern into or out of a fallback
    // char.
    let mut fonts = lock_fonts();
    let font = fonts.ui_font(TextFace::Ui).cloned();
    // A MAP, not a scan: this function's doc promises a LINEAR operation count,
    // and a linear probe per character would make it quadratic in the number of
    // distinct characters — which is exactly the CJK paragraph this measure was
    // corrected for, where the distinct-character count IS the text length.
    // ASCII, nearly every char of the copy, is charged from a table indexed by
    // the char, which costs no hash: the map's lookups were a third of every
    // Settings page's build (2026-10-18).
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
            // A fallback char is drawn by another face: no kern out of it.
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
    // Where else a line may break than after white space: between CJK
    // characters, kinsoku kept ([`aterm_grapheme::line_breaks`], the rule
    // every wrap of the chrome reads), the bytes the next line would start
    // at. ASCII text has none, and wraps exactly as it always did.
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
    // Where white space is no place to break: before a run of nothing but
    // closing punctuation or slashes, which goes with the word before it
    // ([`aterm_grapheme::never_starts_a_line`], UAX #14 LB13): a spaced
    // slash ends its line ("64 MiB /" / "2,048-file"), never starts one.
    // The grapheme indices after each space that leads up to such a run.
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
            // A SPACE THAT RUNS PAST THE BOX ENDS THE LINE: it paints nothing
            // (callers trim a line's trailing space), so the word before it
            // is whole on this line, the spaces after it go with it, and the
            // next line starts at the next word. Charging it against the box
            // broke a line a word early wherever its last word ended within
            // a space's width of the edge: a setting's note wrapped 4pt short
            // of its box ("Safe says yes only" / "to requests…" in 122pt,
            // where "…only to" is 120.6). A no-break space is glue, no space
            // a line ends at ([`aterm_grapheme::is_break_space`]).
            let split = if whitespace {
                let mut split = index + 1;
                while graphemes.get(split).is_some_and(|(_, _, space)| *space) {
                    split += 1;
                }
                // What follows goes with the word before the spaces: the
                // line ends at the break before that word instead.
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
    // The rest is the last line — none where the spaces a line ended at ran
    // to the text's end, which would be an empty line under it.
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

/// Exact, whitespace-preserving UI-face line breaks: after white space, or
/// between CJK characters with kinsoku kept ([`aterm_grapheme::line_breaks`]),
/// else inside a token too long for the line. The returned ranges are
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
///
/// A char the UI face LACKS measures at the advance of the face the pen will
/// draw it with — the UI face's own East Asian fallback at `face`'s weight,
/// else the chrome cascade's regular pick ([`ChromeFonts::ui_miss_face`]; the
/// strip band draws every label as a regular-weight prim, and a mono
/// primary's bold sibling shares its advances anyway) — with no kern into or
/// out of it and no tracking, exactly as [`Canvas::text`]'s proportional arm
/// advances. It used to charge the UI face's own `.notdef` advance (~0.6 em)
/// for a glyph the pen then drew at the cascade face's real advance (~1 em for
/// an ideograph), so a CJK label fitted to its span by this measure overran it
/// under the pen, and the band's centring, which subtracts this width, was off
/// by half the difference. A char no loaded face covers measures the pen's own
/// glyph where it has one ([`synthesized`]), else its 0.6 em placeholder.
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

/// The installed font [`ui_text_width_for`] measures `face` in, or `None` with no
/// UI face installed (each scalar then measures 0.6 em). A caller that keeps a
/// measure keys it on this font's identity (`Arc::ptr_eq`), and holds it, so a
/// later install cannot reuse its address.
pub(crate) fn ui_font_of(face: TextFace) -> Option<Arc<ChromeFont>> {
    lock_fonts().ui_font(face).cloned()
}

/// Whether every scalar in any substring advances at least `minimum` pixels
/// under the same routing, tracking and kerning used by the proportional pen.
/// A substring's first scalar loses its incoming kern. Both sides of a UI
/// kern pair must keep the floor: prefix growth adds the right side, while
/// shortening a suffix removes the left side.
pub(crate) fn ui_text_advances_at_least(face: TextFace, s: &str, px: f32, minimum: f32) -> bool {
    ui_text_width_with_floor(&mut lock_fonts(), face, s, px, minimum).1
}

/// One linear actual-font walk for width and the optional scalar floor.
/// Width preserves the pen's arithmetic; a failed floor never changes it.
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

/// [`ui_text_width_with_floor`]'s walk, and whether the face itself mapped
/// every char (so the width depends on that font alone, [`MeasureMemo`]).
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

/// [`ui_text_width_with_floor`]'s pen: the width so far, and the char (with
/// its advance) a kern into the next one pairs with. The whole-string measure
/// pushes it through every char; [`ui_text_widths_at`] copies it at each cut,
/// so the two add the same terms in the same order and agree to the bit.
/// `whole`: the face itself mapped every char so far ([`MeasureMemo`]).
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
    /// Moves the pen past `ch`, and says whether every advance it charged
    /// (each side of a kern pair included) keeps `fits`.
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

/// What [`ui_text_width_for`] measures `text[..cut] + tail` at, for each of
/// `cuts` (ascending byte offsets into `text`, each on a char boundary), in
/// ONE pass of the pen: an elider that measured each shorter candidate afresh
/// paid a whole measure per grapheme it shed. Each width is the one the
/// whole-string measure gives that candidate, to the bit.
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

/// sRGB EOTF lookup: a gamma-encoded byte (0..=255) → linear-light (0..1), built once. Mirrors
/// `aterm_render::srgb_to_linear` so the tray composites in the same space as the terminal
/// glyph path.
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
/// Raster fills call this per channel of every covered pixel, so it avoids `powf`: the 8-bit
/// result is a monotonic threshold search (output `n + 1` once linear light crosses the EOTF
/// of `(n + 0.5) / 255`). The 255 thresholds are built once; a coarse linear-light index gives
/// the answer or its neighbour and the exact thresholds correct it, preserving the rounded
/// byte.
fn linear_to_srgb_u8(l: f32) -> u8 {
    const BUCKETS: usize = 8_192;
    struct Quantizer {
        thresholds: [f32; 255],
        coarse: [u8; BUCKETS + 1],
    }

    let l = l.clamp(0.0, 1.0);
    static QUANTIZER: OnceLock<Quantizer> = OnceLock::new();
    let quantizer = QUANTIZER.get_or_init(|| {
        // Find the first representable non-negative f32 the direct OETF rounds upward at each
        // byte boundary. Searching float bit patterns is monotonic on [0, 1] and keeps the
        // last-bit behaviour around a halfway value, where a closed-form inverse can be off by
        // one ulp.
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

/// A glyph memo key: the face's address (0 for the pen's own glyphs), `px` bits, the
/// char, and the pen's quarter of a pixel ([`pen_phase`], 0 wherever it draws on whole
/// pixels).
type GlyphCacheKey = (usize, u32, char, u8);
type GlyphCacheEntry = (ChromeMetrics, std::sync::Arc<[u8]>);

/// The last few composites of one rounded box, so of one colour
/// ([`Canvas::blend_rim`], [`Canvas::blend_span`]): `(pixel under, coverage
/// bits)` to the pixel it became, replaced oldest first. Eight hold a side's
/// antialiasing ramp and the interior's first pixel from row to row.
#[derive(Default)]
struct RimMemo {
    entries: [Option<RimComposite>; 8],
    next: usize,
}

/// One [`RimMemo`] entry: `(pixel under, coverage bits)` and the pixel it
/// became.
type RimComposite = ((u32, u32), [u8; 4]);

impl RimMemo {
    fn get(&self, key: (u32, u32)) -> Option<[u8; 4]> {
        self.entries
            .iter()
            .flatten()
            .find(|(held, _)| *held == key)
            .map(|(_, became)| *became)
    }

    fn put(&mut self, key: (u32, u32), became: [u8; 4]) {
        self.entries[self.next] = Some((key, became));
        self.next = (self.next + 1) % self.entries.len();
    }
}

struct Canvas {
    /// Device-pixel origin of this retained tile in the full tray. Raster math
    /// stays in full-surface coordinates so a regional pass makes the exact
    /// same AA/snapping decisions as a fresh full raster.
    origin_x: i32,
    origin_y: i32,
    w: u32,
    h: u32,
    px: Vec<u8>,
    /// Per-raster glyph memo: a native page repeats a small alphabet across many text runs,
    /// and `ChromeFont` does not cache rasters. It drops with the canvas (no cross-font or
    /// unbounded process state).
    glyphs: std::collections::HashMap<GlyphCacheKey, GlyphCacheEntry>,
    /// Device-px clip stack; each entry is the running intersection as `(x0, y0, x1, y1)`
    /// exclusive. `blend` rejects pixels outside the top entry. Empty ⇒ unclipped. Driven by
    /// `DrawPrim::ClipPush`/`ClipPop`.
    clip: Vec<(i32, i32, i32, i32)>,
    /// Memo for [`Self::blend_glyph_texel`]'s coverage remap: the `(ink, ground)`
    /// pair it was filled for and the corrected coverage per coverage byte
    /// (`NaN` = not yet computed). A run of text sits on one fill, so the pair
    /// rarely changes and each byte costs its `powf` once per raster.
    glyph_remap: (u32, u32, [f32; 256]),
    /// The open fade layers, the innermost last ([`DrawPrim::FadePush`]):
    /// what is drawn inside one is drawn whole, and its pop fades it as one
    /// picture ([`Self::pop_fade`]).
    fade: Vec<FadeLayer>,
}

/// One open fade ([`DrawPrim::FadePush`]): its band in device px and the
/// pixels of the region it may change as they were when it opened, so its pop
/// can fade everything drawn inside it AS ONE LAYER — a panel, the text on it
/// and an opaque terminal frame over the panel fade together, with no prim
/// fading over another faded prim (and so reading more opaque where it lies
/// on top) and no prim left unfaded.
struct FadeLayer {
    /// The band's top and bottom (device px) and the opacity at each.
    top: f32,
    bottom: f32,
    from: f32,
    to: f32,
    /// The device rect the layer may change: the canvas, under the clip the
    /// layer opened in, from the band's first faded row down. Empty where
    /// nothing is drawn (a replay that only tracks the stacks).
    region: DeviceRect,
    /// `region`'s pixels as they were when the layer opened, row by row.
    under: Vec<u8>,
}

impl FadeLayer {
    /// How opaque the layer leaves device row `y` (1: untouched): eased out
    /// from `from` at the band's top to `to` at its bottom, so it starts with
    /// no step and falls fastest where it starts.
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
        // Native retained surfaces start transparent, which `vec![0]` already is; only a
        // non-transparent backdrop pays the O(width × height) fill.
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

    /// [`Self::raster_glyph`] with the pen `phase` quarters of a pixel right
    /// of a whole pixel ([`pen_phase`]); each phase is memoized on its own.
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

    /// [`Self::raster_glyph`] for a glyph the pen draws itself
    /// ([`synthesized`]), cached under the null face pointer — an address no
    /// parsed face can occupy. `None` for a char the pen does not draw.
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

    /// Intersect device-px rect `(x, y, w, h)` with the current clip and push it.
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

    /// Pop the most recent clip. An unbalanced pop is a harmless no-op.
    fn pop_clip(&mut self) {
        self.clip.pop();
    }

    /// Open a fade layer over the band `top..bottom` (device px), `from` to
    /// `to` opaque. Where `draw` is false (a replay that only tracks the
    /// stacks) the layer holds nothing and its pop changes nothing.
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
            // The first row the band dims: its centre at or under `top`.
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

    /// Close the innermost fade layer: every pixel of its region drawn inside
    /// it is mixed back toward what was under it by the band's opacity at
    /// its row, in premultiplied linear light — exactly drawing the layer
    /// whole at that opacity over what it covered (`(o·L) over U` is
    /// `U + o·((L over U) − U)`). A pixel nothing changed costs a compare; a
    /// run of equal changes along a row composites once. An unbalanced pop is
    /// a harmless no-op.
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

    /// Composite `c` (straight alpha) over pixel `(x, y)` with extra `cov` (0..1) AA, in
    /// linear light (as the main glyph path blends), honoring the clip stack.
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
        let cov = cov.clamp(0.0, 1.0);
        let a = (f32::from(c[3]) / 255.0) * cov;
        if a <= 0.0 {
            return;
        }
        let i = ((local_y as u32 * self.w + local_x as u32) * 4) as usize;
        self.composite_at(i, c, cov, a);
    }

    /// [`Self::blend`] at one coverage across `x0..x1` of row `y` (a rounded
    /// box's interior, or the straight run of its top or bottom rim), byte for
    /// byte what blending each pixel of it gives: the span is cut to the canvas
    /// and the clip stack as each pixel would be.
    ///
    /// Typing into Settings search re-rastered the whole page on every key,
    /// and three quarters of that raster was this interior: every card, row and
    /// field is a panel, translucent over the panel under it, and each of its
    /// pixels paid a linear-light source-over (three EOTF lookups and three
    /// OETF quantizations) for the same answer as its neighbour. The result is
    /// a function of the pixel under it alone, so a run of equal pixels under
    /// the panel (a flat fill — the common case) costs one composite and
    /// copies, and an opaque full-strength panel is a plain fill.
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

    /// [`Self::blend`] for one pixel of a rounded box's antialiased rim, where
    /// `memo` holds the box's recent rim results: a pixel whose coverage and
    /// whose pixel under it repeat one of them takes that one's result. A
    /// box's side repeats the same few coverages over the same ground on every
    /// row, so after its first row its sides composite nothing; byte for byte
    /// [`Self::blend`] all the same.
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

    /// The pixel at byte `i` as one word, for comparing pixels whole.
    fn pixel_word(&self, i: usize) -> u32 {
        u32::from_ne_bytes([self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]])
    }

    /// Source-over of `c` at coverage `cov` (`a` its effective alpha, above
    /// zero) onto the pixel at byte `i`: [`Self::blend`]'s arithmetic once the
    /// pixel is known to be on the canvas and inside the clip.
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
        // Equal straight-alpha colours remain that colour under source-over;
        // only alpha changes. This is the common full-coverage shadow case
        // (black over transparent/black) and avoids needless colour transfer.
        if cov >= 1.0 && self.px[i] == c[0] && self.px[i + 1] == c[1] && self.px[i + 2] == c[2] {
            let out_a = a + da * (1.0 - a);
            self.px[i + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
            return;
        }
        #[cfg(test)]
        LINEAR_COMPOSITES.with(|n| n.set(n.get() + 1));
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

    /// Composite one GLYPH coverage texel: [`Self::blend`] after the terminal
    /// renderer's perceptual coverage remap ([`aterm_render::correct_alpha`],
    /// the default `TextBlending::LinearCorrected`), keyed on the ink and the
    /// fill already under the texel.
    ///
    /// Plain linear-light coverage puts a half-covered dark-on-light texel at
    /// its LINEAR midpoint: black over white at 50% lands on sRGB ~0xBC, so a
    /// terminal-font stem that straddles two columns never gets darker than a
    /// mid grey and light-theme chrome text (the command palette, the theme
    /// cards) reads washed out beside the same font in the terminal, which
    /// has always remapped. The remap moves the blended luminance to where a
    /// gamma-space blend puts it (~0x80), chroma still mixing in linear light.
    /// Endpoint bytes stay exact: 0 draws nothing and 255 is the plain blend.
    /// A texel over a fully transparent canvas has no ground to key on and
    /// takes the plain coverage.
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
    /// The exact `Rc` the frame was forked from, held weakly: the weak handle keeps the `Rc`
    /// allocation reserved, so no later fork can reuse the address and alias this entry,
    /// without keeping the forked `Renderer` and its faces alive.
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
/// renderer, except the identities matched separately: the fork source
/// (compared by pointer beside this key), the engine snapshot (represented by
/// `input_fingerprint`, which `settings_preview::build_terminal_specimen_input`
/// builds as a total function of the snapshot it returns) and the cursor scene
/// played onto it (its own fingerprint).
///
/// Every field must appear here. Omitting one — a variation axis, `stem_gamma`,
/// the device scale — would paint stale pixels after a settings tweak, which is
/// exactly the WYSIWYG contract this specimen exists to honour.
fn terminal_specimen_frame_key(spec: &TerminalSpecimenSpec, scale: f32) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hash = std::collections::hash_map::DefaultHasher::new();
    specimen_renderer_key(spec, scale).hash(&mut hash);
    spec.input_fingerprint.hash(&mut hash);
    spec.motion
        .as_ref()
        .map(|scene| scene.fingerprint)
        .hash(&mut hash);
    // A different picture, dim or tint draws different pixels.
    spec.wallpaper
        .as_ref()
        .map(crate::widget::SpecimenWallpaper::identity)
        .hash(&mut hash);
    hash.finish()
}

/// `input` with `wallpaper` prepared for the frame a renderer with this
/// metrics draws it at ([`crate::wallpaper_present::prepare`], the terminal
/// present's own preparation): the base layer under default backgrounds and,
/// with the tint on, default-foreground glyphs tinted toward the picture.
/// One frame's preparation is kept, so a steady preview never rescales.
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

/// Everything the configured renderer fork is built from: the frame key less
/// the snapshot and the scene, so one fork renders every frame of a moving
/// preview.
fn specimen_renderer_key(spec: &TerminalSpecimenSpec, scale: f32) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hash = std::collections::hash_map::DefaultHasher::new();
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

/// Render one specimen frame on one of this thread's configured forks for
/// `spec`, forking only when no held fork matches the renderer key and fork
/// source, then play the cursor scene (if any) onto the snapshot at the fork's
/// own cell metrics.
/// The finished frame of a terminal specimen at `scale`: the retained one
/// where the same computation ran before, else rendered (and retained for a
/// still sample).
///
/// This is by far the most expensive prim in the tray: the fork is a full
/// `fontdue` re-parse of the primary face (and a second one for its real bold
/// sibling) inside `fork_semantic_surface`, and `render_input` then renders a
/// whole mini terminal frame from cold glyph/layout caches. A still preview
/// re-rasterizes with every one of those inputs unchanged, so the finished
/// frame is memoized; a moving one keeps its configured fork, so only
/// `render_input` runs per frame.
///
/// A hit is the same computation replayed: the key is total over what
/// reaches the renderer, the fork source is matched by POINTER identity
/// rather than by value, and the frame itself does not depend on where it
/// is placed.
fn specimen_frame(spec: &TerminalSpecimenSpec, scale: f32) -> Option<Rc<aterm_render::Frame>> {
    let key = terminal_specimen_frame_key(spec, scale);
    let source = spec.prepared_font.renderer.as_ref();
    if let Some(frame) = cached_specimen_frame(key, source) {
        return Some(frame);
    }
    let frame = Rc::new(render_specimen_frame(spec, scale)?);
    // A typing scene never repeats a frame soon enough to pay for a slot,
    // and holding one would evict the still cards sharing the page.
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
        /// Most-recently-used first, bounded like the frame memo.
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
        // The wallpaper is prepared for this fork's exact frame, the size the
        // renderer admits a base layer at.
        let walled = spec.wallpaper.as_ref().map(|wallpaper| {
            specimen_with_wallpaper(&spec.input, wallpaper, spec.theme, frame_size)
        });
        let input = walled.as_ref().unwrap_or(&*spec.input);
        Some(match spec.motion.as_deref() {
            Some(scene) => {
                let input = scene.decorate(input, renderer.cell_size());
                let mut frame = renderer.try_render_input(&input).ok()?;
                // The live terminal's post passes, on the CPU: its bloom
                // kernel and selected crown (`aterm_render::bloom`).
                // A refused post pass is not retained or presented as a finished specimen.
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

/// One specimen frame as [`Canvas::terminal_specimen`] renders it, bypassing
/// the frame memo: the shipping renderer's own output for `spec` at `scale`
/// (on this thread's configured fork), the cursor scene played on that
/// renderer's cells, for tests that compare the tray blit against it.
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

        // A rounded box inset by one pixel lies inside the region whose analytic coverage is
        // exactly 1: fill that span without an SDF per interior pixel and keep the SDF on the
        // edge and corner fringe, so the output bytes are unchanged.
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

    /// The antialiased rim of a rounded box (`(cx, cy, hx, hy, r)`, as
    /// [`sd_round_box`] takes it) across `x0..x1` of row `py`. Between the
    /// box's corners, wherever a pixel is no nearer a side than the row is to
    /// the top or bottom (`qx <= min(qy, 0)`: the top and bottom rims, and the
    /// whole width of a hairline too thin for an interior), the distance does
    /// not read `x`: that run is one coverage, blended as a span. The corners
    /// and the sides take the distance pixel by pixel. Byte for byte the
    /// per-pixel rim.
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

    /// A band icon ([`DrawPrim::BandIcon`]) in an `n`-pixel square at `(x, y)`
    /// device pixels: the band's own coverage raster, fitted to a cap height
    /// of about two thirds of the square and a stem of a ninth, blended in
    /// `c` at each pixel's coverage.
    fn band_icon(&mut self, x: f32, y: f32, size: f32, icon: aterm_render::BandIcon, c: [u8; 4]) {
        let n = size.round().max(2.0) as usize;
        let side = n as f32;
        let metrics = aterm_render::procedural::IconMetrics {
            baseline: (side * 0.84).round(),
            cap: (side * 0.68).round(),
            stem: (side / 9.0).round().max(1.0),
            head: (side * 0.08).round(),
        };
        let coverage = aterm_render::procedural::band_icon_coverage(icon, n, n, metrics, false);
        let (x0, y0) = (x.round() as i32, y.round() as i32);
        for (row, line) in coverage.chunks(n).enumerate() {
            for (col, &cov) in line.iter().enumerate() {
                if cov == 0 {
                    continue;
                }
                // `blend` clips to the canvas and the clip stack.
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

    /// Raster a round-capped anti-aliased segment from the signed distance to its centre line.
    /// A shared primitive, not an icon bitmap, so regional and full rasters draw identical
    /// arrows at every display scale.
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
    /// `frac >= 1.0` draws the full ring (a ring's track).
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

    /// Draw `s` with its baseline at `baseline` (device px). `Mono` draws in the chrome
    /// terminal stack: a per-char face pick ([`select_chrome_face`], then
    /// [`ChromeFonts::coverage`], then [`synthesized`]) on a 26.6 fixed-point pen with
    /// per-glyph rounding (placement error ≤ 0.5 px, no drift). `Ui`/`UiBold` draw in real
    /// regular/semibold system faces (Helvetica Neue, Segoe UI, Noto Sans or DejaVu Sans; the
    /// terminal stack is the final fallback) with real kerning and [`UI_TRACKING_EM`]. Both
    /// place glyphs by their bearings on the caller's baseline, so a coverage face's ideograph
    /// sits on the Latin baseline too.
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
        // Native proportional UI face with PER-CHARACTER coverage fallback.
        // Fontdue will happily rasterize glyph 0 as a visible tofu box, so a
        // run-level face choice is not sufficient: keep the real UI face for
        // every covered character, and route only each miss through the UI
        // face's own East Asian fallback at the run's weight, then the terminal
        // cascade ([`ChromeFonts::ui_miss_face`]). This is what keeps labels
        // proportional while arrows/CJK/etc. remain genuine glyphs rather than
        // `.notdef`, and a Regular label's ideographs Regular.
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

            // Preserve following-label placement without ever drawing `.notdef`.
            pen += px * 0.6;
        }
    }

    /// Render a complete bounded terminal snapshot with the shipping CPU
    /// renderer, then composite its exact opaque frame into the semantic card.
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

    /// Place a rendered terminal frame with its top-left at device `(x, y)`.
    /// Every frame pixel is opaque, so it REPLACES what is under it (what
    /// `blend` does for an opaque, fully covered source): one clipped span
    /// per row is written straight into the canvas, never a per-pixel blend.
    /// Inside a fade the frame is part of its layer, faded with the panel
    /// under it when the layer pops ([`Self::pop_fade`]).
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

    /// Draw `ch` of `font` with its pen at `pen` (device px, fractional) on
    /// `baseline`, at the pen's quarter pixel ([`pen_phase`]): each glyph
    /// keeps its true place in the run, so small text's letter gaps stay
    /// even rather than each glyph snapping to a whole pixel. Its advance.
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

    /// A crisp, fully covered axis-aligned device-px rectangle snapped to the integer grid: the
    /// building block for hairline rules and the text caret, which the rounded SDF would smear
    /// across two device rows.
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

    /// Stroke (outline) a rounded rect with line `width`, centered on the rect edge. A
    /// near-square outline (`radius <= 0.5`, or too small or thin to round) is four
    /// device-px-snapped spans, so 1px frames, carets and hairlines stay crisp; a rounded one
    /// uses the analytic SDF (`cov(|sd| − width/2)`) for its corner AA. All inputs are device px.
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
            // A crisp frame: top and bottom spans, then the sides between them, so translucent
            // strokes never double-blend the corners.
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
    // OCCLUSION: where a terminal specimen (an opaque frame) covers most of
    // the tile, nothing drawn before it shows where it lands, so what comes
    // before it is drawn only in the strips of the tile around it, under
    // those prims' own clips; then everything from it on over the whole
    // tile. The bytes are the same as drawing every prim everywhere (the
    // frame replaces what is under it, `blit_terminal_frame`): a preview
    // tick of a cursor specimen no longer re-blends the backdrop, the
    // section and card panels and the terminal panel under the frame.
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
    // A fade never popped still fades everything after its push.
    while !cv.fade.is_empty() {
        cv.pop_fade();
    }
    cv.px
}

#[cfg(test)]
thread_local! {
    /// How many prims [`replay_tray`] drew on this thread: a test's evidence
    /// that the prims an opaque specimen covers were not drawn under it.
    pub(crate) static DRAWN_PRIMS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// A device-pixel rect as (left, top, right, bottom), the shape
/// [`Canvas::device_bounds`] answers in.
type DeviceRect = (i32, i32, i32, i32);

/// A device-space clip, using the canvas's exact floor/ceil arithmetic.
/// Callers scale the origin and size separately before converting: adding
/// in logical space first can round a fractional edge to a different pixel.
fn device_clip_rect(x: f32, y: f32, w: f32, h: f32) -> DeviceRect {
    (
        x.floor() as i32,
        y.floor() as i32,
        (x + w).ceil() as i32,
        (y + h).ceil() as i32,
    )
}

/// The specimen prim that covers at least half of `cv`'s tile, as its frame
/// lands under the clips active there (the most such area; the later on a
/// tie), and the device rect it covers. Its frame (and those of the specimens
/// before it that were rendered to measure them) go in `frames`, so the
/// raster blits them rather than rendering them again. `None` where no
/// specimen covers that much.
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

/// The device rect the occluding specimen covers of `region`
/// ([`occluding_specimen`]), for a test that rasters exactly that rect.
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

/// `tile` less `covered` (inside it), as up to four rects: the bands above
/// and below, and the two sides between.
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

/// Draw `range` of `prims` onto `cv` (only their clip and fade pushes and
/// pops where `draw` is false), carrying the depth of clips that miss the
/// tile in `skipped`. A specimen whose frame is in `frames` is blitted from
/// it.
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
                // The track: the full circle.
                cv.arc(cx * s, cy * s, r_mid, th, 1.0, *track);
                // The arc over it.
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

    /// A PANEL'S INTERIOR IS BLENDED ONCE A RUN, BYTE FOR BYTE AS EACH PIXEL
    /// (r8 D3). Typing into Settings search re-rastered the page on every key,
    /// and three quarters of it was panels' interiors: each pixel of a
    /// translucent card paid its own linear-light composite for the answer its
    /// neighbour had. `blend_span` gives the span the bytes `blend` gives each
    /// of its pixels — under every fade, clip and tile origin, over pixels that
    /// vary and pixels that repeat, opaque and translucent — and composites a
    /// run of equal pixels once.
    #[test]
    fn a_span_blends_byte_for_byte_as_each_of_its_pixels() {
        // A deterministic mix of runs and singles under the span.
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

    /// A ROUNDED BOX DRAWS THE BYTES ITS PIXELS DREW ONE BY ONE (r8 D3): the
    /// interior span and the rim memo against the per-pixel rounded box they
    /// replace (every pixel through `blend`, the SDF on the rim), for boxes at
    /// fractional positions, of every radius, opaque and translucent, under a
    /// fade and a clip, on tiles with an origin, over pixels that vary and
    /// pixels that repeat.
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
            // Hairlines and rules: no interior, the whole width a rim.
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

    /// A TRANSLUCENT CARD OVER A FLAT ONE COMPOSITES A HANDFUL OF TIMES, NOT
    /// ONCE A PIXEL (r8 D3): a 600x400 card at 60% over a flat card, the shape of every
    /// row on a native page. The two were 521,464 linear-light composites; now
    /// a dozen, the rims' few coverages and one for each interior, whatever
    /// the size.
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
        // The card's middle is the card over the ground, as a pixel blend says.
        let mut cv = Canvas::new(1, 1, [0; 4]);
        cv.blend(0, 0, [40, 44, 52, 230], 1.0);
        cv.blend(0, 0, [250, 250, 250, 153], 1.0);
        let middle = ((220 * 640 + 320) * 4) as usize;
        assert_eq!(rgba[middle..middle + 4], cv.px[..4]);
    }

    /// A WORD THE UI FACE DRAWS WHOLE IS MEASURED ONCE ([`MeasureMemo`]):
    /// measured again at the same size (its width) or the same width (its
    /// line breaks) it costs no glyph walk and reads the same. Negative
    /// controls: a string with a char the face lacks (an ideograph, drawn by
    /// a fallback face that lands lazily) is measured every time, and a new
    /// UI font measures afresh.
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
        // An ideograph reaches another face: measured every time.
        let mixed = "\u{8a2d}\u{5b9a} Settings";
        let _ = ui_text_width_for(TextFace::Ui, mixed, px);
        let walks = measure_walks();
        let _ = ui_text_width_for(TextFace::Ui, mixed, px);
        assert_eq!(measure_walks(), walks + 1, "never kept");
        // Another UI font: measured afresh, in it.
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

    /// The Settings preview resolves a BUNDLED family (the default — `Iosevka
    /// Term Semibold` on Windows, `JetBrains Mono` on Linux — `Cascadia Mono
    /// PL` and `JetBrains Mono` on Windows too, and `Iosevka Term` everywhere,
    /// macOS included, when none is installed) to its compiled-in bytes. It
    /// used to hand the resolved `bundled:` identity to the filesystem and
    /// report the owner's default font as unreadable.
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

    /// The Win11 variable cut leads the Windows UI-face candidates, paired with the static
    /// semibold (never itself); the static Segoe UI pair follows for Win10.
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

    /// The Linux ladder reaches Noto Sans on a host with only fonts-noto-core's Regular/Bold
    /// cuts (no NotoSans-SemiBold.ttf), and SemiBold keeps priority where a distro ships it.
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
        // Host-conditional leg: with the Debian core cuts on disk the resolver lands Noto Sans,
        // regular and a real bold companion, never the DejaVu fallback.
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
            // Which family loaded is asserted through the candidate ladder, not the faces: a
            // parsed face exposes no family name, and the ladder is what decides Noto over
            // DejaVu.
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

    /// One admission per distinct UI face file, whatever the ladder pairs (the macOS candidate
    /// names HelveticaNeue.ttc for both faces), and a face on the system font volume is a
    /// mapping (`font_file::maps_in_place`), not a copy.
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
    }

    /// A theme-derived spread of prims — three rings, a capsule, a dot, and
    /// one funnelled text run — enough to drive every core arm of the
    /// rasterizer.
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

    /// Tier-1 conformance for the CHROME FACE GATE: the SHIPPING
    /// [`select_chrome_face`] policy, enumerated over its ENTIRE 2^3 boolean
    /// domain — a complete proof of the face-gate invariants.
    #[test]
    fn chrome_face_gate_exhaustive() {
        let (mut fallback_seen, mut bold_seen, mut primary_seen) = (false, false, false);
        for bits in 0u32..8 {
            let b = |i: u32| (bits >> i) & 1 == 1;
            let (bold_run, bold_has, primary_has) = (b(0), b(1), b(2));
            let pick = select_chrome_face(bold_run, bold_has, primary_has);
            // EmbeddedOnlyAsCoverageFallback: the embedded DejaVu is chosen only when neither
            // user face applies.
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
        // Non-vacuous: every face of the stack is reachable.
        assert!(
            fallback_seen && bold_seen && primary_seen,
            "gate must reach all three faces (fallback={fallback_seen}, \
             bold={bold_seen}, primary={primary_seen})"
        );
    }

    /// The cap-height centering law bound to the shipping [`row_baseline`]: over a lattice of
    /// `(row, size)`, the gap above the cap box and the gap below the baseline balance within
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
        // Negative control: em-bottom placement (baseline = y + px) fails the law for the same
        // inputs, sitting ~0.135 em low.
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

    /// The baseline, canvas and size the Mono glyph tests below draw at.
    const MONO_GLYPH_BASELINE: f32 = 40.0;
    const MONO_GLYPH_CANVAS: (u32, u32) = (64, 56);
    const MONO_GLYPH_PX: f32 = 26.0;

    /// `s` drawn as a Mono run with `primary` as the chrome's primary face,
    /// white over black: the RGBA buffer.
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

    /// Whether a Regular Mono run draws `ch` from the chrome's PRIMARY face —
    /// the one [`mono_run_image`] last installed — and not from the embedded
    /// DejaVu fallback or a coverage face under it, which a primary that
    /// declined `ch` would hand it to.
    fn primary_draws(ch: char) -> bool {
        let mut fonts = lock_fonts();
        let Some(primary) = fonts.primary.as_ref().map(|face| Arc::clone(&face.font)) else {
            return false;
        };
        fonts
            .face_for(TextWeight::Regular, ch)
            .is_some_and(|face| Arc::ptr_eq(&face.font, &primary))
    }

    /// A MONO RUN DRAWS LATIN-1 AS ITS FACE'S UNICODE SUBTABLE MAPS IT. Mono
    /// text is set in the terminal's face, and on Monaco the Stats Deck drew
    /// `·` as `∑` and `×` as `◊`: the chrome looked chars up in the enumerated
    /// cmap, where a Macintosh subtable listed after the Unicode one wins. The
    /// embedded DejaVu Sans Mono with its Mac Roman record moved last has that
    /// shape on every host, so it must draw each char itself — not hand it to
    /// the embedded DejaVu under it, whose texels are the same — texel for
    /// texel as the shipped face (whose Unicode subtables come last) does, and
    /// not as the Mac Roman glyph of the char's byte (`∑ ◊ ∞ È`), which it
    /// drew before.
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

    /// ON MONACO `·` IS A SMALL DOT AND `×` IS NOT `◊`. The face the defect
    /// was seen on, read from the system: Monaco itself draws both (not the
    /// embedded DejaVu under it, whose `·` is a small raised dot too, so the
    /// dot is checked to be Monaco's), and its middle dot inks a small box
    /// above the baseline, a fraction of what `∑` inks.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_mono_run_on_monaco_draws_the_middle_dot_as_a_dot() {
        let path = "/System/Library/Fonts/Monaco.ttf";
        let Ok(monaco) = std::fs::read(path) else {
            aterm_spec::host::could_not_run(format!("no {path} on this Mac"));
        };
        let monaco: Arc<[u8]> = monaco.into();
        let (width, height) = MONO_GLYPH_CANVAS;
        // Inked texels, and the ink's box `(x0, y0, x1, y1)`, exclusive.
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

    /// Inked column clusters of a raster over a black ground: `(x0, y0, x1, y1)`
    /// (exclusive) per run of inked columns, split on an empty column — one
    /// entry per glyph for a run whose side bearings leave a gap.
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

    /// A chain-face char sits on the run's baseline: the gate's `Fallback` pick (DejaVu) lacks
    /// `日` (the negative control), and the coverage rung draws it by its bearings under the
    /// baseline `row_baseline` derives from the primary's cap ratio, so its body centres on the
    /// Latin cap box, bottom at the baseline, top within an em above.
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
        // Bottom-most inked row is `y1 - 1`.
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

    /// The measure and the pen agree on a chain-face char: for `A日B` in the UI face,
    /// [`ui_text_width_for`] charges the coverage face's advance, not the UI face's `.notdef`
    /// (the negative control under-measures by ~0.4 em at 13 px).
    #[test]
    fn ui_measure_agrees_with_the_pen_for_a_chain_face_char() {
        if !install_chrome_fonts_with_cjk_chain_for_test() || !strip_band_ui_ready() {
            clear_ui_fonts_for_test();
            aterm_spec::host::could_not_run("no system UI face here for the strip band");
        }
        let s = "A日B";
        let px = 13.0_f32;
        let measured = ui_text_width_for(TextFace::Ui, s, px);
        // Negative control: every char at the UI face's own advance.
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

    /// The rung is last: with a coverage face installed, a char DejaVu covers still resolves to
    /// DejaVu, a char only the chain covers resolves to the chain face at either weight, and a
    /// char nothing covers resolves to no face.
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
        // A char BOTH carry: the gate's pick wins.
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

    /// The band key moves on a landing no install announces: the seed is a fork of the live
    /// renderer over the same chain cells, so the terminal's first ideograph parse lands in a
    /// cell the seed shares without passing through the chrome store, and
    /// [`strip_band_font_epoch`] reads the rung itself.
    ///
    /// Windows and Linux only (a `cfg`, so macOS reports it absent rather than passing):
    /// [`crate::tab_bar::pixel_band`] exists only there, and on macOS the live renderer
    /// resolves CJK through CoreText, so `ready_fallback_faces()` stays empty and the key cannot
    /// move. The sibling tests are not gated: [`install_chrome_fonts_with_cjk_chain_for_test`]
    /// parses the face itself.
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
        // The GUI window's own order: seal (the chain is read, unparsed), then
        // fork the chrome's seed off the sealed generation.
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
        // The live terminal draws the ideograph: ITS parse lands in the shared cell.
        live.prepare_semantic_text("日");
        // AND THE LANDING IS ASYNCHRONOUS, so WAIT for it rather than assuming
        // one read catches it. `strip_band_font_epoch` polls through
        // `poll_semantic_renderer`, whose `results.try_recv()` is NON-BLOCKING:
        // read once, immediately after the request, it sees an empty channel
        // whenever the worker has not posted yet, coverage does not grow and
        // the epoch does not move. That is a race, not a verdict, and it failed
        // exactly that way in a loaded full-suite run while passing six times
        // standalone. The bound keeps the test's teeth: a landing that never
        // arrives still fails, it just no longer fails on scheduling. A hang
        // detector, never a latency budget.
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

    /// How many chain faces THIS thread's installed seed holds parsed.
    fn ready_chain_faces() -> usize {
        lock_fonts()
            .semantic
            .as_ref()
            .map_or(0, |seed| seed.ready_fallback_faces().count())
    }

    /// A LABEL HAS THE CHAIN FACE IT NEEDS PARSED ON THE FRAME THAT DRAWS IT.
    /// With the host's CJK face in the seed's chain, unparsed (a fresh
    /// window), asking whether a CJK title is coverable parses that face —
    /// into the cell the seed shares — and answers yes; a title no face maps
    /// parses nothing and stays uncoverable; a Latin title needs nothing.
    /// NEGATIVE CONTROL: before the ask, the cascade does not reach 日. The
    /// UI faces' own East Asian fallback is taken away, the state of a host
    /// without one: its turn is
    /// `a_cjk_label_has_its_weights_own_ui_faces_parsed_when_the_band_asks`.
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

    /// THE STARTUP WARM PARSES THE CJK AND HANGUL FACES, AND ONLY THOSE. Over
    /// a seed whose chain is unparsed, [`warm_chrome_coverage`] parses the
    /// faces that map Han, kana and Hangul — every face it parses maps one of
    /// them — so the cascade reaches 日 with no label having asked; a second
    /// warm parses nothing. NEGATIVE CONTROL: before it, nothing is parsed.
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

    /// EVERY CHROME-FONT INSTALL WARMS THE FACES A CJK LABEL DRAWS WITH, on
    /// every platform ([`chrome_coverage_warm`]): over a seed whose chain is
    /// unparsed (a fresh window), the install's warm parses the chain's CJK
    /// face, so a Settings or Manual label draws `設定を開く` in ink with no
    /// label having asked. NEGATIVE CONTROL: before the warm, the label's
    /// ideographs have no face (blank advances). With the warm gated to
    /// Windows and Linux, a macOS install warmed nothing and its labels drew
    /// a CJK title as blank advances forever (the live terminal draws CJK
    /// through CoreText there and parses no chain face).
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

    /// Which chars the UI faces' own East Asian fallback is asked for, and of
    /// which face: Hangul of the Korean face, every other CJK char of the
    /// Japanese then the Chinese one, anything else of nothing (it keeps the
    /// terminal cascade, so no symbol, arrow or other script changes face).
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
            // U+3000, the ideographic space, is CJK punctuation: the faces
            // draw it as an em of air, as Windows does.
            assert_eq!(asked.is_some(), ch == '\u{3000}', "{ch:?}: {asked:?}");
        }
    }

    /// Install, SETTLED, a semantic seed whose chain holds the production
    /// Windows chain's East Asian faces, PARSED: Microsoft YaHei Bold and
    /// Malgun Gothic Bold, the weight-matched siblings `aterm_render` puts
    /// beside Iosevka Term SemiBold. Plus the host UI faces and a cold store
    /// of the UI faces' own East Asian fallback. `None` (the caller ends COULD
    /// NOT RUN) where the host lacks any of them.
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

    /// The mean ink of `s`'s lit pixels (white on black, the judges' `t` with
    /// its 0.05 floor) and their count, drawn at `px` in `face`.
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

    /// A QUIET TAB'S CJK AND HANGUL INK LIKE ITS LATIN; THE SELECTED TAB'S ARE
    /// SEMIBOLD. Over the production chain's Bold faces (the 2026-10-18
    /// judges' `j4t-cjk-band-labels-bold`: `日本語` and `한국어` at a mean ink
    /// of 0.635 and 0.614 beside `notes` at 0.540), a Regular label's East
    /// Asian text draws from the UI faces' own Regular fallback — Yu Gothic UI
    /// and Malgun Gothic, the faces Windows itself falls back to from Segoe UI
    /// — and lands within 0.05 of the Latin's mean ink; a Semibold label's
    /// from their Semibold and Bold, heavier than the Regular. Measured and
    /// drawn alike: the pen's advance is the measure's. NEGATIVE CONTROL: the
    /// same labels with the UI faces' fallback taken away draw from the
    /// chain's Bold, as before, heavier than the Latin by more than 0.05.
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
            // Drawn from the store's own faces, the Regular rung's for Ui and
            // the Semibold rung's for UiBold, by identity.
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
            // Measured as drawn: the band fits and centres by this width.
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
        // NEGATIVE CONTROL: the chain's Bold, as every label drew before.
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

    /// A WEIGHT NEVER BORROWS THE OTHER'S FACE. With only the Regular rung
    /// landed, a Semibold run's `日` is not drawn from it: it falls to the
    /// terminal cascade (here nothing, the store being the only East Asian
    /// face), and the band's coverage question reports it missing, so the
    /// band has the Semibold face parsed before it draws. Yu Gothic UI's kana
    /// are proportional, so not even an advance carries across: `の` is
    /// 0.816 em Regular and 0.836 em Semibold. NEGATIVE CONTROL: the Regular
    /// run reaches the Regular face.
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

    /// A LABEL HAS ITS OWN WEIGHT'S EAST ASIAN FACES PARSED WHEN THE BAND
    /// ASKS, and only those. Over a cold store: a Latin label parses nothing;
    /// a quiet `日本語 notes` parses the Regular rung's Han face, which maps
    /// it, and nothing of the Semibold rung or the Hangul faces; the selected
    /// `한국어` parses the Semibold rung's Hangul face and not the Regular's.
    /// The chain, unparsed, is not asked: the UI faces place every char. The
    /// band key witnesses each landing. NEGATIVE CONTROL: before the ask the
    /// store holds nothing and the UI routing does not reach 日.
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

    /// THE STARTUP WARM PARSES THE UI FACES' OWN CJK AND HANGUL FACES, both
    /// weights, and no other: the Regular rung's and the Semibold rung's
    /// first Han face (they map 日 and の) and their Hangul face, four in all;
    /// the Chinese face, which only a char the Japanese face lacks asks for,
    /// stays unread. A second warm parses nothing. NEGATIVE CONTROL: a cold
    /// store.
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

    /// Which chars a label may ask the terminal's chain for: text in any
    /// script, never a colour emoji (the band draws those in colour), a
    /// private-use icon (the cell lane's rules), a joiner, a selector or a
    /// control.
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

    /// [`set_chrome_fonts`] robustness: unparseable bytes leave the slot on the DejaVu fallback
    /// (text still renders), and clearing restores the default stack.
    #[test]
    fn set_chrome_fonts_bad_bytes_keep_fallback_rendering() {
        let garbage: std::sync::Arc<[u8]> = std::sync::Arc::from(&b"not a font"[..]);
        set_chrome_fonts(Some((garbage.clone(), 0)), Some((garbage, 3)), None);
        let w = measure_text("abc", 12.0, TextWeight::Regular);
        assert!(w > 0.0, "fallback still measures after a bad injection");
        // An injected real face (the embedded bytes stand in for the user's font) takes the
        // primary slot; with no bold face, a bold run downgrades to it and measures the same.
        let bytes: std::sync::Arc<[u8]> = std::sync::Arc::from(aterm_render::embedded_font());
        set_chrome_fonts(Some((bytes, 0)), None, None);
        let w2 = measure_text("abc", 12.0, TextWeight::Bold);
        assert!(
            (w2 - w).abs() < 1e-3,
            "bold with no bold face downgrades to primary"
        );
        set_chrome_fonts(None, None, None);
    }

    /// The chrome keeps the person's font under a display layer: the terminal
    /// draws Engraved over DejaVu, and the chrome's face is DejaVu — the face
    /// that draws with every display face off — never the display face, and
    /// never the embedded fallback standing in for a refused display face
    /// (which is what the Manual editor drew while the face was the primary).
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

    /// Bind one renderer and engine frame into the spec [`Canvas::terminal_specimen`]
    /// consumes, as `settings_preview` does (production defaults for every knob a test does not
    /// exercise).
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

    /// An unsupported scalar on the shipping specimen path occupies exactly one terminal cell:
    /// every pixel outside it, including the next glyph's, is byte-identical to the same frame
    /// with a space there.
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
            // Both frames are the same engine snapshot of "A B"; the probe swaps the middle
            // cell for a scalar no face covers, so it occupies that one column by construction.
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
    /// Poll `fonts` until its pending semantic candidate LANDS (the worker's
    /// result disarms `semantic_pending`), then poll once more. The wait ends
    /// on that fact, 5 ms after it at most; its bound is the one-minute hang
    /// detector ([`aterm_spec::wait::MUST_HAPPEN`]) and measures nothing. It
    /// was 20 s, then ten minutes while the worker ran at background QoS: the
    /// first job settles the whole Unicode specimen in a debug build, and
    /// beside 72 spinners (load 100-170) that outran 20 s in 2 of 3 runs. Every
    /// worker this waits on now runs at `Role::Responsive`
    /// ([`SemanticPrewarmWorker::spawn_serialized_by`]): the three tests that
    /// wait here took 0.03-0.15 s each, twenty runs alone and five beside a
    /// whole `-p aterm-gui --lib` run, at load 70 (2026-10-20).
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

    /// A landed font wakes the host, which repaints every specimen that was
    /// waiting on it: its status cannot stick on "Loading font" until
    /// some unrelated input arrives.
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
        // A hang detector: the wake follows the result it announces.
        let started = std::time::Instant::now();
        while WAKES.load(std::sync::atomic::Ordering::SeqCst) == 0
            && started.elapsed() < std::time::Duration::from_secs(60)
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(WAKES.load(std::sync::atomic::Ordering::SeqCst) > 0);
    }

    /// While a new candidate resolves, the committed cascade parked in the
    /// cache keeps drawing the specimen: the box is never blank.
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
        // A worker that takes the job and never answers: the candidate stays
        // pending for the whole test.
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

    /// The same with a named family on screen (a saved Menlo, not the
    /// default): the face that was drawing keeps drawing while another name
    /// resolves, so the box does not flash empty on every typed character.
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
        // A private serialization gate, so the staged coalescing window and the landing wait
        // never queue behind another test's warmup; the worker protocol is production's.
        let gate: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
        let serial = gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut fonts = default_chrome_fonts();
        // Backend startup parks the one worker (`set_chrome_fonts`); candidate
        // requests never demand-spawn it.
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

        // A completion wait, not a timing assertion: correctness is the landed generation.
        await_semantic_landing(&mut fonts);
        assert_eq!(fonts.semantic_generation, 2);
        assert!(fonts.semantic_pending.is_none(), "land disarms");
        assert!(fonts.semantic.is_some(), "latest exact fork installs");
        assert!(fonts.semantic_prewarm_ms.is_some());
    }

    /// Tier-1 (`SemanticPrewarmGeneration`, `Reload`): the shipping reload body on a local font
    /// context whose stub worker queue still holds a job for the superseded generation, read
    /// back off the worker (`queued` the job's generation, `current` the context's). The same
    /// reload without its `cancel_queued` (the model's `BuggyReloadKeepsQueued`) is refused.
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

        // Negative control: the same reload with the cancel lost.
        let mut kept = after;
        kept.insert("queued", before["queued"]);
        assert!(!model.successors("Reload", &before).contains(&kept));
        assert!(!model.check_invariant("QueueContainsOnlyCurrent", &kept));
    }

    /// Tier-1 (`SemanticPrewarmHandshake`, the slot half of `DecideResult`). The pure bind
    /// (`src/semantic_prewarm_conformance.rs`) holds `semantic_prewarm_result_decision`; this
    /// holds what `poll_semantic_renderer` does with it. Every point of the identity ×
    /// readiness × active-slot lattice lands one result through the real poll from a stub
    /// worker channel, and the active slot is read back off `ChromeFonts`: `active_after` is
    /// whether a renderer paints, `active_after_latest` whether it is the requested
    /// candidate's. Negative controls: removing `self.semantic = None` from the
    /// `FailClosedCurrent` arm (`BuggyFailClosedKeepsPrevious`), dropping the `semantic_identity`
    /// line from `InstallCurrent` (`BuggyInstallKeepsStaleIdentity`), or opening
    /// `CacheSuperseded` with `cache_active_semantic()` (`BuggyCacheParksActive`) fails rows.
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
                            // A landed candidate: an install caches it rather
                            // than carrying it as the dormant host seed.
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
        // A private serialization gate, as in
        // `semantic_prewarm_coalesces_and_installs_only_latest_generation`.
        let gate: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
        let mut fonts = default_chrome_fonts();
        // Backend startup parks the one worker (`set_chrome_fonts`); candidate
        // requests never demand-spawn it.
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

        // Whether the worker dequeued A or B won the replacement slot above,
        // force the same uncached B -> A transition. This makes the cache/paint
        // isolation regression deterministic instead of scheduler-dependent.
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

    /// A candidate forked from a sealed generation still reaches the
    /// compiled-in symbol face: a code point only that face covers resolves
    /// to it, not to the primary's `.notdef`.
    #[test]
    fn a_font_candidate_keeps_the_compiled_in_symbol_face() {
        // Only the bundled Symbols Nerd Font covers this icon (nf-fa-github), and
        // `from_bytes` lists no system face, so sealing reads no file.
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
        // Negative control: with the runtime tier switched off, the same
        // candidate has nothing that covers it.
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

    /// `ui_text_width` is a usable layout metric with OR WITHOUT the system face:
    /// positive for ordinary Latin text, empty == 0, and monotonic across
    /// that Latin run. Other queries can have narrow or zero advances, so a
    /// bounded scalar scan must establish its own floor. Runs on the mono fallback (headless CI that
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

        // A committed font, in a local store: neither host typography nor
        // another libtest worker can decide whether a scalar floor holds.
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

    /// A LINE'S TRAILING SPACE NEVER ENDS IT A WORD EARLY: a line whose last
    /// word ends within a space's width of the box keeps that word; the
    /// space after it, which paints nothing, goes with the line. The wrap
    /// charged that space against the box and broke before the word, so
    /// settings notes wrapped about 4pt short of their boxes. CONTROL: a word
    /// that does not fit still goes to the next line.
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
        // CONTROL: a box short of the word sends it on.
        let short = ui_text_width("alpha bet", px);
        let lines: Vec<&str> = ui_text_wrap_ranges(text, px, short)
            .into_iter()
            .map(|range| &text[range])
            .collect();
        assert_eq!(lines[0].trim_end(), "alpha", "{lines:?}");
        // A text that ends in the spaces its last line ends at has no empty
        // line after them.
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

    /// A NO-BREAK SPACE KEEPS ITS NEIGHBOURS ON ONE LINE, as the shared rule
    /// keeps it ([`aterm_grapheme::is_break_space`]): where the line would
    /// end after it, it ends at the space before instead. NEGATIVE CONTROL:
    /// with every white space a break, "50\u{a0}" ended the line and "%"
    /// started the next.
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

    /// The UI faces rasterize something even when the system stack is absent, and a real
    /// semibold face covers more ink than regular on supported desktops.
    #[test]
    fn ui_faces_rasterize_and_bold_is_no_lighter() {
        // UI faces are host-prepared: `set_chrome_fonts` installs them into this context as
        // backend startup does (raster never resolves fonts).
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

    /// A terminal frame is placed by row spans, and lands byte for byte where
    /// an opaque full-coverage blend of each pixel lands: on a regional tile
    /// whose origin is not the surface's, under a clip, and hanging off the
    /// tile's edges. Negative control: the frame did change the tile.
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
        // 50%-coverage opaque white over opaque black lands near the linear-light midpoint
        // (sRGB ~188 / 0xBC), not the gamma-space 128, as the main glyph path does.
        let mut cv = Canvas::new(1, 1, [0, 0, 0, 255]);
        cv.blend(0, 0, [255, 255, 255, 255], 0.5);
        assert!(
            (185..=190).contains(&cv.px[0]),
            "linear-light midpoint expected ~188, got {}",
            cv.px[0]
        );
    }

    /// Chrome glyph coverage blends the way the terminal renderer's default
    /// (`TextBlending::LinearCorrected`) blends it, so dark text on a light
    /// fill reaches its ink as firmly as light text on a dark one. Pure
    /// linear coverage (the negative control) puts a half-covered black
    /// texel over white at ~0xBC and leaves light-theme chrome text grey.
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
        // Negative control: the plain linear blend is far lighter at 50%.
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
        // Endpoints are exact, and a transparent canvas has no ground to key on.
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

    /// The whole text path routes through the corrected texel blend: a mono
    /// run drawn black on white is the exact negative of the same run drawn
    /// white on black (a gamma-space blend is symmetric), where linear
    /// coverage would leave the dark-on-light run visibly lighter.
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

    /// The UI pen places each glyph at the quarter pixel nearest its true
    /// place, the ink moved by that quarter: at 1× an 11 px word's gaps are
    /// even, where snapping each glyph to a whole pixel split "To kens".
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
        // The stem's left edge, in px from the pen's whole pixel: the first
        // inked texel of a row through the stem, less what it leaves bare.
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
        // A whole pixel on is the same mask one pixel on.
        let (m0, c0) = font.char_raster('l', 13.0);
        let (m1, c1) = font.char_raster_at('l', 13.0, 1.0);
        assert_eq!((m1.xmin, m1.width, &c1), (m0.xmin + 1, m0.width, &c0));
        // Phase 0 is the whole-pixel raster.
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

    /// A fade leaves what is drawn above its band untouched and, inside it,
    /// eases the drawing out from its top with no step there to nothing at
    /// its bottom, falling fastest where it starts: a scrolling list's peek
    /// in the strip under its room. CONTROL: what is drawn after its pop is
    /// whole again.
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

    /// Every unresolved slot a semantic font status names is read, each with
    /// its family, past each one's reason and before what was requested.
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

    /// A FADE FADES WHAT IT HOLDS AS ONE PICTURE. A preview in a list's fading
    /// strip is a terminal panel with the terminal's opaque frame over it, the
    /// frame the panel's colour where no glyph is: under a fade the two must
    /// read as one surface, never as the frame standing out of the panel (a
    /// notch at the frame's right edge and a step under its last row). Over a
    /// contrasting ground, a wide panel and a narrower one of the same colour
    /// over it, and an opaque frame over both, inside one fade: on every row
    /// of the band the frame, the inner panel and the outer panel read the
    /// same bytes. CONTROL: with no fade the frame is blitted byte for byte,
    /// and above the band it is untouched.
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

    /// The HsvDisk raster: the centre (saturation 0) is the grey axis at
    /// `value` (white×value), the rim is anti-aliased over ~1px (a partial blend,
    /// neither full disk colour nor pure backdrop), and pixels past the rim stay
    /// untouched backdrop.
    #[test]
    fn hsv_disk_center_edge_and_outside() {
        // Centre the disk on a pixel sample point (x+0.5, y+0.5) so pixel (10,10) reads the
        // exact centre (saturation 0).
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
        // A 1px-tall horizontal Stroke fully covers exactly one device row (no smear).
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

    /// Every admitted message glyph inks in the cold chrome store (the embedded DejaVu alone,
    /// what unit tests and a host whose faces lack a symbol hold): `ℹ` and `⏸` are the pen's
    /// own and the rest are DejaVu's, so no Settings ▸ Messages severity cell draws blank and a
    /// newly admitted glyph cannot go blank silently.
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
                // On the cap box: from the baseline (an O's overshoot at
                // most) to under 0.72 em, inside its own advance.
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

    /// The pen inks `ℹ` on both arms (the mono stack and the proportional UI arm) in the cold
    /// store, inside the glyph's own box on the given baseline, and nothing else on the canvas.
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
            // A ring, not a blob: the centre of the ring is dark (the stem
            // and the dot sit off it), and both the top and the bottom of
            // the ring are lit.
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
