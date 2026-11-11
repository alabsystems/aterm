// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn posted(content_off: [f32; 2], dest: (u32, u32)) -> GlPosted {
    let mut blit = BlitUniform::bell(false);
    blit.dims = [800.0, 600.0];
    blit.content_off = content_off;
    GlPosted {
        blit,
        frame: (800, 600),
        dest,
        fx: Some([0, 0, 0, 0]),
        bloom: Some([0, 0, 0, 0]),
        tray: None,
        crown: [0, 0, 0, 0],
    }
}

#[test]
fn the_crown_covers_its_quads_not_its_scissor() {
    let quad = |x, y, w, h| GlowInstance {
        rect: [x, y, w, h],
        color: [255; 4],
        color2: [255; 4],
    };
    let scissor = (0, 0, 800, 600);
    let quads = [
        quad(100, 40, 20, 30),
        quad(300, 50, 10, 0),
        quad(90, 60, 5, 40),
    ];
    let lit = glow_footprint(&quads);
    assert_eq!(
        crown_footprint(lit, [0.0, 0.0], scissor),
        [90, 40, 120, 100],
        "the box of the quads that draw (an empty one draws nothing)"
    );
    assert_eq!(
        crown_footprint(lit, [3.0, -50.0], scissor),
        [93, 0, 123, 50],
        "placed by content_off, clipped to the scissor"
    );
    assert_eq!(
        crown_footprint(lit, [0.5, 0.0], (0, 0, 100, 600)),
        [90, 40, 100, 100],
        "a fractional placement covers both pixels, inside the scissor"
    );
    assert_eq!(
        crown_footprint(glow_footprint(&[]), [0.0, 0.0], scissor),
        [0, 0, 0, 0]
    );
    assert_eq!(
        crown_footprint(lit, [f32::NAN, 0.0], scissor),
        [0, 0, 800, 600],
        "an unplaceable crown covers its scissor"
    );
}

#[test]
fn a_post_s_damage_is_what_changed_since_the_last() {
    let row = Some([0, 40, 800, 70]);
    let fit = posted([0.0, 0.0], (800, 600));
    assert_eq!(
        post_damage(None, &fit, row, false),
        None,
        "the first post is whole"
    );
    assert_eq!(
        post_damage(Some(&fit), &fit, row, false),
        Some([0, 40, 800, 70])
    );
    assert_eq!(
        post_damage(Some(&fit), &fit, Some([0, 0, 0, 0]), false),
        Some([0, 0, 0, 0]),
        "nothing written, nothing posted"
    );
    assert_eq!(
        post_damage(Some(&fit), &fit, None, false),
        None,
        "unknown is whole"
    );

    let placed = posted([3.0, 5.0], (806, 610));
    assert_eq!(
        post_damage(Some(&placed), &placed, row, false),
        Some([3, 45, 803, 75]),
        "frame pixels land at the blit's placement"
    );
    let half = posted([2.5, 0.5], (806, 610));
    assert_eq!(
        post_damage(Some(&half), &half, row, false),
        Some([2, 40, 803, 71]),
        "a fractional placement covers both pixels a texel can reach"
    );
    let mut chrome = placed;
    chrome.blit.chrome_y1 = 30.0;
    assert_eq!(
        post_damage(Some(&chrome), &chrome, row, false),
        Some([0, 45, 806, 75]),
        "beside chrome the bands repeat the frame's edge: the whole width"
    );

    let mut flash = fit;
    flash.blit.flag = 1;
    assert_eq!(
        post_damage(Some(&fit), &flash, row, false),
        None,
        "a new blit is whole"
    );
    assert_eq!(
        post_damage(Some(&fit), &posted([0.0, 0.0], (801, 600)), row, false),
        None,
        "a new size is whole"
    );

    let mut halo = fit;
    halo.fx = Some([100, 300, 200, 400]);
    assert_eq!(
        post_damage(Some(&halo), &fit, row, false),
        Some([0, 40, 800, 400]),
        "an effect gone since the last post is damage"
    );
    assert_eq!(
        post_damage(Some(&fit), &halo, row, false),
        Some([0, 40, 800, 400]),
        "an effect drawn now is damage"
    );
    let mut unbounded = fit;
    unbounded.fx = None;
    assert_eq!(post_damage(Some(&unbounded), &fit, row, false), None);
    let mut crowned = fit;
    crowned.crown = [10, 500, 20, 520];
    assert_eq!(
        post_damage(Some(&crowned), &fit, row, false),
        Some([0, 40, 800, 520]),
        "a crown gone since the last post is damage"
    );

    let mut bloomed = fit;
    bloomed.bloom = Some([0, 0, 800, 600]);
    assert_eq!(
        post_damage(Some(&bloomed), &bloomed, row, true),
        Some([0, 40, 800, 70]),
        "a bloom held since the last post shows only the texels written under it"
    );
    assert_eq!(
        post_damage(Some(&bloomed), &bloomed, row, false),
        Some([0, 0, 800, 600]),
        "a bloom drawn from new inputs is damage over its scissor"
    );
    assert_eq!(
        post_damage(Some(&bloomed), &fit, row, false),
        Some([0, 0, 800, 600]),
        "a bloom gone since the last post is damage"
    );
    let mut spread = fit;
    spread.bloom = None;
    assert_eq!(post_damage(Some(&fit), &spread, row, false), None);
    assert_eq!(
        post_damage(Some(&spread), &spread, row, true),
        Some([0, 40, 800, 70]),
        "an unscissored bloom held is no damage of its own either"
    );
    let mut both = halo;
    both.bloom = Some([0, 0, 800, 600]);
    assert_eq!(
        post_damage(Some(&both), &both, row, true),
        Some([0, 40, 800, 400]),
        "a held bloom leaves the other effects' damage"
    );

    let mut card = fit;
    card.tray = Some(([0, 0, 800, 30], 7));
    assert_eq!(
        post_damage(Some(&card), &card, row, false),
        Some([0, 40, 800, 70]),
        "the same card at the same place shows nothing new"
    );
    let mut redrawn = card;
    redrawn.tray = Some(([0, 0, 800, 30], 8));
    assert_eq!(
        post_damage(Some(&card), &redrawn, row, false),
        Some([0, 0, 800, 70]),
        "a new card is damage"
    );
    let mut moved = card;
    moved.tray = Some(([0, 500, 800, 530], 7));
    assert_eq!(
        post_damage(Some(&card), &moved, row, false),
        Some([0, 0, 800, 530]),
        "a card moved is damage where it was and where it is"
    );
    assert_eq!(
        post_damage(Some(&card), &fit, row, false),
        Some([0, 0, 800, 70]),
        "a card gone is damage"
    );
}
