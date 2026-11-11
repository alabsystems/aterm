// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
