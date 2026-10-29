// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Prism Glass's solver laws (`docs/design/PRISM-GLASS.md` §1, §3, §15.2 1.4).

use super::scan::{Columns, Ground, LAY_LEG, SLAB_DENSITIES, all_lerps, scan};
use super::*;

/// The glass pages: the first six shipped.
fn glass_pages() -> impl Iterator<Item = &'static Palette> {
    tables::SHIPPED.iter().filter(|p| p.mode == Mode::Glass)
}

/// A page's name in [`SHIPPED_PAGES`], [`PACK_LIGHT_PAGES`] or [`SWEEP_PAGES`].
fn name(p: &Palette) -> &'static str {
    SHIPPED_PAGES
        .iter()
        .chain(&PACK_LIGHT_PAGES)
        .chain(&SWEEP_PAGES)
        .find(|(_, fg, bg)| (*fg, *bg) == (p.fg, p.bg))
        .map_or("?", |(n, _, _)| n)
}

/// The theme pack's light pages (`contrib/themes/pack/manifest.tsv`, appearance `light`): name,
/// theme fg and page, in the manifest's order. None ships a table; the census solves them.
const PACK_LIGHT_PAGES: [(&str, u32, u32); 8] = [
    ("Dawnfox", 0x0057_5279, 0x00FA_F4ED),
    ("Dayfox", 0x003D_2B5A, 0x00F6_F2EE),
    ("Flexoki Light", 0x0010_0F0F, 0x00FF_FCF0),
    ("GitHub Light High Contrast", 0x000E_1116, 0x00FF_FFFF),
    ("Iceberg Light", 0x0033_374C, 0x00E8_E9EC),
    ("Kanagawa Lotus", 0x0054_5464, 0x00F2_ECBC),
    ("Melange Light", 0x0054_433A, 0x00F1_F1F1),
    ("TokyoNight Day", 0x0037_60BF, 0x00E1_E2E7),
];

/// The pack's light pages solved by [`solve`], once for every test that reads them.
fn pack_pages() -> &'static [Palette] {
    static SOLVED: std::sync::OnceLock<Vec<Palette>> = std::sync::OnceLock::new();
    SOLVED.get_or_init(|| {
        PACK_LIGHT_PAGES
            .iter()
            .map(|&(_, fg, bg)| solve(fg, bg))
            .collect()
    })
}

/// Four synthetic lip-forward pages of the sweep in `docs/design/PRISM-GLASS.md` §15.8 item 34,
/// each named by its page: name, theme fg and page. The lip-forward rules' rejected forms lay the
/// same tables as the chosen ones on every shipped and pack page; these pages tell them apart. On
/// #e7e7e7, #e6e7ed and #eee8b8 a CYAN FOLD that gives each thin entry its nearest clean
/// neighbour leaves mud lerps where the edge run leaves none (item 33), and on #eff0f6 only the
/// arc table's gold stretch is mud, so an AMBER HOLD that reads the walk table alone, or that
/// fires on the gold anchor's reach, leaves it mud (item 32).
const SWEEP_PAGES: [(&str, u32, u32); 4] = [
    ("#e7e7e7", 0x0061_6161, 0x00E7_E7E7),
    ("#e6e7ed", 0x0061_6161, 0x00E6_E7ED),
    ("#eee8b8", 0x0061_6161, 0x00EE_E8B8),
    ("#eff0f6", 0x0067_6767, 0x00EF_F0F6),
];

/// [`SWEEP_PAGES`] solved by [`solve`], once for every test that reads them.
fn sweep_pages() -> &'static [Palette] {
    static SOLVED: std::sync::OnceLock<Vec<Palette>> = std::sync::OnceLock::new();
    SOLVED.get_or_init(|| {
        SWEEP_PAGES
            .iter()
            .map(|&(_, fg, bg)| solve(fg, bg))
            .collect()
    })
}

/// Every page the census reads: the shipped tables, the pack's light pages, then the sweep's.
fn census_pages() -> impl Iterator<Item = &'static Palette> {
    tables::SHIPPED
        .iter()
        .chain(pack_pages())
        .chain(sweep_pages())
}

/// The arc table's caret: [`caret_table`] over the arc lip.
fn caret_arc(p: &Palette) -> CaretTable {
    caret_table(p.fg, p.bg, p.mode, &p.lip_arc, &arc_arcs())
}

/// The walk table's caret, as shipped.
fn caret_walk(p: &Palette) -> CaretTable {
    CaretTable {
        fill: p.caret.to_vec(),
        fg_side: (0..GLASS_LEN).map(|i| p.caret_fg_side.has(i)).collect(),
        folded: (0..GLASS_LEN).map(|i| p.caret_folded.has(i)).collect(),
        snap: (0..GLASS_LEN - 1).map(|k| p.caret_snap.has(k)).collect(),
    }
}

/// The text of `tables.rs` for `palettes`.
fn render(palettes: &[Palette]) -> String {
    let hex = |c: u32| format!("0x{:04X}_{:04X}", c >> 16, c & 0xffff);
    let table = |out: &mut String, field: &str, t: &[u32]| {
        out.push_str(&format!("        {field}: [\n"));
        for row in t.chunks(6) {
            let cells: Vec<String> = row.iter().map(|&c| format!("{},", hex(c))).collect();
            out.push_str(&format!("            {}\n", cells.join(" ")));
        }
        out.push_str("        ],\n");
    };
    let entries = |e: Entries| {
        format!(
            "Entries([0x{:016X}, 0x{:016X}, 0x{:016X}])",
            e.0[0], e.0[1], e.0[2]
        )
    };
    let mut out = format!(
        "// SPDX-License-Identifier: Apache-2.0\n// Copyright 2026 Andrew Yates\n{}\n\n",
        aterm_census::seal::line(
            "//",
            "aterm-effects' `the_shipped_tables_are_the_generators_output` test under \
             ATERM_BLESS_GOLDEN=1"
        )
    );
    out.push_str(
        "//! The shipped light pages' Prism Glass tables (`docs/design/PRISM-GLASS.md` §1, \
         §15.2 1.4):\n\
         //! what [`super::solve`] gives each of [`super::SHIPPED_PAGES`], in its order. Hex is\n\
         //! authoritative.\n\n\
         use super::{Entries, Mode, Palette};\n\n\
         /// The shipped pages' palettes.\n\
         #[rustfmt::skip]\n",
    );
    out.push_str(&format!(
        "pub static SHIPPED: [Palette; {}] = [\n",
        palettes.len()
    ));
    for p in palettes {
        out.push_str(&format!("    // {}\n    Palette {{\n", name(p)));
        out.push_str(&format!(
            "        fg: {},\n        bg: {},\n",
            hex(p.fg),
            hex(p.bg)
        ));
        out.push_str(&format!("        mode: Mode::{:?},\n", p.mode));
        table(&mut out, "glass", &p.glass);
        table(&mut out, "lip", &p.lip);
        table(&mut out, "glass_arc", &p.glass_arc);
        table(&mut out, "lip_arc", &p.lip_arc);
        table(&mut out, "caret", &p.caret);
        out.push_str(&format!(
            "        caret_fg_side: {},\n        caret_snap: {},\n        caret_folded: {},\n",
            entries(p.caret_fg_side),
            entries(p.caret_snap),
            entries(p.caret_folded)
        ));
        out.push_str(&format!(
            "        fold_at: {},\n        cyan_snap: {},\n    }},\n",
            p.fold_at, p.cyan_snap
        ));
    }
    out.push_str("];\n");
    aterm_census::seal::stamp(&out).expect("the head carries a seal line")
}

/// **THE SHIPPED TABLES ARE THE SOLVER'S OUTPUT**, byte for byte: every shipped page solved
/// from scratch renders `tables.rs` exactly. `ATERM_BLESS_GOLDEN=1` rewrites it.
#[test]
fn the_shipped_tables_are_the_generators_output() {
    let solved: Vec<Palette> = SHIPPED_PAGES
        .iter()
        .map(|&(_, fg, bg)| solve(fg, bg))
        .collect();
    let modes: Vec<Mode> = solved.iter().map(|p| p.mode).collect();
    assert_eq!(
        modes,
        [[Mode::Glass; 6].as_slice(), &[Mode::LipForward; 2]].concat(),
        "six glass pages, then Solarized Light and Catppuccin Latte lip-forward"
    );
    let want = render(&solved);
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/rainbow_kitty/prism_glass/tables.rs");
    if std::env::var_os("ATERM_BLESS_GOLDEN").is_some_and(|v| v == "1") {
        std::fs::write(&path, &want).expect("tables.rs is writable");
        return;
    }
    assert!(
        include_str!("tables.rs") == want,
        "tables.rs is not the solver's output; regenerate it with ATERM_BLESS_GOLDEN=1 targo \
         --unverified test -p aterm-effects --lib the_shipped_tables_are_the_generators_output"
    );
}

/// The shipped pages are the light built-in schemes, and the effects tests' light fixture.
#[test]
fn the_shipped_pages_are_every_light_builtin_scheme() {
    let pack =
        |c: aterm_types::Rgb| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
    let mut light: Vec<(u32, u32)> = aterm_types::scheme::builtin_names()
        .into_iter()
        .filter_map(aterm_types::scheme::builtin)
        .map(|s| (pack(s.foreground), pack(s.background)))
        .filter(|&(_, bg)| !aterm_render::theme_is_dark(bg))
        .collect();
    light.push((0x0024_292F, 0x00FF_FFFF));
    light.sort_unstable();
    light.dedup();
    let mut shipped: Vec<(u32, u32)> = SHIPPED_PAGES.iter().map(|&(_, f, b)| (f, b)).collect();
    shipped.sort_unstable();
    assert_eq!(light, shipped);
    for &(_, fg, bg) in &SHIPPED_PAGES {
        assert!(
            Palette::shipped(fg, bg).is_some(),
            "{fg:06x} on {bg:06x} ships"
        );
    }
}

/// The census's pack pages are the theme pack's light rows: every `light` row of
/// `contrib/themes/pack/manifest.tsv`, in its order, with the foreground and background its
/// `<id>.conf` names.
#[test]
fn the_pack_light_pages_are_the_manifests_light_rows() {
    let pack = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contrib/themes/pack");
    let manifest =
        std::fs::read_to_string(pack.join("manifest.tsv")).expect("the pack's manifest reads");
    let key = |conf: &str, key: &str| {
        conf.lines()
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| k.trim() == key)
            .map(|(_, v)| v.trim().to_owned())
            .unwrap_or_else(|| panic!("no {key}"))
    };
    let light: Vec<String> = manifest
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.split('\t').collect::<Vec<_>>())
        .filter(|cols| cols.get(2) == Some(&"light"))
        .map(|cols| {
            let conf = std::fs::read_to_string(pack.join(format!("{}.conf", cols[0])))
                .unwrap_or_else(|e| panic!("{}.conf: {e}", cols[0]));
            assert_eq!(key(&conf, "appearance"), "light", "{}", cols[0]);
            format!(
                "{} {} on {}",
                cols[0],
                key(&conf, "foreground").to_lowercase(),
                key(&conf, "background").to_lowercase()
            )
        })
        .collect();
    let want: Vec<String> = PACK_LIGHT_PAGES
        .iter()
        .map(|&(n, fg, bg)| format!("{n} #{fg:06x} on #{bg:06x}"))
        .collect();
    assert_eq!(light, want);
}

/// §1: the glass anchors on Default Light are the spec's hex, but for the blue anchor, which
/// the solver lays a hair greener than the spec's `#14acff` (its hue is the spec hex's).
#[test]
fn default_lights_glass_anchors_are_the_spec_hex() {
    let (fg, bg) = (0x002B_2D33, 0x00F6_F7F9);
    let got = glass_anchors(fg, bg).map(Option::unwrap);
    let mut want = GLASS_ANCHORS;
    want[5] = 0x000E_ACFF;
    assert_eq!(
        got.map(|c| format!("{c:06x}")),
        want.map(|c| format!("{c:06x}"))
    );
}

/// §3 `glass-legibility-bar`, the light partner of `bed-legibility-bar`: on every glass page,
/// every origin interval at one, two and three slabs per cell and on the lay leg, under both
/// column models, at every alpha 1..=255, the theme fg keeps its bar plus [`LERP_BAR_GUARD`]
/// (5.35 on a 5.25 bar) over the light glass. Never sampled: every interval is scored.
#[test]
fn the_light_glass_keeps_every_glyph_at_its_bar_at_every_alpha() {
    let mut pages = 0;
    for p in glass_pages() {
        pages += 1;
        let need = glass_bar(p.fg, p.bg) + LERP_BAR_GUARD;
        let mut ground = Ground::new(p.fg, p.bg);
        for slabs in SLAB_DENSITIES.iter().chain([&LAY_LEG]) {
            for model in [Columns::K16, Columns::Gpu] {
                let r = scan(&p.glass, &mut ground, *slabs, model);
                assert!(
                    r.intervals > GLASS_LEN,
                    "{}: the scan covers intervals",
                    name(p)
                );
                assert!(
                    r.worst.contrast >= need,
                    "{} at {} slab(s), step {}, {model:?}: {:.4} < {need:.4} at t {} ({:06x} -> {:06x})",
                    name(p),
                    slabs.per_cell,
                    slabs.step,
                    r.worst.contrast,
                    r.worst.t,
                    r.worst.pair.0,
                    r.worst.pair.1
                );
            }
        }
        for &c in &p.glass_arc {
            for a in 1..=255u8 {
                let o = aterm_render::over_premul(p.bg, aterm_render::premul_rgb(c, a), a);
                assert!(
                    ground.contrast(o) >= need,
                    "{}: arc glass {c:06x} at alpha {a} is {:.4}",
                    name(p),
                    ground.contrast(o)
                );
            }
        }
    }
    assert_eq!(pages, 6);
}

/// Every table a shipped page draws from, by name.
fn tables_of(p: &Palette) -> Vec<(&'static str, Vec<u32>)> {
    let mut out = Vec::new();
    if p.mode == Mode::Glass {
        out.push(("glass129", p.glass.to_vec()));
        out.push(("glass511", p.glass_arc.to_vec()));
    }
    out.push(("lip129", p.lip.to_vec()));
    out.push(("lip511", p.lip_arc.to_vec()));
    out.push(("caret129", p.caret.to_vec()));
    out.push(("caret511", caret_arc(p).fill));
    out
}

/// Whether `c` is mud in `table`: a lip or caret under either rule, a glass on the spec rule's
/// bands or under the broad rule (its chroma is the HEADROOM gate's, at the anchors).
fn mud_in(table: &str, c: u32) -> bool {
    if table.starts_with("glass") {
        mud_bands(c) || mud_broad(c)
    } else {
        mud(c)
    }
}

/// §1 `light-mud-census`: no entry of any shipped page's glass, lip or caret table, at either
/// length, is mud, nor of any light page of the theme pack or [`SWEEP_PAGES`], solved here; and
/// Default Light's glass, the spec's own census, is mud under neither whole rule. #eff0f6 is why
/// the AMBER HOLD's trigger reads the entries of both tables: its unheld gold stretch is mud in
/// the arc table alone, and its gold anchor reaches the broad rule's line at 1.95:1, so a trigger
/// on the walk table or on that reach would leave it mud.
#[test]
fn no_table_is_mud_on_any_light_theme() {
    for p in census_pages() {
        for (table, t) in tables_of(p) {
            let muddy: Vec<String> = t
                .iter()
                .filter(|&&c| mud_in(table, c))
                .map(|c| format!("{c:06x}"))
                .collect();
            assert!(muddy.is_empty(), "{} {table}: mud {muddy:?}", name(p));
        }
    }
    let dl = tables::SHIPPED.first().expect("Default Light ships first");
    for t in [dl.glass.as_slice(), &dl.glass_arc] {
        assert_eq!(t.iter().filter(|&&c| mud(c)).count(), 0);
    }
    let (_, _, eff0f6) = SWEEP_PAGES[3];
    let stretch_mud = |arcs: &[f64]| {
        arcs.iter()
            .filter(|&&a| on_gold_stretch(a) && mud(deep_forward(eff0f6, a)))
            .count()
    };
    assert_eq!(
        stretch_mud(&walk_arcs()),
        0,
        "#eff0f6's unheld walk stretch is clean"
    );
    assert!(
        stretch_mud(&arc_arcs()) > 0,
        "#eff0f6's unheld arc stretch is mud"
    );
    assert!(
        page_l_max(eff0f6, DEEP_HUES[2], LIP_FORWARD_GOLD_CR) >= LIP_FORWARD_GOLD_L,
        "#eff0f6's gold anchor reaches the broad rule's line at 1.95:1"
    );
}

/// Neighbour pairs `(k, k + 1)` of `table` on `p` that the sampler snaps across instead of
/// lerping: the lip's fold, and its cyan pair where `cyan_snap` is set.
fn snaps(p: &Palette, table: &str) -> Vec<bool> {
    let lip = |len: usize, fold: usize, cyan: usize| {
        (0..len - 1)
            .map(|k| k == fold || (p.cyan_snap && k == cyan))
            .collect::<Vec<_>>()
    };
    match table {
        "lip129" => lip(GLASS_LEN, usize::from(p.fold_at), CYAN_AT),
        "lip511" => lip(ARC_LEN, FOLD_AT_ARC, CYAN_AT_ARC),
        "caret129" => caret_walk(p).snap,
        "caret511" => caret_arc(p).snap,
        _ => vec![
            false;
            if table.ends_with("511") {
                ARC_LEN - 1
            } else {
                GLASS_LEN - 1
            }
        ],
    }
}

/// §1's LERP census: every distinct colour `lerp_rgb` can draw between two neighbours of any
/// shipped table, or of any table of the theme pack's light pages or [`SWEEP_PAGES`], but across
/// a snap, is mud under neither rule; and the deep walk table without its fold snap would draw
/// mud, as would TokyoNight Day's lip without its cyan snap, so both snaps are load-bearing.
/// #e7e7e7, #e6e7ed and #eee8b8 fold their cyan and snap too.
#[test]
fn no_lerp_between_entries_is_mud() {
    for p in census_pages() {
        for (table, t) in tables_of(p) {
            let snap = snaps(p, table);
            let mut mud_colours = Vec::new();
            for k in (0..t.len() - 1).filter(|&k| !snap[k]) {
                mud_colours.extend(
                    all_lerps(t[k], t[k + 1])
                        .into_iter()
                        .filter(|&c| mud_in(table, c)),
                );
            }
            assert!(
                mud_colours.is_empty(),
                "{} {table}: {} lerp colours are mud, first {:06x}",
                name(p),
                mud_colours.len(),
                mud_colours[0]
            );
        }
    }
    let dl = tables::SHIPPED.first().expect("Default Light ships first");
    let at = usize::from(dl.fold_at);
    let unsnapped = all_lerps(dl.lip[at], dl.lip[at + 1])
        .into_iter()
        .filter(|&c| mud_spec(c))
        .count();
    assert!(
        unsnapped > 0,
        "Default Light's fold pair lerps through mud without its snap"
    );
    assert_eq!((CYAN_AT, CYAN_AT_ARC), (76, 310));
    assert!(
        tables::SHIPPED.iter().all(|p| !p.cyan_snap),
        "no shipped page snaps its cyan"
    );
    let tnd = pack_pages()
        .iter()
        .find(|p| name(p) == "TokyoNight Day")
        .expect("TokyoNight Day is a pack page");
    assert!(tnd.cyan_snap, "TokyoNight Day folds its cyan and snaps");
    assert!(
        sweep_pages()[..3].iter().all(|p| p.cyan_snap),
        "#e7e7e7, #e6e7ed and #eee8b8 fold their cyan and snap"
    );
    for (table, t, at) in [
        ("lip129", &tnd.lip[..], CYAN_AT),
        ("lip511", &tnd.lip_arc[..], CYAN_AT_ARC),
    ] {
        let unsnapped = all_lerps(t[at], t[at + 1])
            .into_iter()
            .filter(|&c| mud(c))
            .count();
        assert!(
            unsnapped > 0,
            "TokyoNight Day's {table} cyan pair {at} lerps through mud without its snap"
        );
    }
}

/// §1 `caret-cut-out`: every caret entry's cut-out keeps [`CARET_BAR`] against its side, and so
/// does every colour `lerp_rgb` draws between unsnapped neighbours, on every shipped page, at
/// both lengths, on every light page of the theme pack and on [`SWEEP_PAGES`]. Solarized Light's
/// walk caret, the shipped page with the least room, keeps 4.6 too. The walk caret, fill, sides,
/// folds and snaps, is [`caret_table`] over the walk lip the page ships with, so a lip-forward
/// page's caret is solved from its held and folded lip.
#[test]
fn every_caret_lerp_keeps_its_cut_out() {
    for p in census_pages() {
        assert_eq!(
            caret_walk(p),
            caret_table(p.fg, p.bg, p.mode, &p.lip, &walk_arcs()),
            "{}: the walk caret is the caret table of the walk lip it ships with",
            name(p)
        );
        for caret in [caret_walk(p), caret_arc(p)] {
            let side = |k: usize| if caret.fg_side[k] { p.fg } else { p.bg };
            for (k, &fill) in caret.fill.iter().enumerate() {
                assert!(
                    contrast(fill, side(k)) >= CARET_BAR,
                    "{} caret {k} {fill:06x}: cut-out {:.4}",
                    name(p),
                    contrast(fill, side(k))
                );
            }
            for k in (0..caret.fill.len() - 1).filter(|&k| !caret.snap[k]) {
                assert_eq!(caret.fg_side[k], caret.fg_side[k + 1], "a side flip snaps");
                for c in all_lerps(caret.fill[k], caret.fill[k + 1]) {
                    assert!(
                        contrast(c, side(k)) >= CARET_BAR,
                        "{} caret lerp {k}: {c:06x} cuts out {:.4}",
                        name(p),
                        contrast(c, side(k))
                    );
                }
            }
        }
    }
}

/// §1: a page whose glass anchors lose their chroma at the floor has no headroom and draws no
/// glass, only its lip; and a page that is not light draws nothing. On every census page that
/// draws lip-forward, every lip129 and lip511 entry keeps [`LIP_FORWARD_CR`] against the page, or
/// [`LIP_FORWARD_GOLD_CR`] where its own hue lies in the broad rule's band (h 60..115), so the
/// colours the LIP-FORWARD AMBER HOLD (at h 58.5) and the CYAN FOLD lay are held to the full bar.
#[test]
fn a_theme_without_headroom_draws_no_glass() {
    let mut lip_forward = 0;
    for p in census_pages() {
        assert_eq!(headroom(p.fg, p.bg), p.mode == Mode::Glass, "{}", name(p));
        if p.mode != Mode::LipForward {
            continue;
        }
        lip_forward += 1;
        assert!(
            p.glass.iter().chain(&p.glass_arc).all(|&c| c == 0),
            "{}",
            name(p)
        );
        for (table, t) in [("lip129", &p.lip[..]), ("lip511", &p.lip_arc[..])] {
            for (i, &c) in t.iter().enumerate() {
                let gold = (BROAD_MUD_HUES.0..=BROAD_MUD_HUES.1).contains(&lch(c).h);
                let bar = if gold {
                    LIP_FORWARD_GOLD_CR
                } else {
                    LIP_FORWARD_CR
                };
                assert!(
                    contrast(c, p.bg) >= bar,
                    "{} {table} {i} {c:06x}: {:.4} against the page, under {bar}",
                    name(p),
                    contrast(c, p.bg)
                );
            }
        }
    }
    assert_eq!(
        lip_forward, 10,
        "two shipped pages, four pack pages and the sweep's four draw lip-forward"
    );
    let dark = solve(0x00DC_DFE4, 0x0028_2C34);
    assert_eq!(dark.mode, Mode::Paper);
    assert!(dark.glass.iter().chain(&dark.lip).all(|&c| c == 0));
    assert!(!headroom(0x00DC_DFE4, 0x0028_2C34));
}

/// §15.8 items 32 and 33 on the two pack pages whose lips they change, against the lip each entry
/// solves to without them ([`deep_forward`]). The gold stretch is walk 20..=39 and arc 77..=155.
/// The LIP-FORWARD AMBER HOLD changes all of it on Kanagawa Lotus, and all but arc 77 (which
/// solves to the held colour already) on TokyoNight Day, and lays one colour across each page's
/// whole stretch: the lip-forward lip at h 58.5, #fb8a00 and #f18500, within 0.5° of
/// [`AMBER_HOLD_HUE`] and at [`LIP_FORWARD_CR`]. The LIP-FORWARD CYAN FOLD changes TokyoNight
/// Day's walk 73..=77 and arc 292..=317, each run taking its zone's clean entry beside it: walk
/// 73..=76 take entry 72's #00b4ab and 77 takes 78's #00b1c7; arc 292..=310 take 291's #00b4ab and
/// 311..=317 take 318's #00b1c6. Each keeps C >= [`HEADROOM_C`] and [`LIP_FORWARD_CR`].
#[test]
fn the_lip_forward_rules_lay_one_amber_and_fold_tokyonight_days_cyan() {
    let (walk, arc) = (walk_arcs(), arc_arcs());
    let stretch = |arcs: &[f64]| -> Vec<usize> {
        (0..arcs.len())
            .filter(|&i| on_gold_stretch(arcs[i]))
            .collect()
    };
    let span = |r: std::ops::RangeInclusive<usize>| r.collect::<Vec<usize>>();
    assert_eq!(stretch(&walk), span(20..=39));
    assert_eq!(stretch(&arc), span(77..=155));
    let page = |n: &str| {
        pack_pages()
            .iter()
            .find(|p| name(p) == n)
            .unwrap_or_else(|| panic!("{n} is a pack page"))
    };
    let changed = |p: &Palette, t: &[u32], arcs: &[f64]| -> Vec<usize> {
        (0..t.len())
            .filter(|&i| t[i] != deep_forward(p.bg, arcs[i]))
            .collect()
    };
    for (n, amber, walk_changed, arc_changed) in [
        ("Kanagawa Lotus", 0x00FB_8A00, span(20..=39), span(77..=155)),
        (
            "TokyoNight Day",
            0x00F1_8500,
            [span(20..=39), span(73..=77)].concat(),
            [span(78..=155), span(292..=317)].concat(),
        ),
    ] {
        let p = page(n);
        assert_eq!(changed(p, &p.lip, &walk), walk_changed, "{n} lip129");
        assert_eq!(changed(p, &p.lip_arc, &arc), arc_changed, "{n} lip511");
        for (table, t, at) in [
            ("lip129", &p.lip[..], stretch(&walk)),
            ("lip511", &p.lip_arc[..], stretch(&arc)),
        ] {
            let other: Vec<u32> = at.iter().map(|&i| t[i]).filter(|&c| c != amber).collect();
            assert!(
                other.is_empty(),
                "{n} {table}: the stretch lays {other:06x?} beside {amber:06x}"
            );
        }
        let held = lch(amber);
        assert!(
            (held.h - AMBER_HOLD_HUE).abs() < 0.5,
            "{n}: {amber:06x} at h {:.2}",
            held.h
        );
        assert!(
            contrast(amber, p.bg) >= LIP_FORWARD_CR,
            "{n}: {amber:06x} at {:.4}",
            contrast(amber, p.bg)
        );
    }
    let tnd = page("TokyoNight Day");
    for (table, t, folds) in [
        (
            "lip129",
            &tnd.lip[..],
            [(73..=76, 72, 0x0000_B4AB), (77..=77, 78, 0x0000_B1C7)],
        ),
        (
            "lip511",
            &tnd.lip_arc[..],
            [(292..=310, 291, 0x0000_B4AB), (311..=317, 318, 0x0000_B1C6)],
        ),
    ] {
        for (run, src, colour) in folds {
            assert_eq!(t[src], colour, "TokyoNight Day {table} {src}");
            assert!(
                t[run.clone()].iter().all(|&c| c == colour),
                "TokyoNight Day {table} {run:?} takes {colour:06x}"
            );
            assert!(
                lch(colour).c >= HEADROOM_C && contrast(colour, tnd.bg) >= LIP_FORWARD_CR,
                "TokyoNight Day {table} {colour:06x}: C {:.4}, {:.4} against the page",
                lch(colour).c,
                contrast(colour, tnd.bg)
            );
        }
    }
}

/// A palette cache that has a pair runs no solve unit for it, nor for a shipped pair.
#[test]
fn a_steady_frame_advances_no_solve_unit() {
    let mut cache = PaletteCache::new();
    let _ = take_solves();
    for &(_, fg, bg) in &SHIPPED_PAGES {
        assert!(cache.frame(fg, bg).is_some());
    }
    assert_eq!(take_solves(), 0, "a shipped pair solves nothing");
    let dark = (0x00DC_DFE4, 0x0028_2C34);
    assert!(
        cache.frame(dark.0, dark.1).is_some(),
        "a dark pair classifies in one unit"
    );
    assert_eq!(take_solves(), 1);
    for _ in 0..16 {
        assert_eq!(
            cache.frame(dark.0, dark.1).map(|p| p.mode),
            Some(Mode::Paper)
        );
    }
    assert_eq!(take_solves(), 0, "a kept pair's frames solve nothing");
    assert!(cache.holds(dark.0, dark.1));
}

/// A fresh light pair is pending (main's paper arm draws) until its solve completes, each frame
/// runs at most [`SOLVE_UNITS_PER_FRAME`] units, and it completes within [`MAX_SOLVE_UNITS`] with
/// the palette [`solve`] gives.
#[test]
fn a_fresh_pair_completes_within_its_unit_count() {
    let (fg, bg) = (0x0033_3333, 0x00FA_FAF7);
    assert!(Palette::shipped(fg, bg).is_none());
    let mut cache = PaletteCache::new();
    let mut frames = 0;
    let mut units = 0;
    let _ = take_solves();
    let palette = loop {
        let got = cache.frame(fg, bg).copied();
        let ran = take_solves();
        assert!(ran <= SOLVE_UNITS_PER_FRAME, "{ran} units in one frame");
        units += ran;
        frames += 1;
        if let Some(p) = got {
            break p;
        }
        assert_eq!(cache.pending(), Some((fg, bg)));
        assert!(frames <= MAX_SOLVE_UNITS.div_ceil(SOLVE_UNITS_PER_FRAME));
    };
    assert!(units <= MAX_SOLVE_UNITS, "{units} units");
    assert_eq!(palette.mode, Mode::Glass);
    let mut again = PaletteSolve::new(fg, bg);
    assert_eq!(again.advance(usize::MAX), units);
    assert_eq!(again.done(), Some(&palette));
    assert!(again.lift_rounds() >= 1);
    assert!(cache.holds(fg, bg) && cache.pending().is_none());
}

/// The cache keeps the last [`PALETTE_CACHE_LEN`] pairs it was asked for, and drops the least
/// recently asked.
#[test]
fn the_palette_cache_holds_at_most_eight_pairs() {
    let mut cache = PaletteCache::new();
    let pair = |n: u32| (0x00E0_E0E0, n);
    for n in 0..PALETTE_CACHE_LEN as u32 {
        assert!(cache.frame(pair(n).0, pair(n).1).is_some());
    }
    assert_eq!(cache.len(), PALETTE_CACHE_LEN);
    assert!(
        cache.frame(pair(0).0, pair(0).1).is_some(),
        "touch the oldest"
    );
    assert!(cache.frame(pair(100).0, pair(100).1).is_some());
    assert_eq!(cache.len(), PALETTE_CACHE_LEN);
    assert!(cache.holds(pair(0).0, pair(0).1), "the touched pair stays");
    assert!(!cache.holds(pair(1).0, pair(1).1), "the least recent goes");
    for n in 200..220 {
        assert!(cache.frame(pair(n).0, pair(n).1).is_some());
        assert!(cache.len() <= PALETTE_CACHE_LEN);
    }
}

/// A solve interrupted for another pair restarts it, and the pair it was solving is not kept.
#[test]
fn a_request_for_another_pair_drops_the_solve_in_progress() {
    let mut cache = PaletteCache::new();
    let (fg, bg) = (0x0033_3333, 0x00FA_FAF7);
    assert!(cache.frame(fg, bg).is_none());
    assert_eq!(cache.pending(), Some((fg, bg)));
    assert!(cache.frame(0x00E0_E0E0, 0x0010_1010).is_some());
    assert_eq!(cache.pending(), None);
    assert!(!cache.holds(fg, bg));
}

/// §1 the olive fold and the AMBER HOLD on Default Light's walk lip: the fold sits at entry 39,
/// gold before and green after; the held entries keep h 58.5 and no lip on the amber arcs is
/// darker than the hold's line.
#[test]
fn default_lights_lip_folds_at_39_and_holds_its_amber() {
    let dl = tables::SHIPPED.first().expect("Default Light ships first");
    assert_eq!(FOLD_AT, 39);
    assert_eq!(FOLD_AT_ARC, 155);
    let (gold, green) = (lch(dl.lip[39]), lch(dl.lip[40]));
    assert!(gold.h < 90.0 && green.h > 125.0, "{gold:?} | {green:?}");
    let held: Vec<usize> = (0..GLASS_LEN)
        .filter(|&i| (lch(dl.lip[i]).h - AMBER_HOLD_HUE).abs() < 1.0)
        .collect();
    assert!(!held.is_empty(), "the hold holds some entries");
    for &i in &held {
        assert!(lch(dl.lip[i]).l < AMBER_HOLD_L + 0.01, "entry {i}");
    }
    for (i, &c) in dl.lip.iter().enumerate() {
        let p = lch(c);
        assert!(
            !(p.h >= BROAD_MUD_HUES.0 && p.h <= DEEP_HUES[2] + 1.0 && p.l < BROAD_MUD_L),
            "lip {i} {c:06x} is dark amber: {p:?}"
        );
        assert!(
            contrast(c, dl.bg) >= DEEP_PAGE_CR,
            "lip {i} {c:06x} vs page"
        );
        assert!(
            lch(dl.glass[i]).l - p.l >= DEEP_UNDER_GLASS_L - 1e-9,
            "lip {i} sits under its glass"
        );
    }
}

/// `all_lerps` reaches every colour k/16 does, and only colours between its ends.
#[test]
fn all_lerps_covers_every_sixteenth() {
    let (a, b) = (0x00EC_B000, 0x0080_D700);
    let all = all_lerps(a, b);
    for k in 0..=16u8 {
        let c = crate::effect_util::lerp_rgb(a, b, f32::from(k) / 16.0);
        assert!(all.binary_search(&c).is_ok(), "k {k}: {c:06x}");
    }
    for &c in &all {
        for sh in [16u32, 8, 0] {
            let (x, lo, hi) = ((c >> sh) & 0xff, (a >> sh) & 0xff, (b >> sh) & 0xff);
            assert!(x >= lo.min(hi) && x <= lo.max(hi));
        }
    }
}
