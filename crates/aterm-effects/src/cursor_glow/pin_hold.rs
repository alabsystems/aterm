// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A PINNED TRAIL'S LIGHTS WEAR THE PIN ON A DARK PAGE (composed r1's judges, O2(b) and O4(d);
//! the pin ruling: a colour the user chose keeps its hue and chroma, and how much light it lays
//! is not its colour).
//!
//! On a dark page each style lifts a chosen colour to the light it lays by scaling its channels
//! toward full, which keeps its HSV saturation and so gains chroma with its value: the slate pin
//! `#20242c` (chroma 12) laid Comet's blade in `#99add5` (chroma 60) and Sparkle's rail tip in
//! `#bed1fa`, and Beam's sparks kept their own white, ice and violet tints (`#b0a6f5`, chroma
//! 79). After every body has laid its light and before the letter guard weighs it
//! ([`crate::cursor_fx::CursorFx::tick`]), each light of the frame's streams (the over quads,
//! halos and under quads, and both streams' ramps) is worn in the pin's hue at the light it
//! lays on the page (the luminance of the two added), at the lower of its own chroma and the
//! chroma the pin's light has there ([`pin_light_at`]): a white glint stays white, a lifted
//! slate stays near-neutral grey, and a built-in hue takes the pin's. The hold is two-sided: a
//! bright light keeps its own chroma up to the pin's, even where the pin's ray has paled past
//! it ([`kept_chroma`]), so a vivid pin's light at or under the pin's chroma is no duller for
//! the hold, while a dim light, where a grey pin's lights stack, still takes the ray's share.
//! A light laid past the pin's chroma is still cut to it, and where such lights stack at part
//! strength the pixels they lay read duller than unheld: Classic's pinned rail is two lights of
//! `#c4006c` (chroma 196 against the pin's 150), an under quad's and a ramp's, at about three
//! quarters, and reads `#710b47`-`#8a255f` where unheld it read `#98085a` (composed r3's
//! replay; r2's one-sided hold laid `#6e3357`). Each light lays the page as bright as it did,
//! so what the letter guard and the caret's floors read of it is what they read before.
//! Controls: [`CursorGlow::set_pin_chroma`], [`CursorGlow::set_pin_hold_two_sided`].
//!
//! The pin's light is light, not ink: it adds onto the page and onto every other light laid
//! where it lies (a bloom, a halo over a quad). Its path is a ray from black to the pin moved by
//! one shift on every channel to its first clip ([`pin_top`], the pin's own hue and absolute
//! chroma), then on toward white, so a dim light carries the share of the pin's chroma its light
//! is of the top's. A path that held the pin's chroma down to black (an ink's) laid each dim
//! light at the whole of it, and the lights stacked where they met: Beam's head summed five such
//! layers to chroma 52 under a pin of 12.

use super::{CursorGlow, GlowConfig};
use aterm_core::render::{GlowQuad, end_alpha, end_color, glow_end};

/// A light whose hue lies within this many degrees of the pin's, at no more chroma than the pin
/// has at its luminance (with [`CHROMA_KEPT`] to spare), is already the pin's and is left as it
/// is.
const HUE_KEPT_DEG: f32 = 3.0;

/// The chroma a light already in the pin's hue may carry past the pin's at its luminance and
/// stay as it is: a rounding step's worth of the style's own lift, well under the judges' 6.
const CHROMA_KEPT: u32 = 4;

/// The brightest channel from which a light keeps any of its own chroma past the pin's share
/// ([`kept_chroma`]), under a grey pin; a vivid pin's scales by its distance from grey.
const KEEP_FROM: f32 = 64.0;
/// The brightest channel from which a light keeps its own chroma up to the pin's whole
/// ([`kept_chroma`]), under a grey pin.
const KEEP_WHOLE: f32 = 128.0;

/// How many recent lights one pass remembers (a frame's lights repeat their colours).
const MEMO: usize = 64;

/// One pass's recolouring of lights under `pin`, remembering the last [`MEMO`] lights.
struct PinHold {
    pin: u32,
    pin_hue: f32,
    page: u32,
    /// The hold cuts every light to the pin's share (a test's control).
    one_sided: bool,
    memo: [(u32, u32); MEMO],
}

impl PinHold {
    fn new(pin: u32, page: u32) -> Self {
        let (pin_hue, _, _) = crate::color_math::rgb2hsv(pin);
        Self {
            pin,
            pin_hue,
            page: page & 0x00FF_FFFF,
            one_sided: false,
            // A key with bit 31 set is no colour: every slot starts empty.
            memo: [(u32::MAX, 0); MEMO],
        }
    }

    /// `light` (`0x00RRGGBB`) worn in the pin ([`self`]).
    fn wear(&mut self, light: u32) -> u32 {
        let light = light & 0x00FF_FFFF;
        let slot = (light.wrapping_mul(0x9E37_79B1) >> 26) as usize % MEMO;
        if self.memo[slot].0 == light {
            return self.memo[slot].1;
        }
        let worn = pinned_light(self, light);
        self.memo[slot] = (light, worn);
        worn
    }
}

/// A colour's chroma: its largest channel less its least.
#[inline]
const fn chroma(c: u32) -> u32 {
    let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
    let hi = if r > g { r } else { g };
    let hi = if hi > b { hi } else { b };
    let lo = if r < g { r } else { g };
    let lo = if lo < b { lo } else { b };
    hi - lo
}

/// The pin moved by one shift on every channel until its first channel meets full: the
/// brightest light of its own hue and absolute chroma.
#[inline]
const fn pin_top(pin: u32) -> u32 {
    let (r, g, b) = ((pin >> 16) & 0xff, (pin >> 8) & 0xff, pin & 0xff);
    let hi = if r > g { r } else { g };
    let hi = if hi > b { hi } else { b };
    let k = 255 - hi;
    ((r + k) << 16) | ((g + k) << 8) | (b + k)
}

/// `light` added onto `page` channel by channel, as the renderer adds a light at alpha 0.
#[inline]
fn on_page(page: u32, light: u32) -> u32 {
    let ch = |sh: u32| (((page >> sh) & 0xff) + ((light >> sh) & 0xff)).min(255) << sh;
    ch(16) | ch(8) | ch(0)
}

/// The pin's light that lays `page` as bright as `light` lays it (the WCAG luminance of the
/// two added onto the page): along the ray from black to [`pin_top`], then from it on toward
/// white, bisected on that luminance. A light's luminance alone is not what it lays: added onto
/// a tinted page, a light with more green lays more light than one of its luminance with more
/// red, and a hold that kept the light's own luminance lifted the halo round Laser's pinned bolt
/// (its caret read 4.35:1 where it read 4.52).
fn pin_light_at(pin: u32, page: u32, light: u32) -> u32 {
    let top = pin_top(pin);
    let at = |t: f32| -> u32 {
        let ch = |sh: u32| -> u32 {
            let c = ((top >> sh) & 0xff) as f32;
            let v = if t <= 1.0 {
                c * t
            } else {
                c + (255.0 - c) * (t - 1.0)
            };
            v.round().clamp(0.0, 255.0) as u32
        };
        (ch(16) << 16) | (ch(8) << 8) | ch(0)
    };
    let table = crate::color_math::relative_luminance_table();
    let lays = |c: u32| crate::color_math::relative_luminance_with(table, on_page(page, c));
    let want = lays(light);
    let (mut lo, mut hi) = (0.0f32, 2.0f32);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if lays(at(mid)) < want {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (a, z) = (at(lo), at(hi));
    if want - lays(a) <= lays(z) - want {
        a
    } else {
        z
    }
}

/// The grey that lays `page` as bright as `light` lays it (as [`pin_light_at`] weighs it).
fn grey_laying(page: u32, light: u32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    let lays = |c: u32| crate::color_math::relative_luminance_with(table, on_page(page, c));
    let want = lays(light);
    let grey = |v: u32| (v << 16) | (v << 8) | v;
    let (mut lo, mut hi) = (0u32, 255u32);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if lays(grey(mid)) < want {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    if want - lays(grey(lo)) <= lays(grey(hi)) - want {
        grey(lo)
    } else {
        grey(hi)
    }
}

/// `light` in the pin's hue at the light it lays on the hold's page, at the chroma it keeps
/// ([`kept_chroma`]).
fn pinned_light(hold: &PinHold, light: u32) -> u32 {
    let (pin, pin_hue, page) = (hold.pin, hold.pin_hue, hold.page);
    let own = chroma(light);
    if own == 0 {
        return light;
    }
    let at = pin_light_at(pin, page, light);
    let most = chroma(at);
    let kept = kept_chroma(chroma(pin), own, most, brightest(light), hold.one_sided);
    let (hue, _, _) = crate::color_math::rgb2hsv(light);
    let off = (hue - pin_hue).abs();
    if own <= kept + CHROMA_KEPT && off.min(360.0 - off) <= HUE_KEPT_DEG {
        return light;
    }
    if kept == most || most == 0 {
        return at;
    }
    // The pin's hue at the chroma kept, laying the page as `at` lays it: the grey there moved
    // toward `at` (both in the pin's hue), past it where a light keeps more than the pin's share,
    // no further than every channel stays in range (a clipped channel turns the hue).
    let grey = grey_laying(page, at);
    let mut share = kept as f32 / most as f32;
    for sh in [16u32, 8, 0] {
        let g = ((grey >> sh) & 0xff) as f32;
        let d = ((at >> sh) & 0xff) as f32 - g;
        if d > 0.0 {
            share = share.min((255.0 - g) / d);
        } else if d < 0.0 {
            share = share.min(g / -d);
        }
    }
    let mix = |sh: u32| -> u32 {
        let g = ((grey >> sh) & 0xff) as f32;
        let p = ((at >> sh) & 0xff) as f32;
        (g + (p - g) * share).round().clamp(0.0, 255.0) as u32
    };
    (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

/// A colour's brightest channel.
#[inline]
const fn brightest(c: u32) -> u32 {
    let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
    let hi = if r > g { r } else { g };
    if hi > b { hi } else { b }
}

/// THE HOLD IS TWO-SIDED (composed r2's identity judge, O9(e): "hue and chroma stay" means
/// neither raised nor cut). The chroma a light of chroma `own` and brightest channel `top`
/// keeps under a pin of chroma `pin_chroma`, where the pin's light at its luminance has `most`:
/// at least the lower of its own and the pin's share there ([`pin_light_at`], the stacking guard
/// the slate pin needed: dim lights that meet add up to no more than the pin), and its own up to
/// the pin's once it is bright enough not to stack past it ([`KEEP_FROM`] to [`KEEP_WHOLE`],
/// scaled by how near grey the pin is: a near-grey pin has no chroma to spare for two lights
/// that meet). The share alone cut a vivid pin's light under the pin: Classic pink's rail on the
/// dark page went from `#8d0954` (OKLCH chroma 0.169) to `#6e3357` (0.096), the pin at 0.200,
/// and every pinned style's light under the pink pin read 20-27% duller. Control:
/// [`CursorGlow::set_pin_hold_two_sided`].
#[inline]
fn kept_chroma(pin_chroma: u32, own: u32, most: u32, top: u32, one_sided: bool) -> u32 {
    let share = own.min(most);
    if one_sided {
        return share;
    }
    let grey = 1.0 - pin_chroma as f32 / 255.0;
    let (from, whole) = (KEEP_FROM * grey, KEEP_WHOLE * grey);
    let w = ((top as f32 - from) / (whole - from).max(1.0)).clamp(0.0, 1.0);
    let own_kept = (own.min(pin_chroma) as f32).min(pin_chroma as f32 * w);
    (share as f32).max(own_kept).round() as u32
}

impl CursorGlow {
    /// Every light of the frame worn in the pinned trail colour on a dark page ([`self`]): `out`
    /// (the frame's over quads) and the glow's own halos, under quads and ramps. Nothing where
    /// the trail wears no pinned colour, on a light page (whose inks the light ground's chosen
    /// solve already lays in the pin's chroma), or where a test turned the hold off.
    pub(crate) fn hold_pinned_lights(&mut self, cfg: &GlowConfig, out: &mut [GlowQuad]) {
        if !cfg.dark_theme || !self.trail_pinned(cfg) || self.pin_chroma_free() {
            return;
        }
        let mut hold = PinHold::new(cfg.color & 0x00FF_FFFF, cfg.theme_bg);
        hold.one_sided = self.pin_one_sided();
        let quads = |qs: &mut [GlowQuad], hold: &mut PinHold| {
            for q in qs.iter_mut() {
                q.color = hold.wear(q.color);
                q.color2 = hold.wear(q.color2);
            }
        };
        quads(out, &mut hold);
        quads(&mut self.under_out, &mut hold);
        for h in &mut self.halo_out {
            h.color = hold.wear(h.color);
        }
        for ramps in [&mut self.out_ramps, &mut self.under_ramps] {
            for e in &mut ramps.ends {
                *e = glow_end(hold.wear(end_color(*e)), end_alpha(*e));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLATE: u32 = 0x0020_242C;
    const PINK: u32 = 0x00D0_3A8C;
    /// The dark page the trail replays read on, and Solarized Dark's teal one.
    const PAGES: [u32; 3] = [0x0011_1318, 0x0000_2B36, 0];

    fn lum(c: u32) -> f32 {
        crate::color_math::relative_luminance_with(crate::color_math::relative_luminance_table(), c)
    }

    /// A light worn in a pin lays the page as bright as it did (the luminance of the two added
    /// onto the page, within a rounding step's worth), takes no more chroma than it keeps
    /// ([`kept_chroma`]: never more than it had or than the pin's own) and wears the pin's hue;
    /// white stays white.
    #[test]
    fn a_light_worn_in_the_pin_keeps_its_light_and_takes_the_pin_s_hue() {
        for (pin, page) in [SLATE, PINK, 0x00B0_B0C8]
            .into_iter()
            .flat_map(|pin| PAGES.map(|page| (pin, page)))
        {
            let mut hold = PinHold::new(pin, page);
            let (pin_hue, _, _) = crate::color_math::rgb2hsv(pin);
            for light in [
                0x0099_ADD5,
                0x00BE_D1FA,
                0x00A9_8BFF,
                0x00CF_ECFF,
                0x00B0_A6F5,
                0x0051_5B70,
                0x0012_0A30,
                0x00FF_FFFF,
                0x00FF_E588,
                0x0040_4040,
            ] {
                let worn = hold.wear(light);
                let (l0, l1) = (lum(on_page(page, light)), lum(on_page(page, worn)));
                assert!(
                    (l1 - l0).abs() <= 0.01 + 0.03 * l0,
                    "{light:06x} under {pin:06x} on {page:06x}: it laid {l0:.4}, worn {l1:.4} \
                     ({worn:06x})"
                );
                let most = chroma(pin_light_at(pin, page, light));
                let kept = kept_chroma(chroma(pin), chroma(light), most, brightest(light), false);
                assert!(
                    chroma(worn) <= kept + CHROMA_KEPT + 2
                        && chroma(worn) <= chroma(light).min(chroma(pin)) + CHROMA_KEPT + 2,
                    "{light:06x} under {pin:06x}: chroma {} (own {}, kept {kept}, the pin's share \
                     {most})",
                    chroma(worn),
                    chroma(light)
                );
                if chroma(worn) >= 12 {
                    let (h, _, _) = crate::color_math::rgb2hsv(worn);
                    let d = (h - pin_hue).abs();
                    assert!(
                        d.min(360.0 - d) <= 12.0,
                        "{light:06x} under {pin:06x}: hue {h:.0}, the pin's {pin_hue:.0}"
                    );
                }
            }
            assert_eq!(hold.wear(0x00FF_FFFF), 0x00FF_FFFF, "white stays white");
        }
        // A dim light carries its share of the pin's chroma, so lights that meet add up to no
        // more than the pin's: five of the slate's dim lights stacked stay within its chroma.
        let mut hold = PinHold::new(SLATE, PAGES[0]);
        let dim = hold.wear(0x0012_0A30);
        let five = [16u32, 8, 0].iter().fold(0u32, |acc, &sh| {
            acc | ((((dim >> sh) & 0xff) * 5).min(255) << sh)
        });
        assert!(
            chroma(five) <= chroma(SLATE) + 2,
            "five stacked {dim:06x} read chroma {} ({five:06x})",
            chroma(five)
        );
    }

    /// THE HOLD IS TWO-SIDED ([`kept_chroma`]): a vivid pin's light already in its hue and
    /// under its chroma is kept as it is, where the share alone dulled it (a light of the colour
    /// composed r1 laid Classic pink's dark rail in, `#8d0954`, chroma 132 under the pin's 150;
    /// one-sided it goes to chroma 68). Negative control (the hold one-sided, as composed r2's):
    /// it is cut to under three quarters of its chroma.
    #[test]
    fn a_vivid_pin_s_own_light_keeps_its_chroma() {
        const RAIL: u32 = 0x008D_0954;
        for page in [PAGES[0], PAGES[1]] {
            let mut hold = PinHold::new(PINK, page);
            assert_eq!(
                hold.wear(RAIL),
                RAIL,
                "the rail on {page:06x} keeps its colour"
            );
            let mut cut = PinHold::new(PINK, page);
            cut.one_sided = true;
            let worn = cut.wear(RAIL);
            assert!(
                4 * chroma(worn) < 3 * chroma(RAIL),
                "negative control: one-sided, the rail on {page:06x} keeps chroma {} ({worn:06x})",
                chroma(worn)
            );
        }
    }
}
