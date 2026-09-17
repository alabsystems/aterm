// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CYAN ROUND'S CENSUS — one instrument, BOTH tables, four questions.
//!
//! BEFORE is the shipped v0.86.0 table and its `CROSS_PACE` walk, transcribed
//! below from `HEAD` so that both arcs are measured by the same code and the
//! same two inks. AFTER is whatever this build links.
//!
//! 1. per-entry S and V across each leg — does the pale zone still exist?
//! 2. per-leg step for BOTH inks (the composited bed under the letters on
//!    Nord, and the vivid rail below the row): median, p90 and worst CIE-Lab
//!    step a cell is asked to carry, over 1000 walk phases x 16 cells;
//! 3. anchor residency — the share of phases with a cell within ten entries
//!    of each named anchor;
//! 4. yellow's share of a pass — cells of 16 whose rail reads yellow.
use aterm_effects::rainbow_kitty::meteor::tri;
use aterm_effects::rainbow_kitty::ribbon::{
    UNDER_COV_CAP, bed_ink, bed_luma_budget, rail_ink, walk_arc, walk_t,
};
use aterm_effects::spectrum::{
    SPECTRUM_ANCHOR_AT, SPECTRUM_LUT, SPECTRUM_LUT_LEN, spectrum, spectrum_hsv,
};
use aterm_render::{over_premul, premul_rgb};

const NORD_FG: u32 = 0x00D8_DEE9;
const NORD_BG: u32 = 0x002E_3440;
const PHASES: usize = 1000;
const WINDOW_ENTRIES: f32 = 10.0;
const NAMES: [&str; 7] = [
    "red", "orange", "yellow", "green", "blue", "indigo", "violet",
];
const LEGS: [&str; 6] = [
    "red>orange",
    "orange>yellow",
    "yellow>green",
    "green>blue",
    "blue>indigo",
    "indigo>violet",
];

// ===========================================================================
// THE SHIPPED (v0.86.0) TABLE AND WALK, transcribed from HEAD.
// ===========================================================================
const OLD_LUT: [u32; 511] = [
    0x00FF_0000,
    0x00FF_0200,
    0x00FF_0500,
    0x00FF_0700,
    0x00FF_0A00,
    0x00FF_0C00,
    0x00FF_0F00,
    0x00FF_1100,
    0x00FF_1400,
    0x00FF_1600,
    0x00FF_1800,
    0x00FF_1B00,
    0x00FF_1D00,
    0x00FF_1F00,
    0x00FF_2200,
    0x00FF_2400,
    0x00FF_2600,
    0x00FF_2800,
    0x00FF_2A00,
    0x00FF_2D00,
    0x00FF_2F00,
    0x00FF_3100,
    0x00FF_3300,
    0x00FF_3500,
    0x00FF_3700,
    0x00FF_3900,
    0x00FF_3B00,
    0x00FF_3D00,
    0x00FF_3F00,
    0x00FF_4100,
    0x00FF_4300,
    0x00FF_4500,
    0x00FF_4700,
    0x00FF_4900,
    0x00FF_4B00,
    0x00FF_4D00,
    0x00FF_4F00,
    0x00FF_5100,
    0x00FF_5300,
    0x00FF_5400,
    0x00FF_5600,
    0x00FF_5800,
    0x00FF_5A00,
    0x00FF_5C00,
    0x00FF_5E00,
    0x00FF_5F00,
    0x00FF_6100,
    0x00FF_6300,
    0x00FF_6500,
    0x00FF_6700,
    0x00FF_6800,
    0x00FF_6A00,
    0x00FF_6C00,
    0x00FF_6E00,
    0x00FF_6F00,
    0x00FF_7100,
    0x00FF_7300,
    0x00FF_7500,
    0x00FF_7600,
    0x00FF_7800,
    0x00FF_7A00,
    0x00FF_7C00,
    0x00FF_7D00,
    0x00FF_7F00,
    0x00FF_8100,
    0x00FF_8200,
    0x00FF_8400,
    0x00FF_8600,
    0x00FF_8700,
    0x00FF_8900,
    0x00FF_8B00,
    0x00FF_8C00,
    0x00FF_8E00,
    0x00FF_9000,
    0x00FF_9100,
    0x00FF_9300,
    0x00FF_9500,
    0x00FF_9600,
    0x00FF_9800,
    0x00FF_9A00,
    0x00FF_9B00,
    0x00FF_9D00,
    0x00FF_9F00,
    0x00FF_A000,
    0x00FF_A200,
    0x00FF_A400,
    0x00FF_A500,
    0x00FF_A700,
    0x00FF_A800,
    0x00FF_AA00,
    0x00FF_AC00,
    0x00FF_AD00,
    0x00FF_AF00,
    0x00FF_B100,
    0x00FF_B200,
    0x00FF_B400,
    0x00FF_B500,
    0x00FF_B700,
    0x00FF_B900,
    0x00FF_BA00,
    0x00FF_BC00,
    0x00FF_BD00,
    0x00FF_BF00,
    0x00FF_C100,
    0x00FF_C200,
    0x00FF_C400,
    0x00FF_C500,
    0x00FF_C700,
    0x00FF_C900,
    0x00FF_CA00,
    0x00FF_CC00,
    0x00FF_CE00,
    0x00FF_CF00,
    0x00FF_D100,
    0x00FF_D200,
    0x00FF_D400,
    0x00FF_D600,
    0x00FF_D700,
    0x00FF_D900,
    0x00FF_DA00,
    0x00FF_DC00,
    0x00FF_DD00,
    0x00FF_DF00,
    0x00FF_E100,
    0x00FF_E200,
    0x00FF_E400,
    0x00FF_E500,
    0x00FF_E700,
    0x00FF_E900,
    0x00FF_EA00,
    0x00FF_EC00,
    0x00FF_ED00,
    0x00FF_EF00,
    0x00FF_F100,
    0x00FF_F200,
    0x00FF_F400,
    0x00FF_F500,
    0x00FF_F700,
    0x00FF_F900,
    0x00FF_FA00,
    0x00FF_FC00,
    0x00FF_FD00,
    0x00FF_FF00,
    0x00FD_FF00,
    0x00FB_FF00,
    0x00F9_FF00,
    0x00F7_FF00,
    0x00F5_FF00,
    0x00F3_FF00,
    0x00F1_FF00,
    0x00EF_FF00,
    0x00ED_FF00,
    0x00EC_FF00,
    0x00EA_FF00,
    0x00E8_FF00,
    0x00E6_FF00,
    0x00E4_FF00,
    0x00E2_FF00,
    0x00E0_FF00,
    0x00DE_FF00,
    0x00DC_FF00,
    0x00DA_FF00,
    0x00D8_FF00,
    0x00D6_FF00,
    0x00D4_FF00,
    0x00D2_FF00,
    0x00D0_FF00,
    0x00CE_FF00,
    0x00CC_FF00,
    0x00CA_FF00,
    0x00C8_FF00,
    0x00C6_FF00,
    0x00C4_FF00,
    0x00C2_FF00,
    0x00C0_FF00,
    0x00BE_FF00,
    0x00BC_FF00,
    0x00BA_FF00,
    0x00B8_FF00,
    0x00B6_FF00,
    0x00B4_FF00,
    0x00B1_FF00,
    0x00AF_FF00,
    0x00AD_FF00,
    0x00AB_FF00,
    0x00A9_FF00,
    0x00A7_FF00,
    0x00A5_FF00,
    0x00A3_FF00,
    0x00A1_FF00,
    0x009F_FF00,
    0x009D_FF00,
    0x009B_FF00,
    0x0099_FF00,
    0x0096_FF00,
    0x0094_FF00,
    0x0092_FF00,
    0x0090_FF00,
    0x008E_FF00,
    0x008C_FF00,
    0x008A_FF00,
    0x0087_FF00,
    0x0085_FF00,
    0x0083_FF00,
    0x0081_FF00,
    0x007F_FF00,
    0x007D_FF00,
    0x007A_FF00,
    0x0078_FF00,
    0x0076_FF00,
    0x0074_FF00,
    0x0072_FF00,
    0x006F_FF00,
    0x006D_FF00,
    0x006B_FF00,
    0x0069_FF00,
    0x0066_FF00,
    0x0064_FF00,
    0x0062_FF00,
    0x0060_FF00,
    0x005D_FF00,
    0x005B_FF00,
    0x0059_FF00,
    0x0056_FF00,
    0x0054_FF00,
    0x0052_FF00,
    0x004F_FF00,
    0x004D_FF00,
    0x004B_FF00,
    0x0048_FF00,
    0x0046_FF00,
    0x0044_FF00,
    0x0041_FF00,
    0x003F_FF00,
    0x003C_FF00,
    0x003A_FF00,
    0x0038_FF00,
    0x0035_FF00,
    0x0033_FF00,
    0x0030_FF00,
    0x002E_FF00,
    0x002B_FF00,
    0x0029_FF00,
    0x0026_FF00,
    0x0024_FF00,
    0x0021_FF00,
    0x001F_FF00,
    0x001C_FF00,
    0x001A_FF00,
    0x0017_FF00,
    0x0015_FF00,
    0x0012_FF00,
    0x0010_FF00,
    0x000D_FF00,
    0x000A_FF00,
    0x0008_FF00,
    0x0005_FF00,
    0x0003_FF00,
    0x0000_FF00,
    0x0000_FF03,
    0x0001_FF06,
    0x0001_FF08,
    0x0002_FF0B,
    0x0003_FF0E,
    0x0003_FF11,
    0x0004_FF14,
    0x0004_FF16,
    0x0005_FF19,
    0x0005_FF1C,
    0x0006_FF1F,
    0x0006_FF21,
    0x0007_FF24,
    0x0008_FF27,
    0x0008_FF29,
    0x0009_FF2C,
    0x0009_FF2E,
    0x000A_FF31,
    0x000B_FF34,
    0x000B_FF36,
    0x000C_FF39,
    0x000C_FF3B,
    0x000D_FF3E,
    0x000E_FF40,
    0x000E_FF43,
    0x000F_FF45,
    0x0010_FF47,
    0x0010_FF4A,
    0x0011_FF4C,
    0x0012_FF4F,
    0x0012_FF51,
    0x0013_FF53,
    0x0014_FF56,
    0x0014_FF58,
    0x0015_FF5A,
    0x0016_FF5D,
    0x0017_FF5F,
    0x0017_FF61,
    0x0018_FF64,
    0x0019_FF66,
    0x001C_FF6D,
    0x0020_FF75,
    0x0025_FF7E,
    0x002B_FF87,
    0x0031_FF91,
    0x0038_FF9A,
    0x0040_FFA3,
    0x0047_FFAB,
    0x004D_FFB2,
    0x0054_FFB8,
    0x0059_FFBD,
    0x005E_FEC0,
    0x0063_FBC1,
    0x0067_F7C1,
    0x006A_F3C1,
    0x006D_EFC0,
    0x006E_ECC0,
    0x006F_EBC2,
    0x006F_EBCE,
    0x006F_EBD9,
    0x006F_EBE9,
    0x006F_DBEB,
    0x006F_CBEB,
    0x006F_BDEB,
    0x006F_B1EB,
    0x006C_B0ED,
    0x0065_AFF2,
    0x005B_ADF8,
    0x0050_AAFD,
    0x0047_A6FF,
    0x0040_A1FF,
    0x0038_9BFF,
    0x0031_95FF,
    0x002B_8EFF,
    0x0025_87FF,
    0x0020_80FF,
    0x001C_7AFF,
    0x0019_75FF,
    0x0018_73FF,
    0x0018_72FF,
    0x0017_70FF,
    0x0016_6EFF,
    0x0016_6CFF,
    0x0015_6BFF,
    0x0015_69FF,
    0x0014_67FF,
    0x0013_66FF,
    0x0013_64FF,
    0x0012_62FF,
    0x0012_60FF,
    0x0011_5FFF,
    0x0011_5DFF,
    0x0010_5BFF,
    0x0010_59FF,
    0x000F_57FF,
    0x000F_56FF,
    0x000F_54FF,
    0x000E_52FF,
    0x000E_50FF,
    0x000D_4EFF,
    0x000D_4CFF,
    0x000C_4BFF,
    0x000C_49FF,
    0x000C_47FF,
    0x000B_45FF,
    0x000B_43FF,
    0x000A_41FF,
    0x000A_3FFF,
    0x000A_3DFF,
    0x0009_3BFF,
    0x0009_39FF,
    0x0008_37FF,
    0x0008_35FF,
    0x0008_33FF,
    0x0007_31FF,
    0x0007_2FFF,
    0x0007_2DFF,
    0x0006_2BFF,
    0x0006_29FF,
    0x0005_27FF,
    0x0005_24FF,
    0x0005_22FF,
    0x0004_20FF,
    0x0004_1EFF,
    0x0004_1BFF,
    0x0003_19FF,
    0x0003_17FF,
    0x0003_14FF,
    0x0002_12FF,
    0x0002_0FFF,
    0x0002_0DFF,
    0x0001_0AFF,
    0x0001_08FF,
    0x0001_05FF,
    0x0000_03FF,
    0x0000_00FF,
    0x0001_00FD,
    0x0003_00FB,
    0x0004_00F8,
    0x0005_00F6,
    0x0007_00F4,
    0x0008_00F2,
    0x0009_00F0,
    0x000A_00EE,
    0x000C_00EC,
    0x000D_00EA,
    0x000E_00E7,
    0x000F_00E5,
    0x0011_00E3,
    0x0012_00E1,
    0x0013_00DF,
    0x0014_00DD,
    0x0015_00DB,
    0x0017_00D9,
    0x0018_00D8,
    0x0019_00D6,
    0x001A_00D4,
    0x001B_00D2,
    0x001C_00D0,
    0x001D_00CE,
    0x001E_00CC,
    0x0020_00CA,
    0x0021_00C9,
    0x0022_00C7,
    0x0023_00C5,
    0x0024_00C3,
    0x0025_00C2,
    0x0026_00C0,
    0x0027_00BE,
    0x0028_00BC,
    0x0029_00BB,
    0x002A_00B9,
    0x002B_00B7,
    0x002C_00B6,
    0x002D_00B4,
    0x002E_00B3,
    0x002F_00B1,
    0x0030_00B0,
    0x0031_00AE,
    0x0031_00AD,
    0x0032_00AB,
    0x0033_00AA,
    0x0034_00A8,
    0x0035_00A7,
    0x0036_00A5,
    0x0037_00A4,
    0x0038_00A2,
    0x0038_00A1,
    0x0039_00A0,
    0x003A_009E,
    0x003B_009D,
    0x003C_009B,
    0x003D_009A,
    0x003D_0099,
    0x003E_0098,
    0x003F_0096,
    0x0040_0095,
    0x0040_0094,
    0x0041_0092,
    0x0042_0091,
    0x0043_0090,
    0x0043_008F,
    0x0044_008D,
    0x0045_008C,
    0x0046_008B,
    0x0046_008A,
    0x0047_0089,
    0x0048_0088,
    0x0048_0086,
    0x0049_0085,
    0x004A_0084,
    0x004A_0083,
    0x004B_0082,
    0x004D_0084,
    0x004E_0086,
    0x0050_0088,
    0x0052_0089,
    0x0053_008B,
    0x0055_008D,
    0x0057_008F,
    0x0059_0091,
    0x005A_0093,
    0x005C_0095,
    0x005E_0097,
    0x0060_0099,
    0x0061_009B,
    0x0063_009D,
    0x0065_009F,
    0x0067_00A1,
    0x0069_00A3,
    0x006B_00A5,
    0x006C_00A7,
    0x006E_00A9,
    0x0070_00AB,
    0x0072_00AD,
    0x0074_00B0,
    0x0076_00B2,
    0x0078_00B4,
    0x007A_00B6,
    0x007C_00B8,
    0x007E_00BA,
    0x0080_00BD,
    0x0082_00BF,
    0x0084_00C1,
    0x0086_00C3,
    0x0088_00C5,
    0x008A_00C8,
    0x008C_00CA,
    0x008E_00CC,
    0x0090_00CE,
    0x0092_00D1,
    0x0094_00D3,
];

const OLD_CROSS_PACE: [u16; 65] = [
    36237, 37201, 37876, 38411, 38586, 38755, 38865, 39044, 39183, 39296, 39438, 39617, 39951,
    40256, 40543, 40654, 40730, 40809, 40869, 40923, 40977, 41022, 41066, 41100, 41141, 41173,
    41213, 41243, 41279, 41317, 41353, 41382, 41415, 41456, 41489, 41602, 41660, 41715, 41773,
    41821, 41909, 41988, 42079, 42187, 42231, 42308, 42395, 42434, 42488, 42567, 42629, 42692,
    42799, 42906, 43059, 43272, 43660, 44140, 44558, 45137, 45521, 45969, 46487, 46934, 47545,
];

const OLD_ANCHOR_AT: [usize; 7] = [0, 63, 142, 258, 394, 471, 510];
const OLD_CROSS_LUT0: usize = 258 + 24;
const OLD_CROSS_LUT1: usize = 394 - 24;

fn lerp(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |sh: u32| {
        let (x, y) = (((a >> sh) & 0xff) as f32, ((b >> sh) & 0xff) as f32);
        ((x + (y - x) * t).round().clamp(0.0, 255.0) as u32) << sh
    };
    ch(16) | ch(8) | ch(0)
}
fn old_spectrum(t: f32) -> u32 {
    let x = t.clamp(0.0, 1.0) * 510.0;
    let i = (x as usize).min(510);
    let j = (i + 1).min(510);
    lerp(OLD_LUT[i], OLD_LUT[j], x - i as f32)
}
fn old_walk_pace(x: f32) -> f32 {
    let a0 = OLD_CROSS_LUT0 as f32 / 510.0;
    let a1 = OLD_CROSS_LUT1 as f32 / 510.0;
    let x = x.clamp(0.0, 1.0);
    if x <= a0 || x >= a1 {
        return x;
    }
    let f = (x - a0) / (a1 - a0) * 64.0;
    let i = (f as usize).min(63);
    let (a, b) = (
        f32::from(OLD_CROSS_PACE[i]),
        f32::from(OLD_CROSS_PACE[i + 1]),
    );
    (a + (b - a) * (f - i as f32)) / 65535.0
}

fn s2l(v: f32) -> f32 {
    let v = v / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
fn lab(c: u32) -> (f32, f32, f32) {
    let ch = |sh: u32| ((c >> sh) & 0xff) as f32;
    let (r, g, b) = (s2l(ch(16)), s2l(ch(8)), s2l(ch(0)));
    let (x, y, z) = (
        0.4124 * r + 0.3576 * g + 0.1805 * b,
        0.2126 * r + 0.7152 * g + 0.0722 * b,
        0.0193 * r + 0.1192 * g + 0.9505 * b,
    );
    let f = |t: f32| {
        if t > 0.008_856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let (fx, fy, fz) = (f(x / 0.95047), f(y), f(z / 1.08883));
    (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
}
fn de(a: u32, b: u32) -> f32 {
    let (l1, a1, b1) = lab(a);
    let (l2, a2, b2) = lab(b);
    ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
}
fn glass(ink: u32) -> u32 {
    let a = u8::try_from(UNDER_COV_CAP.clamp(0.0, 255.0) as u16).unwrap_or(255);
    over_premul(NORD_BG, premul_rgb(ink, a), a)
}
fn pick(v: &mut [f32], q: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    v[((v.len() - 1) as f32 * q) as usize]
}

struct Arc {
    name: &'static str,
    lut: [u32; 511],
    anchor_at: [usize; 7],
    read: fn(f32) -> u32,
    walk: fn(f32) -> f32,
}

fn leg_of(anchor_at: &[usize; 7], a: f32) -> usize {
    let x = (a.clamp(0.0, 1.0) * 510.0) as usize;
    for i in 0..6 {
        if x >= anchor_at[i] && x <= anchor_at[i + 1] {
            return i;
        }
    }
    5
}

fn census(arc: &Arc) {
    let budget = bed_luma_budget(NORD_FG);
    println!("\n################  {}  ################", arc.name);

    // ---- 1. the table's own chroma and value -----------------------------
    println!("TABLE: leg / entries / share / min S / min V / min chroma");
    for (i, leg) in LEGS.iter().enumerate() {
        let (a, b) = (arc.anchor_at[i], arc.anchor_at[i + 1]);
        let (mut ms, mut mv, mut mc) = (1.0f64, 1.0f64, 255i64);
        for &c in &arc.lut[a..=b] {
            let (_, s, v) = spectrum_hsv(c);
            ms = ms.min(s);
            mv = mv.min(v);
            let (r, g, bl) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
            mc = mc.min((r.max(g).max(bl) - r.min(g).min(bl)) as i64);
        }
        println!(
            "  {leg:<14} {:>4}  {:5.1}%   {ms:.3}   {mv:.3}   {mc:>3}",
            b - a,
            100.0 * (b - a) as f32 / 510.0
        );
    }
    let hue_share = |lo: f64, hi: f64| {
        100.0
            * arc
                .lut
                .iter()
                .filter(|&&c| {
                    let h = spectrum_hsv(c).0;
                    h >= lo && h <= hi
                })
                .count() as f32
            / 511.0
    };
    println!(
        "  hue shares: warm 0..120 {:.1}%, yellow 45..75 {:.1}%, cyan window 165..200 {:.1}%",
        hue_share(0.0, 120.0),
        hue_share(45.0, 75.0),
        hue_share(165.0, 200.0)
    );
    // THE NEAREST-ANCHOR REGIONS, in midpoint arithmetic over the 510 steps —
    // the share of the arc that is NEAREST each name, which is the unit the
    // owner's yellow ruling is argued in.
    let mid = |i: usize| 0.5 * (arc.anchor_at[i] + arc.anchor_at[i + 1]) as f32;
    println!(
        "  REGIONS (nearest anchor, of 510): yellow {:.1} entries {:.1}%, R+O+Y {:.1} entries {:.1}%, green+blue {:.1} entries {:.1}%",
        mid(2) - mid(1),
        100.0 * (mid(2) - mid(1)) / 510.0,
        mid(2),
        100.0 * mid(2) / 510.0,
        mid(4) - mid(2),
        100.0 * (mid(4) - mid(2)) / 510.0
    );
    let (mut gs, mut gv) = (1.0f64, 1.0f64);
    for &c in &arc.lut[arc.anchor_at[3]..=arc.anchor_at[4]] {
        let (_, s, v) = spectrum_hsv(c);
        gs = gs.min(s);
        gv = gv.min(v);
    }
    println!("  GREEN->BLUE minimum: S {gs:.4}, V {gv:.4}");
    let worst_chord = (0..510)
        .map(|i| {
            [16u32, 8, 0]
                .into_iter()
                .map(|sh| {
                    (((arc.lut[i] >> sh) & 0xff) as i32 - ((arc.lut[i + 1] >> sh) & 0xff) as i32)
                        .abs()
                })
                .max()
                .unwrap_or(0)
        })
        .max()
        .unwrap_or(0);
    println!("  worst adjacent byte chord in the table: {worst_chord}");

    // ---- 2/3/4. the walk --------------------------------------------------
    for (wname, paced) in [("identity walk", false), ("paced walk", true)] {
        let bed = |a: f32| glass(bed_ink((arc.read)(a), budget));
        let rail = |a: f32| rail_ink((arc.read)(a));
        let anchors: Vec<f32> = arc.anchor_at.iter().map(|&x| x as f32 / 510.0).collect();
        let win = WINDOW_ENTRIES / 510.0;
        let mut res = [0usize; 7];
        let mut bedv: Vec<Vec<f32>> = vec![Vec::new(); 6];
        let mut railv: Vec<Vec<f32>> = vec![Vec::new(); 6];
        let mut yellow = 0usize;
        let mut hlone = 0usize;
        let mut ylone = 0usize;
        // THE WALK'S FIRST FULL PASS: seventeen cells at phase zero, and which
        // of them the eye would name yellow (the nearest of the seven anchors).
        let name_of = |a: f32| {
            let x = a.clamp(0.0, 1.0) * 510.0;
            let mut best = 0usize;
            for k in 1..7 {
                if (x - arc.anchor_at[k] as f32).abs() < (x - arc.anchor_at[best] as f32).abs() {
                    best = k;
                }
            }
            best
        };
        let first_pass = (0..=16)
            .filter(|&d| {
                let t = tri(walk_t(f32::from(d as u8))).clamp(0.0, 1.0);
                name_of(if paced { (arc.walk)(t) } else { t }) == 2
            })
            .count();
        for p in 0..PHASES {
            let t0 = p as f32 / PHASES as f32 / 16.0;
            let a: Vec<f32> = (0..=16)
                .map(|d| {
                    let t = tri(t0 + walk_t(d as f32)).clamp(0.0, 1.0);
                    if paced { (arc.walk)(t) } else { t }
                })
                .collect();
            for (i, an) in anchors.iter().enumerate() {
                if a.iter().any(|x| (x - an).abs() <= win) {
                    res[i] += 1;
                }
            }
            for d in 0..16 {
                let l = leg_of(&arc.anchor_at, 0.5 * (a[d] + a[d + 1]));
                bedv[l].push(de(bed(a[d]), bed(a[d + 1])));
                railv[l].push(de(rail(a[d]), rail(a[d + 1])));
            }
            let (mut hc, mut yc) = (0usize, 0usize);
            for x in a.iter().take(16) {
                let h = spectrum_hsv(rail(*x)).0;
                if (45.0..=75.0).contains(&h) {
                    yellow += 1;
                    hc += 1;
                }
                if name_of(*x) == 2 {
                    yc += 1;
                }
            }
            if hc <= 1 {
                hlone += 1;
            }
            if yc <= 1 {
                ylone += 1;
            }
        }
        println!("\n  -- {wname} --");
        println!(
            "  leg                 n    bed med   bed p90   bed MAX    rail med  rail p90  rail MAX"
        );
        for i in 0..6 {
            let n = bedv[i].len();
            println!(
                "  {:<14} {n:>7}   {:7.2}   {:7.2}   {:7.2}    {:7.2}   {:7.2}   {:7.2}",
                LEGS[i],
                pick(&mut bedv[i], 0.5),
                pick(&mut bedv[i], 0.9),
                pick(&mut bedv[i], 1.0),
                pick(&mut railv[i], 0.5),
                pick(&mut railv[i], 0.9),
                pick(&mut railv[i], 1.0)
            );
        }
        let mut allb: Vec<f32> = bedv.iter().flatten().copied().collect();
        let mut allr: Vec<f32> = railv.iter().flatten().copied().collect();
        let (bm, bx) = (pick(&mut allb, 0.5), pick(&mut allb, 1.0));
        let (rm, rx) = (pick(&mut allr, 0.5), pick(&mut allr, 1.0));
        println!(
            "  WHOLE ARC: bed med {bm:.2} max {bx:.2} (max/med {:.2}x), rail med {rm:.2} max {rx:.2} (max/med {:.2}x)",
            bx / bm,
            rx / rm
        );
        print!("  anchor residency:");
        for (i, nm) in NAMES.iter().enumerate() {
            print!(" {nm} {:.1}%", 100.0 * res[i] as f32 / PHASES as f32);
        }
        println!();
        println!(
            "  YELLOW: {first_pass} cells of 17 on the first full pass; {:.3} of 16 per pass ({:.1}% of cells); passes showing <=1 yellow slab: {:.2}% by rail hue, {:.2}% by nearest anchor",
            yellow as f32 / PHASES as f32,
            100.0 * yellow as f32 / (PHASES * 16) as f32,
            100.0 * hlone as f32 / PHASES as f32,
            100.0 * ylone as f32 / PHASES as f32
        );
    }
}

fn main() {
    let mut new_lut = [0u32; 511];
    new_lut.copy_from_slice(&SPECTRUM_LUT[..SPECTRUM_LUT_LEN]);
    let mut new_at = [0usize; 7];
    new_at.copy_from_slice(&SPECTRUM_ANCHOR_AT);
    census(&Arc {
        name: "BEFORE - shipped v0.86.0 arc",
        lut: OLD_LUT,
        anchor_at: OLD_ANCHOR_AT,
        read: old_spectrum,
        walk: old_walk_pace,
    });
    census(&Arc {
        name: "AFTER - this build's arc",
        lut: new_lut,
        anchor_at: new_at,
        read: spectrum,
        walk: |t| walk_arc(t.min(0.999_999)),
    });
}
