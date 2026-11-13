// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The numeric checks `system-state-gallery` reads from a System State frame (init-on-by-default
//! design §4.9): glyph ink against its wash and the ground, text against its background, a card
//! against the page, clipped or ellipsised text, and the 4 pt vertical rhythm.
//!
//! Where the page's text, glyphs and cards are is never guessed from pixels: it is the page's own
//! compiled layout, `aterm ctl inspect app/v1 view <view> audit` (kept beside each frame as
//! `<frame>.layout.txt`): each `paint-node` line's kind, rect, clip, state, `surface` and `fade`,
//! and each `paint-text` line's fit. Only the colours are read from the PNG, inside those rects.

use std::collections::HashMap;

use crate::pyjson::J;
use crate::trail_contrast::{Image, PxBox, Rgb, contrast, hex, mode, round_to};

/// Text against its background (WCAG AA for body text).
pub const TEXT_MIN: f64 = 4.5;
/// A glyph's ink against its wash and against the ground (WCAG 1.4.11, non-text contrast).
pub const GLYPH_MIN: f64 = 3.0;
/// A card against the page it sits on (design §4.4).
pub const CARD_MIN: f64 = 1.2;
/// The vertical rhythm's step, in points.
pub const RHYTHM_PT: f64 = 4.0;
/// How far off the rhythm's grid an edge may print and still sit on it: the layout prints its
/// rects to one decimal.
const RHYTHM_SLACK: f64 = 0.15;
/// Words whose visible part keeps less of their line's height than this are cut by a scroll
/// edge: a line box's words sit in its middle, so a trim of its padding cuts none of them.
const SCROLL_KEEP: f64 = 0.8;
/// A node thinner than this is a hairline (a rule between rows), which no rhythm or glyph judges.
const HAIRLINE_PT: f64 = 2.0;
/// A colour is solid ink when at least this many pixels share it exactly: one stray
/// antialiasing pixel is never the ink.
const SOLID_COUNT: usize = 2;
/// The band read just outside a glyph for its wash, in pixels: 1 to 3 out.
const WASH_RING: (i64, i64) = (1, 3);
/// The bands a card's surface and the page around it are read from, in points: 2 to 5 inside
/// the card's edge, and 2 to 6 outside it (clear of its 1 pt rim and its lit edge).
const CARD_INSIDE: (f64, f64) = (2.0, 5.0);
const CARD_OUTSIDE: (f64, f64) = (2.0, 6.0);
/// A card's corner radius, kept out of the side bands.
const CARD_CORNER: f64 = 12.0;

/// The node kinds whose words a `paint-text` line audits.
const TEXT_KINDS: [&str; 4] = ["text", "button", "rich-text", "text-field"];
/// The group surfaces that are cards: the elevated card, the hero card and a code block.
const CARD_SURFACES: [&str; 3] = ["card", "hero", "code"];

/// A rectangle in the window's logical points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    /// `x,y,w,h`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Rect> {
        let mut it = text.split(',').map(|v| v.trim().parse::<f64>().ok());
        let r = Rect {
            x: it.next()??,
            y: it.next()??,
            w: it.next()??,
            h: it.next()??,
        };
        (it.next().is_none() && [r.x, r.y, r.w, r.h].iter().all(|v| v.is_finite())).then_some(r)
    }

    #[must_use]
    pub fn right(self) -> f64 {
        self.x + self.w
    }

    #[must_use]
    pub fn bottom(self) -> f64 {
        self.y + self.h
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    /// The part of `self` inside `other` (empty when they do not meet).
    #[must_use]
    pub fn intersect(self, other: Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        Rect {
            x,
            y,
            w: (self.right().min(other.right()) - x).max(0.0),
            h: (self.bottom().min(other.bottom()) - y).max(0.0),
        }
    }

    #[must_use]
    pub fn contains(self, x: f64, y: f64) -> bool {
        self.x <= x && x < self.right() && self.y <= y && y < self.bottom()
    }

    /// The pixels the rect covers at `scale`, each edge rounded to the nearest pixel boundary.
    #[must_use]
    pub fn px(self, scale: f64) -> PxBox {
        let at = |v: f64| (v * scale).round() as i64;
        PxBox {
            x0: at(self.x),
            y0: at(self.y),
            x1: at(self.right()),
            y1: at(self.bottom()),
        }
    }
}

/// One laid-out node of the page (a `paint-node` line).
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub key: String,
    /// `group`, `text`, `button`, `switch`, `custom`, …
    pub kind: String,
    pub rect: Rect,
    /// The ancestors' clip.
    pub clip: Rect,
    pub visible: bool,
    /// An ancestor clip removes a perceptible part of it.
    pub clipped: bool,
    pub role: String,
    pub label: String,
    /// A control's state says `enabled:false`.
    pub disabled: bool,
    pub audit_id: String,
    /// A group's backdrop: `page`, `navigation`, `card`, `hero`, `code` or `none`; `-` for other
    /// kinds, and `None` from a build whose audit does not say.
    pub surface: Option<String>,
    /// The band of a scrolling list's edge it lies in, `(top, bottom)` in points: from `top`
    /// down, the list fades its rows out (by design, the cue that more lies below); `None` when
    /// no fade reaches it, or from a build whose audit does not say.
    pub fade: Option<(f64, f64)>,
}

impl Node {
    /// Its part a person sees at full strength: inside its clip and above any fade.
    #[must_use]
    pub fn undimmed(&self) -> Rect {
        let shown = self.rect.intersect(self.clip);
        match self.fade {
            Some((top, _)) if shown.bottom() > top => Rect {
                h: (top - shown.y).max(0.0),
                ..shown
            },
            _ => shown,
        }
    }

    /// Whether the fade dims more of it than a line box's padding: its undimmed part keeps
    /// under [`SCROLL_KEEP`] of the part its clip shows.
    #[must_use]
    pub fn faded(&self) -> bool {
        let shown = self.rect.intersect(self.clip);
        !shown.is_empty() && self.undimmed().h < shown.h * SCROLL_KEEP
    }
}

/// The fit of one node's words (a `paint-text` line).
#[derive(Clone, Debug, PartialEq)]
pub struct TextFit {
    pub key: String,
    /// The words were ellipsised, or need more room than they have.
    pub overflow: bool,
    /// A clip edge cuts the words.
    pub clip_truncated: bool,
    pub required: f64,
    pub available: f64,
    /// Conservative painted-word bounds in logical points, when the audit supplies them.
    pub word_band: Option<(f64, f64)>,
    /// The words as painted.
    pub painted: String,
}

/// The page's compiled layout as the instance reported it.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// Device pixels per point.
    pub scale: f64,
    /// The page's bounds in the window, in points.
    pub viewport: Rect,
    pub nodes: Vec<Node>,
    pub texts: Vec<TextFit>,
}

/// The fields of an audit line: `name=value` and `name="debug string"` pairs, in order. A bare
/// word (a control state's `focused:false`) is no field.
#[must_use]
pub fn fields(line: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = line;
    loop {
        rest = rest.trim_start_matches(' ');
        if rest.is_empty() {
            return out;
        }
        let word_end = rest.find([' ', '=']).unwrap_or(rest.len());
        if !rest[word_end..].starts_with('=') {
            rest = &rest[word_end..];
            continue;
        }
        let name = rest[..word_end].to_string();
        rest = &rest[word_end + 1..];
        if rest.starts_with('"') {
            let (value, after) = debug_string(rest);
            out.push((name, value));
            rest = after;
        } else {
            let end = rest.find(' ').unwrap_or(rest.len());
            out.push((name, rest[..end].to_string()));
            rest = &rest[end..];
        }
    }
}

/// The string a Rust `{:?}` wrote at the head of `text` (which starts with its opening quote),
/// unescaped, and the text after its closing quote.
fn debug_string(text: &str) -> (String, &str) {
    let mut out = String::new();
    let mut chars = text.char_indices().skip(1);
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return (out, &text[i + 1..]),
            '\\' => match chars.next().map(|(_, e)| e) {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('0') => out.push('\0'),
                Some('u') => {
                    let mut hex_digits = String::new();
                    for (_, h) in chars.by_ref() {
                        match h {
                            '{' => {}
                            '}' => break,
                            _ => hex_digits.push(h),
                        }
                    }
                    if hex_digits.bytes().all(|b| b.is_ascii_hexdigit())
                        && let Some(ch) = u32::from_str_radix(&hex_digits, 16)
                            .ok()
                            .and_then(char::from_u32)
                    {
                        out.push(ch);
                    }
                }
                Some(other) => out.push(other),
                None => {}
            },
            _ => out.push(c),
        }
    }
    (out, "")
}

fn field<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

/// Read the layout from an `inspect app/v1 view <view> audit` reply's lines.
/// # Errors
/// No `inspection-source` line (its scale and viewport), or a `paint-node` line with no key or
/// a malformed rect.
pub fn parse_layout(text: &str) -> Result<Layout, String> {
    let mut scale = None;
    let mut viewport = None;
    let mut nodes = Vec::new();
    let mut texts = Vec::new();
    for line in text.lines().map(str::trim_end) {
        if let Some(rest) = line.strip_prefix("inspection-source ") {
            let f = fields(rest);
            scale = field(&f, "scale").and_then(|s| s.parse::<f64>().ok());
            viewport = field(&f, "viewport").and_then(Rect::parse);
        } else if let Some(rest) = line.strip_prefix("paint-node ") {
            let f = fields(rest);
            let key =
                field(&f, "key").ok_or_else(|| format!("a paint-node line with no key: {line}"))?;
            let rect = |name: &str| {
                field(&f, name)
                    .and_then(Rect::parse)
                    .ok_or_else(|| format!("paint-node {key:?}: no {name}"))
            };
            nodes.push(Node {
                key: key.to_string(),
                kind: field(&f, "kind").unwrap_or("-").to_string(),
                rect: rect("rect")?,
                clip: rect("clip")?,
                visible: field(&f, "visible") == Some("true"),
                clipped: field(&f, "clipped") == Some("true"),
                role: field(&f, "role").unwrap_or("-").to_string(),
                label: field(&f, "label").unwrap_or_default().to_string(),
                disabled: field(&f, "state") == Some("enabled:false"),
                audit_id: field(&f, "audit-id").unwrap_or("-").to_string(),
                surface: field(&f, "surface").map(str::to_string),
                fade: field(&f, "fade").and_then(|v| {
                    let (top, bottom) = v.split_once(',')?;
                    Some((top.parse().ok()?, bottom.parse().ok()?))
                }),
            });
        } else if let Some(rest) = line.strip_prefix("paint-text ") {
            let f = fields(rest);
            let number = |name: &str| {
                field(&f, name)
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(0.0)
            };
            texts.push(TextFit {
                key: field(&f, "key").unwrap_or_default().to_string(),
                overflow: field(&f, "overflow") == Some("true"),
                clip_truncated: field(&f, "clip-truncated") == Some("true"),
                required: number("required"),
                available: number("available"),
                word_band: field(&f, "word-band").and_then(|value| {
                    let (top, bottom) = value.split_once(',')?;
                    let (top, bottom) = (top.parse::<f64>().ok()?, bottom.parse::<f64>().ok()?);
                    (top.is_finite() && bottom.is_finite() && bottom > top).then_some((top, bottom))
                }),
                painted: field(&f, "painted").unwrap_or_default().to_string(),
            });
        }
    }
    let scale = scale.filter(|s| *s > 0.0).ok_or(
        "no inspection-source line with a scale: not an `inspect app/v1 view <view> audit` reply",
    )?;
    let viewport = viewport.ok_or("the inspection-source line names no viewport")?;
    Ok(Layout {
        scale,
        viewport,
        nodes,
        texts,
    })
}

/// One failure a check found: the node and what was measured.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub key: String,
    /// The measured number (a ratio, a count of points off the grid).
    pub value: f64,
    pub detail: String,
}

/// What one check found on one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    /// `text`, `glyph`, `card`, `clip` or `rhythm`.
    pub name: &'static str,
    /// The nodes it judged.
    pub judged: usize,
    /// The worst measured number: the lowest ratio, or the most points off the grid; `None`
    /// when nothing was judged.
    pub worst: Option<f64>,
    pub failures: Vec<Finding>,
    /// Nodes it passed over, and why (`disabled`, `blank`, `hidden`), as `(why, count)`.
    pub skipped: Vec<(&'static str, usize)>,
}

impl Check {
    fn new(name: &'static str) -> Check {
        Check {
            name,
            judged: 0,
            worst: None,
            failures: Vec::new(),
            skipped: Vec::new(),
        }
    }

    #[must_use]
    pub fn pass(&self) -> bool {
        self.failures.is_empty()
    }

    fn skip(&mut self, why: &'static str) {
        match self.skipped.iter_mut().find(|(w, _)| *w == why) {
            Some((_, n)) => *n += 1,
            None => self.skipped.push((why, 1)),
        }
    }

    /// Note a judged ratio: the worst is the lowest, and one under `min` fails.
    fn ratio(&mut self, key: &str, ratio: f64, min: f64, detail: String) {
        self.judged += 1;
        self.worst = Some(self.worst.map_or(ratio, |w| w.min(ratio)));
        if ratio < min {
            self.failures.push(Finding {
                key: key.to_string(),
                value: round_to(ratio, 2),
                detail,
            });
        }
    }

    /// The check as one short phrase: `text 4.62 pass (38)`.
    #[must_use]
    pub fn phrase(&self) -> String {
        let verdict = if self.pass() { "pass" } else { "FAIL" };
        let worst = self
            .worst
            .map_or_else(|| "-".to_string(), |w| format!("{w:.2}"));
        match self.name {
            "clip" => format!("clip {} {verdict}", self.failures.len()),
            "rhythm" => format!(
                "rhythm {}/{} off {verdict}{}",
                self.failures.len(),
                self.judged,
                if self.pass() {
                    String::new()
                } else {
                    format!(" (worst {worst} pt)")
                }
            ),
            name => format!(
                "{name} {worst} {verdict} ({}{})",
                self.judged,
                if self.pass() {
                    String::new()
                } else {
                    format!(", {} under", self.failures.len())
                }
            ),
        }
    }

    #[must_use]
    pub fn json(&self) -> J {
        J::obj([
            ("pass", J::Bool(self.pass())),
            (
                "judged",
                J::Int(i64::try_from(self.judged).unwrap_or(i64::MAX)),
            ),
            ("worst", self.worst.map(|w| round_to(w, 2)).into()),
            (
                "failures",
                J::Arr(
                    self.failures
                        .iter()
                        .map(|f| {
                            J::obj([
                                ("key", J::s(f.key.as_str())),
                                ("value", J::Float(f.value)),
                                ("detail", J::s(f.detail.as_str())),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "skipped",
                J::Obj(
                    self.skipped
                        .iter()
                        .map(|(why, n)| {
                            ((*why).to_string(), J::Int(i64::try_from(*n).unwrap_or(0)))
                        })
                        .collect(),
                ),
            ),
        ])
    }
}

/// One card as the frame paints it.
struct Card {
    /// Its index in the layout's nodes.
    node: usize,
    /// Its visible rect.
    rect: Rect,
    /// Its colour, read just inside its edge; `None` when none of that band shows above its
    /// list's fade.
    colour: Option<Rgb>,
    /// The colour around it, read just outside its edge (`None` likewise).
    around: Option<Rgb>,
    /// It lies on another card (a control's backdrop on its card), not on the page.
    nested: bool,
}

/// The surfaces a frame's colours are judged against: each card's own colour and the colour
/// around it, and the page's colour where no card is.
struct Surfaces {
    cards: Vec<Card>,
    page: Rgb,
}

impl Surfaces {
    fn read(frame: &Image, layout: &Layout) -> Surfaces {
        let scale = layout.scale;
        let view = layout.viewport.px(scale);
        let card_nodes: Vec<(usize, Rect)> = layout
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.visible && is_card(n))
            .map(|(i, n)| (i, n.rect.intersect(n.clip)))
            .filter(|(_, r)| !r.is_empty())
            .collect();
        let boxes: Vec<PxBox> = card_nodes.iter().map(|(_, r)| r.px(scale)).collect();
        let page = mode(pixels_where(frame, view, |x, y| {
            !boxes.iter().any(|b| b.holds(x, y))
        }))
        .unwrap_or_else(|| frame.most_common());
        let cards = card_nodes
            .iter()
            .map(|&(node, rect)| {
                // Read above its list's fade: below it, the card dissolves into the page.
                let seen = above_fade(view, layout.nodes[node].fade, scale);
                Card {
                    node,
                    rect,
                    colour: mode(band(frame, rect, scale, CARD_INSIDE, true, seen)),
                    around: mode(band(frame, rect, scale, CARD_OUTSIDE, false, seen)),
                    nested: card_nodes.iter().any(|&(other, o)| {
                        other != node
                            && o.w * o.h > rect.w * rect.h
                            && o.contains(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0)
                    }),
                }
            })
            .collect();
        Surfaces { cards, page }
    }

    /// The surface under the point `(x, y)`: the smallest card holding it, else the page.
    fn ground_at(&self, x: f64, y: f64) -> Rgb {
        self.cards
            .iter()
            .filter(|c| c.rect.contains(x, y))
            .min_by(|a, b| (a.rect.w * a.rect.h).total_cmp(&(b.rect.w * b.rect.h)))
            .map_or(self.page, |c| c.colour.unwrap_or(self.page))
    }
}

/// Whether `node` is a card's surface: a group whose backdrop is one, or a painted surface the
/// audit names a card (the setup screen paints its list and its Needs-you card itself). A
/// surface is the ground marks are read against, never a mark of its own.
fn is_card(node: &Node) -> bool {
    (node.kind == "group" || node.kind == "custom")
        && node
            .surface
            .as_deref()
            .is_some_and(|s| CARD_SURFACES.contains(&s))
}

/// The pixels of `area` (clipped to the frame) for which `keep(x, y)` holds.
fn pixels_where(frame: &Image, area: PxBox, keep: impl Fn(i64, i64) -> bool) -> Vec<Rgb> {
    let clip = |v: i64, hi: usize| v.clamp(0, i64::try_from(hi).unwrap_or(i64::MAX));
    let (x0, x1) = (clip(area.x0, frame.width), clip(area.x1, frame.width));
    let (y0, y1) = (clip(area.y0, frame.height), clip(area.y1, frame.height));
    let mut out = Vec::new();
    for y in y0..y1 {
        for x in x0..x1 {
            if keep(x, y) {
                out.push(frame.at(
                    usize::try_from(x).unwrap_or(0),
                    usize::try_from(y).unwrap_or(0),
                ));
            }
        }
    }
    out
}

/// The pixels of the band `reach` points (from, to) inside (`inside`) or outside `r`'s edges, its
/// corners left out, within `view`.
fn band(
    frame: &Image,
    r: Rect,
    scale: f64,
    (from, to): (f64, f64),
    inside: bool,
    view: PxBox,
) -> Vec<Rgb> {
    let outer = if inside { grow(r, -from) } else { grow(r, to) };
    let inner = if inside { grow(r, -to) } else { grow(r, from) };
    let (outer_px, inner_px) = (outer.px(scale), inner.px(scale));
    // The corners: a card's rounded ones are neither its surface nor the page.
    let corner = (CARD_CORNER * scale).round() as i64;
    let rp = r.px(scale);
    pixels_where(frame, outer_px, |x, y| {
        if inner_px.holds(x, y) || !view.holds(x, y) {
            return false;
        }
        let near_x = x < rp.x0 + corner || x >= rp.x1 - corner;
        let near_y = y < rp.y0 + corner || y >= rp.y1 - corner;
        !(near_x && near_y)
    })
}

/// The part of `view` above a list's fade (`fade`'s top, in points): what the list shows at
/// full strength.
fn above_fade(view: PxBox, fade: Option<(f64, f64)>, scale: f64) -> PxBox {
    match fade {
        Some((top, _)) => PxBox {
            y1: view.y1.min((top * scale).floor() as i64),
            ..view
        },
        None => view,
    }
}

/// `r` grown by `by` points on every side (shrunk when negative).
fn grow(r: Rect, by: f64) -> Rect {
    Rect {
        x: r.x - by,
        y: r.y - by,
        w: (r.w + 2.0 * by).max(0.0),
        h: (r.h + 2.0 * by).max(0.0),
    }
}

/// The ink among `px` against `ground`: the most contrasting colour at least [`SOLID_COUNT`]
/// pixels share, else the most contrasting single pixel; `None` when every pixel is the ground.
#[must_use]
pub fn solid_ink(px: &[Rgb], ground: Rgb) -> Option<Rgb> {
    let mut counts: HashMap<Rgb, usize> = HashMap::new();
    for &p in px {
        if p != ground {
            *counts.entry(p).or_default() += 1;
        }
    }
    let best = |solid: bool| {
        counts
            .iter()
            .filter(|(_, n)| !solid || **n >= SOLID_COUNT)
            .map(|(p, _)| (contrast(*p, ground), *p))
            .max_by(|a, b| a.0.total_cmp(&b.0).then(b.1.cmp(&a.1)))
            .map(|(_, p)| p)
    };
    best(true).or_else(|| best(false))
}

/// Text against its background: each visible node whose words a `paint-text` line audits, its
/// background the most common colour of its undimmed part ([`Node::undimmed`]) and its ink
/// [`solid_ink`]. Words a scroll edge cuts ([`text_scroll_cut`], which [`clip_check`] judges) or a
/// list fades out ([`Node::faded`]) are passed over.
fn text_check(frame: &Image, layout: &Layout) -> Check {
    let mut check = Check::new("text");
    let view = layout.viewport;
    for node in &layout.nodes {
        if !node.visible || !TEXT_KINDS.contains(&node.kind.as_str()) {
            continue;
        }
        let Some(fit) = layout.texts.iter().find(|t| t.key == node.key) else {
            continue;
        };
        if fit.painted.trim().is_empty() {
            continue;
        }
        if !text_visible(node, fit) {
            check.skip("hidden");
            continue;
        }
        if node.disabled {
            check.skip("disabled");
            continue;
        }
        if text_scroll_cut(node, fit) {
            check.skip("cut");
            continue;
        }
        if node.faded() {
            check.skip("faded");
            continue;
        }
        let area = node.undimmed().intersect(view);
        if area.is_empty() {
            check.skip("hidden");
            continue;
        }
        let px = frame.region(area.px(layout.scale));
        let Some(ground) = mode(px.iter().copied()) else {
            check.skip("hidden");
            continue;
        };
        match solid_ink(&px, ground) {
            Some(ink) => check.ratio(
                &node.key,
                contrast(ink, ground),
                TEXT_MIN,
                format!("{:?}: ink {} on {}", fit.painted, hex(ink), hex(ground)),
            ),
            None => {
                check.judged += 1;
                check.worst = Some(1.0);
                check.failures.push(Finding {
                    key: node.key.clone(),
                    value: 1.0,
                    detail: format!("{:?}: no ink on {}", fit.painted, hex(ground)),
                });
            }
        }
    }
    check
}

/// A glyph against its wash and the ground: each visible painter-drawn mark (a `custom` node no
/// thinner than a hairline), its wash the most common colour just outside it, its ground the
/// card (or page) it sits on, its ink [`solid_ink`] against the wash. A mark that paints nothing
/// here is `blank`, and one a scroll edge cuts or a list fades out is `cut` or `faded`, not
/// judged.
fn glyph_check(frame: &Image, layout: &Layout, surfaces: &Surfaces) -> Check {
    let mut check = Check::new("glyph");
    let scale = layout.scale;
    let view = layout.viewport.px(scale);
    for node in &layout.nodes {
        if !node.visible
            || node.kind != "custom"
            || node.rect.w.min(node.rect.h) < HAIRLINE_PT
            || is_card(node)
        {
            continue;
        }
        if node.rect.intersect(node.clip).is_empty() {
            check.skip("hidden");
            continue;
        }
        if scroll_cut(node) {
            check.skip("cut");
            continue;
        }
        if node.faded() {
            check.skip("faded");
            continue;
        }
        let area = node.undimmed().intersect(layout.viewport);
        if area.is_empty() {
            check.skip("hidden");
            continue;
        }
        let (cx, cy) = (area.x + area.w / 2.0, area.y + area.h / 2.0);
        let ground = surfaces.ground_at(cx, cy);
        let inner = area.px(scale);
        let ring = PxBox {
            x0: inner.x0 - WASH_RING.1,
            y0: inner.y0 - WASH_RING.1,
            x1: inner.x1 + WASH_RING.1,
            y1: inner.y1 + WASH_RING.1,
        };
        let near = PxBox {
            x0: inner.x0 - WASH_RING.0 + 1,
            y0: inner.y0 - WASH_RING.0 + 1,
            x1: inner.x1 + WASH_RING.0 - 1,
            y1: inner.y1 + WASH_RING.0 - 1,
        };
        let wash = mode(pixels_where(frame, ring, |x, y| {
            !near.holds(x, y) && view.holds(x, y)
        }))
        .unwrap_or(ground);
        let px = frame.region(inner);
        let Some(ink) = solid_ink(&px, wash) else {
            check.skip("blank");
            continue;
        };
        let (on_wash, on_ground) = (contrast(ink, wash), contrast(ink, ground));
        check.ratio(
            &node.key,
            on_wash.min(on_ground),
            GLYPH_MIN,
            format!(
                "ink {} on wash {} ({on_wash:.2}) and ground {} ({on_ground:.2})",
                hex(ink),
                hex(wash),
                hex(ground)
            ),
        );
    }
    check
}

/// A card against the page: each visible group whose surface is a card, its colour read just
/// inside its edge and the page's just outside it.
fn card_check(layout: &Layout, surfaces: &Surfaces) -> Check {
    let mut check = Check::new("card");
    for card in &surfaces.cards {
        if card.nested {
            check.skip("nested");
            continue;
        }
        // A sliver the list has faded wholly into the page shows no surface to judge.
        let (Some(colour), Some(around)) = (card.colour, card.around) else {
            check.skip("faded");
            continue;
        };
        check.ratio(
            &layout.nodes[card.node].key,
            contrast(colour, around),
            CARD_MIN,
            format!("card {} on page {}", hex(colour), hex(around)),
        );
    }
    check
}

/// Words a scroll edge cuts: their visible part keeps under [`SCROLL_KEEP`] of their line's
/// height, or loses more than half a point of its width. The audit's own `clipped` passes over
/// a scroll viewport's edge (its content may peek out); a person still meets the words cut.
fn scroll_cut(node: &Node) -> bool {
    let shown = node.rect.intersect(node.clip);
    !shown.is_empty() && (shown.h < node.rect.h * SCROLL_KEEP || shown.w + 0.5 < node.rect.w)
}

fn text_visible(node: &Node, fit: &TextFit) -> bool {
    node.visible
        && !node.rect.intersect(node.clip).is_empty()
        && fit
            .word_band
            .is_none_or(|(top, bottom)| bottom > node.clip.y && top < node.clip.bottom())
}

/// Painted words can fit even when a scroll edge removes their line box's padding.
/// Without bounded word geometry, retain the line-box judgement.
fn text_scroll_cut(node: &Node, fit: &TextFit) -> bool {
    let Some((top, bottom)) = fit.word_band else {
        return scroll_cut(node);
    };
    let shown = node.rect.intersect(node.clip);
    // Audit coordinates are rounded to tenths of a point.
    !shown.is_empty()
        && (top + 0.1 < node.clip.y
            || bottom > node.clip.bottom() + 0.1
            || shown.w + 0.5 < node.rect.w)
}

/// Whether the edge that cuts the node is only its list's faded bottom edge: its whole width
/// shown, its bottom cut where its list fades its rows out.
fn cut_into_fade(node: &Node) -> bool {
    let shown = node.rect.intersect(node.clip);
    shown.w + 0.5 >= node.rect.w
        && node.fade.is_some_and(|(top, bottom)| {
            shown.bottom() >= top - 0.5 && shown.bottom() <= bottom + 0.5
        })
}

/// No clipped or ellipsised text: every visible node's words that were ellipsised, need more
/// room than they have, or are cut by a clip edge (the audit's own judgement) or a scroll edge
/// ([`text_scroll_cut`]). Words scrolled wholly out of view are `hidden`, and words a list's faded
/// bottom edge cuts are `faded` ([`cut_into_fade`]), not failed.
fn clip_check(layout: &Layout) -> Check {
    let mut check = Check::new("clip");
    for node in &layout.nodes {
        if !TEXT_KINDS.contains(&node.kind.as_str()) {
            continue;
        }
        let Some(fit) = layout.texts.iter().find(|t| t.key == node.key) else {
            continue;
        };
        if fit.painted.trim().is_empty() {
            continue;
        }
        if !text_visible(node, fit) {
            check.skip("hidden");
            continue;
        }
        let (r, c) = (node.rect, node.clip);
        let place = format!(
            "rect {:.1},{:.1},{:.1},{:.1} clip {:.1},{:.1},{:.1},{:.1}",
            r.x, r.y, r.w, r.h, c.x, c.y, c.w, c.h
        );
        let detail = if fit.clip_truncated || (node.clipped && !fit.overflow) {
            Some(format!("{:?} cut by a clip edge ({place})", fit.painted))
        } else if fit.overflow {
            Some(format!(
                "{:?} ellipsised: needs {:.1} pt, has {:.1}",
                fit.painted, fit.required, fit.available
            ))
        } else if text_scroll_cut(node, fit) && cut_into_fade(node) {
            check.skip("faded");
            continue;
        } else if text_scroll_cut(node, fit) {
            Some(format!("{:?} cut by a scroll edge ({place})", fit.painted))
        } else {
            None
        };
        check.judged += 1;
        if let Some(detail) = detail {
            check.failures.push(Finding {
                key: node.key.clone(),
                value: 1.0,
                detail,
            });
        }
    }
    check.worst = Some(check.failures.len() as f64);
    check
}

/// How far `v` lies from the nearest multiple of [`RHYTHM_PT`].
fn off_grid(v: f64) -> f64 {
    let r = v.rem_euclid(RHYTHM_PT);
    r.min(RHYTHM_PT - r)
}

/// The 4 pt vertical rhythm: every visible node's top (from the page's top) and its height on
/// the grid. Hairlines and the page's own full-height backdrops are not judged.
fn rhythm_check(layout: &Layout) -> Check {
    let mut check = Check::new("rhythm");
    let view = layout.viewport;
    let mut worst: f64 = 0.0;
    for node in &layout.nodes {
        if !node.visible || node.rect.h < HAIRLINE_PT || node.rect.h >= view.h - 1.0 {
            continue;
        }
        check.judged += 1;
        let top = node.rect.y - view.y;
        let off = off_grid(top).max(off_grid(node.rect.h));
        if off > RHYTHM_SLACK {
            worst = worst.max(off);
            check.failures.push(Finding {
                key: node.key.clone(),
                value: round_to(off, 2),
                detail: format!("{} top {top:.1} height {:.1}", node.kind, node.rect.h),
            });
        }
    }
    check.worst = (check.judged > 0).then_some(round_to(worst, 2));
    check
}

/// Every check on one frame and its layout, in the report's order.
#[must_use]
pub fn measure(frame: &Image, layout: &Layout) -> Vec<Check> {
    let surfaces = Surfaces::read(frame, layout);
    vec![
        text_check(frame, layout),
        glyph_check(frame, layout, &surfaces),
        card_check(layout, &surfaces),
        clip_check(layout),
        rhythm_check(layout),
    ]
}

/// Whether the layout's audit names each group's surface: a build from before the `surface`
/// field cannot be judged for cards.
#[must_use]
pub fn names_surfaces(layout: &Layout) -> bool {
    layout.nodes.iter().any(|n| n.surface.is_some())
}

#[cfg(test)]
#[path = "system_state_measure_tests.rs"]
mod tests;
