// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `vte-marks` floor lane: text whose code points join the glyph before them, replayed into
//! the engine (`Terminal::process`) at `vte-engine`'s grid by `vte-engine`'s harness and judged
//! as that lane is ([`super::vte_engine::judge`]): per corpus, the best sample's MiB per 10^9
//! performance-core cycles, at the load it finds.
//!
//! It is the write path's mark branch's lane. A zero-width code point takes the zero-width arm
//! of `write_char` (a mark alone between ASCII letters reaches the engine as one `print`) or of
//! `write_unicode_bulk` (a run of them), then `preceding_cell` finds the glyph it continues; a
//! VS16 widens that glyph, a skin tone or a flag's second half joins it, and the code point after
//! a ZWJ is folded into it. `vte-engine`'s d1 unicode corpus holds no mark at all. Four corpora,
//! one per way a code point joins:
//!
//! - `marks_latin`: decomposed (NFD) Vietnamese, Czech, German and French, one or two
//!   combining diacritics on a letter, as decomposed text prints them;
//! - `marks_brahmic`: Hindi and Thai, whose non-spacing vowel signs, viramas and tone marks
//!   are zero width; Hindi's spacing vowel signs (U+093E-U+0940 here) are marks too, but
//!   aterm's width table gives each a cell of its own, which the bulk lane's narrow arm
//!   writes;
//! - `marks_hangul`: Korean spelled in conjoining jamo, a wide leading consonant and its
//!   zero-width vowel and trailing consonant;
//! - `marks_emoji`: VS16 on text-default emoji, skin tones, ZWJ sequences and flags among
//!   ASCII words, as a chat or a commit log has them.
//!
//! The corpora are written once per content into the arena cache (a folder named by their
//! digest), whose path the lane prints for a hand A/B of two builds' harnesses.

use std::io::Write as _;
use std::path::Path;

use super::LaneRun;
use super::arena::cat::generate;
use super::arena::{Halt, Scratch, arena_dir};
use super::floors::{FloorLane, measure_cycle_example_with};
use super::vte_engine::{COLS, ROWS, share};

/// Each corpus's size: whole units to at least this many MiB.
const CORPUS_MB: u64 = 16;

/// The lane: its gated keys are each corpus's best MiB per 10^9 performance-core cycles.
pub(crate) const LANE: FloorLane = FloorLane {
    lane: "vte-marks",
    example: "vte_engine_harness",
    keys: &[
        "marks_latin_best_mib_per_gcycle",
        "marks_brahmic_best_mib_per_gcycle",
        "marks_hangul_best_mib_per_gcycle",
        "marks_emoji_best_mib_per_gcycle",
    ],
    baseline_file: "perf-baseline-vte-marks.json",
    comment: "aterm VTE-MARKS baseline: per corpus, the best sample's MiB per 10^9 \
              performance-core cycles (cycles per instruction on P-cores times all \
              instructions, from the kernel's per-core-type counters) of four 16 MiB \
              mark-heavy corpora (NFD Latin diacritics; Hindi and Thai signs; Korean in \
              conjoining jamo; emoji with VS16, skin tones, ZWJ sequences and flags), each \
              replayed into a fresh live session's Terminal::process at 120x40 in 16 KiB \
              slices by vte_engine_harness. BIGGER-IS-BETTER; the lane runs unadmitted at the \
              load it finds, so cycles are what is judged. Re-record with targo --unverified \
              verify perf --record vte-marks.",
    cli: "vte-marks",
};

/// The corpora: each one's name and the lines it repeats.
fn corpora() -> [(&'static str, String); 4] {
    [
        (
            "marks_latin",
            "Tie\u{302}\u{301}ng Vie\u{323}\u{302}t co\u{301} sa\u{301}u thanh \
             \u{111}ie\u{323}\u{302}u: ngang, huye\u{302}\u{300}n, sa\u{306}\u{301}c, ho\u{309}i, \
             nga\u{303}, na\u{323}ng\r\n\
             Z\u{30c}lut\u{30c}ouc\u{30c}ky\u{301} ku\u{30a}n\u{30c} u\u{301}pe\u{30c}l \
             d\u{30c}a\u{301}belske\u{301} o\u{301}dy, pr\u{30c}i\u{301}lis\u{30c} \
             z\u{30c}lut\u{30c}ouc\u{30c}ky\u{301}\r\n\
             Gru\u{308}\u{df}e aus Ko\u{308}ln, Mu\u{308}nchen und Zu\u{308}rich: fu\u{308}nf \
             Bu\u{308}cher, a\u{308}hnlich scho\u{308}n\r\n\
             Un re\u{301}sume\u{301} de\u{301}ja\u{300} e\u{301}crit, c\u{327}a va? \
             L'e\u{301}te\u{301} a\u{300} la plage, ou\u{300} l'on mange\u{301} des cre\u{302}pes\r\n"
                .to_string(),
        ),
        (
            "marks_brahmic",
            "नमस्ते दुनिया, यह हिन्दी में एक परीक्षण वाक्य है और इसमें मात्राएँ हैं\r\n\
             สวัสดีชาวโลก นี่คือประโยคทดสอบภาษาไทยที่มีวรรณยุกต์และสระ\r\n"
                .to_string(),
        ),
        (
            "marks_hangul",
            conjoining(
                "빠른 갈색 여우가 게으른 개를 뛰어넘는다\r\n\
                 다람쥐 헌 쳇바퀴에 타고파, 키스의 고유조건은 입술끼리 만나야 하고\r\n",
            ),
        ),
        (
            "marks_emoji",
            "ship it \u{1F44D}\u{1F3FD} tests \u{2705} love \u{2764}\u{FE0F} sun \
             \u{2600}\u{FE0F} peace \u{270C}\u{FE0F}\u{1F3FE} dev \u{1F468}\u{200D}\u{1F4BB} \
             lab \u{1F469}\u{1F3FB}\u{200D}\u{1F52C}\r\n\
             family \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466} flags \
             \u{1F1FA}\u{1F1F8}\u{1F1EF}\u{1F1F5}\u{1F1F0}\u{1F1F7} wave \u{1F44B}\u{1F3FF} \
             phone \u{260E}\u{FE0F} check \u{2714}\u{FE0F} cloud \u{2601}\u{FE0F}\r\n"
                .to_string(),
        ),
    ]
}

/// `text` with every precomposed Hangul syllable (U+AC00-U+D7A3) spelled in the conjoining jamo
/// it stands for (Unicode's arithmetic decomposition, §3.12): a leading consonant, a vowel and
/// a trailing consonant where it has one, as an NFD writer sends Korean.
fn conjoining(text: &str) -> String {
    const S_BASE: u32 = 0xAC00;
    const L_BASE: u32 = 0x1100;
    const V_BASE: u32 = 0x1161;
    const T_BASE: u32 = 0x11A7;
    const T_COUNT: u32 = 28;
    const N_COUNT: u32 = 21 * T_COUNT;
    const S_COUNT: u32 = 19 * N_COUNT;
    let mut out = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        let s = u32::from(c).wrapping_sub(S_BASE);
        if s >= S_COUNT {
            out.push(c);
            continue;
        }
        let t = T_BASE + s % T_COUNT;
        let jamo = [L_BASE + s / N_COUNT, V_BASE + (s % N_COUNT) / T_COUNT];
        out.extend(
            jamo.into_iter()
                .chain((t != T_BASE).then_some(t))
                .filter_map(char::from_u32),
        );
    }
    out
}

/// The SHA-256 prefix of the corpora, each name and line, and their size: the cache folder's
/// name, so a folder found there holds these bytes.
fn digest(corpora: &[(&str, String)]) -> String {
    let mut all = CORPUS_MB.to_le_bytes().to_vec();
    for (name, unit) in corpora {
        for part in [name.as_bytes(), unit.as_bytes()] {
            all.extend_from_slice(&(part.len() as u64).to_le_bytes());
            all.extend_from_slice(part);
        }
    }
    aterm_digest::Sha256::digest(all)
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Write each corpus to `<dir>/<name>.vt`.
fn write_corpora(dir: &Path, corpora: &[(&str, String)]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (name, unit) in corpora {
        let path = dir.join(format!("{name}.vt"));
        let fail = |e: std::io::Error| format!("{}: {e}", path.display());
        let mut f = std::io::BufWriter::new(std::fs::File::create(&path).map_err(fail)?);
        generate(unit.as_bytes(), CORPUS_MB, &mut f).map_err(fail)?;
        f.flush().map_err(fail)?;
    }
    Ok(())
}

/// The lane: write the corpora (once per content), measure, judge (or record).
pub(crate) fn run(record: bool) -> LaneRun {
    super::vte_engine::judge(&LANE, measure(), record)
}

fn measure() -> Result<aterm_json::Value, Halt> {
    let corpora = corpora();
    let shared = arena_dir()?.join(format!("vte-marks-{CORPUS_MB}-{}", digest(&corpora)));
    // A run that finds no corpora at `shared` writes them in its scratch, which outlives the
    // measurement, and leaves them there.
    let (dir, _scratch) = if shared.is_dir() {
        (shared, None)
    } else {
        let scratch = Scratch::new("vte-marks")?;
        let part = scratch.path().join("corpora");
        write_corpora(&part, &corpora)?;
        match share(&part, &shared) {
            Ok(()) => (shared, Some(scratch)),
            Err(e) => {
                eprintln!("  corpora not left for a hand A/B: {e}");
                (part, Some(scratch))
            }
        }
    };
    eprintln!("  corpora ({CORPUS_MB} MiB each): {}", dir.display());
    let corpus = |name: &str| format!("{name}={}", dir.join(format!("{name}.vt")).display());
    // The harness's payload folder is the corpora's own: it holds no `.bench`, so the harness
    // runs no vtebench scenario, only these.
    let mut args = vec![dir.display().to_string()];
    for (name, _) in &corpora {
        args.extend(["--corpus".to_string(), corpus(name)]);
    }
    args.extend([
        "--rows".to_string(),
        ROWS.to_string(),
        "--cols".to_string(),
        COLS.to_string(),
    ]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    measure_cycle_example_with(LANE.example, &[], &args).map_err(Halt::Fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How many of `text`'s code points join the glyph before them, in the ways the corpora
    /// exercise: one aterm's width table gives no cell (a non-spacing mark, a conjoining vowel
    /// or trailing jamo, ZWJ, VS16), a skin tone, a flag's second regional indicator, or the
    /// emoji after a ZWJ. Devanagari's spacing marks (U+0903, U+093B, U+093E-U+0940,
    /// U+0949-U+094C, U+094E, U+094F) are marks too, but that table gives each a cell of its
    /// own, so they are not counted.
    fn joining(text: &str) -> usize {
        let (mut count, mut after_zwj, mut open_flag) = (0, false, false);
        for c in text.chars() {
            let cp = u32::from(c);
            let indicator = (0x1F1E6..=0x1F1FF).contains(&cp);
            let joins = after_zwj
                || (indicator && open_flag)
                || matches!(
                    cp,
                    0x300..=0x36F
                        | 0x900..=0x902
                        | 0x93A
                        | 0x93C
                        | 0x941..=0x948
                        | 0x94D
                        | 0xE31
                        | 0xE34..=0xE3A
                        | 0xE47..=0xE4E
                        | 0x1160..=0x11FF
                        | 0x200D
                        | 0xFE0F
                        | 0x1F3FB..=0x1F3FF
                );
            count += usize::from(joins);
            open_flag = indicator && !open_flag;
            after_zwj = cp == 0x200D;
        }
        count
    }

    #[test]
    fn only_what_takes_no_cell_of_its_own_is_counted_as_joining() {
        // कि: a spacing vowel sign; कु: a non-spacing one; क्: a virama.
        assert_eq!(joining("\u{915}\u{93F} \u{915}\u{941} \u{915}\u{94D}"), 2);
        // Two flags, four regional indicators: each flag's second joins its first.
        assert_eq!(joining("\u{1F1FA}\u{1F1F8}\u{1F1EF}\u{1F1F5}"), 2);
        // A ZWJ sequence: the joiner and the emoji after it.
        assert_eq!(joining("\u{1F468}\u{200D}\u{1F4BB}"), 2);
    }

    #[test]
    fn every_corpus_is_mark_heavy() {
        let corpora = corpora();
        let keys: Vec<String> = corpora
            .iter()
            .map(|(name, _)| format!("{name}_best_mib_per_gcycle"))
            .collect();
        assert_eq!(keys, LANE.keys, "one gated key per corpus, in order");
        for (name, unit) in &corpora {
            let joining = joining(unit);
            let printing = unit.chars().filter(|c| !c.is_ascii_whitespace()).count();
            assert!(
                joining * 6 >= printing,
                "{name}: {joining} joining of {printing} printing code points"
            );
        }
    }

    #[test]
    fn a_syllable_is_spelled_in_its_conjoining_jamo() {
        assert_eq!(conjoining("한"), "\u{1112}\u{1161}\u{11AB}");
        assert_eq!(conjoining("가"), "\u{1100}\u{1161}");
        assert_eq!(conjoining("힣"), "\u{1112}\u{1175}\u{11C2}");
        assert_eq!(conjoining("a 가\r\n"), "a \u{1100}\u{1161}\r\n");
    }

    #[test]
    fn the_corpora_are_written_whole_and_named_by_their_bytes() {
        let corpora = corpora();
        let dir = Scratch::new("atv-vte-marks").expect("scratch");
        write_corpora(dir.path(), &corpora).expect("writes");
        for (name, unit) in &corpora {
            let bytes = std::fs::read(dir.path().join(format!("{name}.vt"))).expect("reads");
            let target = CORPUS_MB << 20;
            assert!(
                bytes.len() as u64 >= target && bytes.len().is_multiple_of(unit.len()),
                "{name}"
            );
            assert!(bytes.starts_with(unit.as_bytes()), "{name}");
        }
        let mut other = corpora.clone();
        other[0].1.push('x');
        assert_ne!(digest(&corpora), digest(&other));
    }
}
