// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `trail-companion` tests: the status fields, the overlap judge, and a gallery tree read end to end.

use super::*;

use std::sync::atomic::{AtomicU32, Ordering};

/// The gallery's grid: 14x26 cells under a 12 px pad and a 2 px top pad.
const GRID: Grid = Grid {
    ox: 12,
    oy: 2,
    cw: 14,
    ch: 26,
    rows: 14,
    cols: 72,
    source: "dims",
};

const DIMS: &str = "OK 14 72 1008 364 session=0 cell_w=14 cell_h=26 pad=12 pad_top=2 head=0 tab_rows=0 \
                    offset_x=0 offset_y=0 band_left=0 band_top=0 crop_left=0 crop_top=0";

fn glyphs(lines: &[&str]) -> Vec<Glyph> {
    screen_cells(&lines.iter().map(|l| (*l).to_string()).collect::<Vec<_>>())
}

#[test]
fn the_status_names_the_body_and_the_icons() {
    let row = "trail style=\"rainbow kitty pet\" pet_body=436,509,88,132 pet_focus=typing \
               word_icons=40,60,10,30;100,120,10,30 selection=front";
    assert_eq!(status_box(row, "pet_body"), Ok(Some((436, 509, 88, 132))));
    assert_eq!(
        status_boxes(row, "word_icons"),
        Ok(Some(vec![(40, 60, 10, 30), (100, 120, 10, 30)]))
    );
    assert_eq!(status_box("trail pet_body=none", "pet_body"), Ok(None));
    assert_eq!(
        status_boxes("trail word_icons=none", "word_icons"),
        Ok(Some(Vec::new()))
    );
    assert_eq!(
        status_boxes("trail pet_body=none", "word_icons"),
        Ok(None),
        "a row without the field names no icons"
    );
    assert!(status_box("trail pet_body=1,2,3", "pet_body").is_err());
}

#[test]
fn a_body_beside_the_line_meets_no_letter_and_one_over_it_does() {
    // `$ echo the quick brown fox` on row 4: its 26 cells span x 12..376, y 106..132.
    let text = glyphs(&["", "", "", "", "$ echo the quick brown fox"]);
    let caret = Some((4, 26));
    // Clear of the line's last cell (`x`, 362..376) and of the caret's (376..390).
    let beside = (404, 474, 88, 132);
    let m = judge(beside, &GRID, &text, caret, &[], SHAVE);
    assert!(!m.over(), "{m:?}");
    // Negative control: the same body four cells to the left lies on `ox` and the caret.
    let over = (404 - 4 * 14, 474 - 4 * 14, 88, 132);
    let m = judge(over, &GRID, &text, caret, &[], SHAVE);
    assert!(m.over());
    assert_eq!(
        m.letters.iter().map(|l| l.2).collect::<String>(),
        "ox",
        "{m:?}"
    );
    assert_eq!(
        m.shaved, 2,
        "a body across both cells meets each past the shave: {m:?}"
    );
    assert!(m.caret_px > 0, "{m:?}");
    // Blank cells are no letters: a body over the spaces between the words meets none.
    let gap = (12 + 6 * 14 + 2, 12 + 7 * 14 - 2, 106, 132);
    assert!(
        judge(gap, &GRID, &text, None, &[], SHAVE)
            .letters
            .is_empty()
    );
}

#[test]
fn a_graze_counts_raw_but_not_shaved_and_an_icon_counts() {
    let text = glyphs(&["ab"]);
    // One pixel into `a`'s cell (x 12..26, y 2..28) from above.
    let graze = (12, 26, -20, 3);
    let m = judge(graze, &GRID, &text, None, &[], SHAVE);
    assert_eq!(m.letters.len(), 1, "{m:?}");
    assert_eq!(m.shaved, 0, "a one-pixel graze is under the shave: {m:?}");
    let icon = (200, 220, 40, 60);
    let m = judge((210, 230, 50, 70), &GRID, &text, None, &[icon], SHAVE);
    assert_eq!(m.icon_px, 100);
    assert!(m.over());
}

static SCRATCH: AtomicU32 = AtomicU32::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = SCRATCH.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "lab-trail-companion-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("solarized-dark")).expect("scratch dir");
    dir
}

fn write_frame(dir: &Path, stem: &str, frame: &str, body: &str, icons: Option<&str>) {
    write_frame_with_head(dir, stem, frame, (body, None), icons);
}

fn write_frame_with_head(
    dir: &Path,
    stem: &str,
    frame: &str,
    (body, head): (&str, Option<&str>),
    icons: Option<&str>,
) {
    let put = |name: String, text: &str| std::fs::write(dir.join(name), text).expect("a sidecar");
    put(format!("{stem}.dims.txt"), DIMS);
    put(format!("{stem}.{frame}.png"), "");
    put(format!("{stem}.{frame}.clean.png"), "");
    // The clean twin keeps a status of its own whose body lies over `brown`: read as a frame, it would be counted
    // (and over the letters), so the walk's exclusion of the twin is what the counts below hold.
    put(
        format!("{stem}.{frame}.clean.status.txt"),
        "trail style=\"rainbow kitty pet\" pet_body=250,320,88,132 pet_focus=typing word_icons=none\n",
    );
    put(
        format!("{stem}.{frame}.text.txt"),
        "\n\n\n\n$ echo the quick brown fox\n",
    );
    put(
        format!("{stem}.{frame}.cursor.txt"),
        "OK 4 26 1 blinking_block\n",
    );
    let icons = icons.map_or(String::new(), |i| format!(" word_icons={i}"));
    let head = head.map_or(String::new(), |h| format!(" head_body={h}"));
    put(
        format!("{stem}.{frame}.status.txt"),
        &format!(
            "trail style=\"rainbow kitty pet\" pet_body={body}{head} pet_focus=typing{icons}\n"
        ),
    );
}

#[test]
fn a_gallery_tree_is_read_end_to_end() {
    let dir = scratch("tree");
    write_frame(
        &dir,
        "rainbow-kitty-pet-dark",
        "t0",
        "404,474,88,132",
        Some("none"),
    );
    write_frame(&dir, "rainbow-kitty-pet-dark", "t120", "none", Some("none"));
    // Negative control: a body laid over `brown`, and one over a word icon.
    write_frame(
        &dir.join("solarized-dark"),
        "rainbow-dog-dark",
        "mid",
        "250,320,88,132",
        None,
    );
    write_frame(
        &dir,
        "flat-rainbow-dark",
        "t0",
        "500,540,40,80",
        Some("510,530,50,70"),
    );
    let a = parse_args(&[dir.to_string_lossy().into_owned()]).expect("args");
    let out = report(&a).expect("a report");
    let int = |k: &str| out.get(k).and_then(J::as_i64);
    assert_eq!(int("frames"), Some(4), "{}", render(&out));
    assert_eq!(int("with_body"), Some(3));
    assert_eq!(int("companion_over_letters"), Some(2));
    assert_eq!(int("on_letters"), Some(1));
    assert_eq!(
        int("shaved"),
        Some(1),
        "the body over `brown` meets it past the shave"
    );
    assert_eq!(int("on_icons"), Some(1));
    assert_eq!(
        int("icons_unread"),
        Some(1),
        "the Solarized frame names no icons"
    );
    let listed = out.get("listed").and_then(J::as_arr).expect("listed");
    assert_eq!(listed.len(), 2);
    assert_eq!(
        listed[0].get("frame").and_then(J::as_str),
        Some("flat-rainbow-dark.t0.png")
    );
    let over = parse_args(&[
        dir.to_string_lossy().into_owned(),
        "--max".into(),
        "1".into(),
    ])
    .expect("args");
    let failures = report(&over).expect("a report");
    assert_eq!(
        failures.get("failures").and_then(J::as_arr).map(<[J]>::len),
        Some(1),
        "two frames over --max 1 fail"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn report_of(path: &Path, max: Option<i64>) -> Result<J, String> {
    let mut args = vec![path.to_string_lossy().into_owned()];
    if let Some(m) = max {
        args.extend(["--max".into(), m.to_string()]);
    }
    report(&parse_args(&args).expect("args"))
}

#[test]
fn a_path_with_no_frame_is_an_error_not_a_pass() {
    let dir = scratch("empty");
    assert!(
        report_of(&dir.join("no-such-gallery"), Some(0)).is_err(),
        "a typo'd path is a file error (exit 2)"
    );
    assert!(
        report_of(&dir, Some(0)).is_err(),
        "a tree with no frame is no evidence"
    );
    std::fs::write(dir.join("x-dark.t0.png"), "").expect("a frame");
    assert!(
        report_of(&dir, Some(0)).is_err(),
        "a frame without its status is no evidence"
    );
    // Negative control: the same tree once it holds a frame read whole reports, and passes.
    write_frame(
        &dir,
        "rainbow-kitty-pet-dark",
        "t0",
        "404,474,88,132",
        Some("none"),
    );
    let out = report_of(&dir, Some(0)).expect("a report");
    assert_eq!(out.get("frames").and_then(J::as_i64), Some(1));
    assert_eq!(
        out.get("failures").and_then(J::as_arr).map(<[J]>::len),
        Some(0)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_frame_not_read_is_never_counted_clean() {
    let stem = "rainbow-dog-dark";
    // A body over `brown`, the frame's text, caret or status then left unread as a gallery can leave it.
    let frame = |tag: &str, unread: &dyn Fn(&Path)| {
        let dir = scratch(tag);
        write_frame(&dir, stem, "t0", "250,320,88,132", Some("none"));
        unread(&dir);
        let out = report_of(&dir, Some(0)).expect("a report");
        let _ = std::fs::remove_dir_all(&dir);
        out
    };
    let int = |out: &J, k: &str| out.get(k).and_then(J::as_i64);
    let failed = |out: &J| {
        out.get("failures")
            .and_then(J::as_arr)
            .map_or(0, <[J]>::len)
    };
    // Negative control: read whole, the frame is over the letters and fails --max 0 on that alone.
    let whole = frame("whole", &|_| {});
    assert_eq!(
        int(&whole, "companion_over_letters"),
        Some(1),
        "{}",
        render(&whole)
    );
    assert_eq!(failed(&whole), 1, "{}", render(&whole));
    let rm = |dir: &Path, suffix: &str| {
        std::fs::remove_file(dir.join(format!("{stem}.t0{suffix}"))).expect("a sidecar");
    };
    let no_text = frame("no-text", &|d| rm(d, ".text.txt"));
    assert_eq!(
        int(&no_text, "text_unread"),
        Some(1),
        "{}",
        render(&no_text)
    );
    assert_eq!(int(&no_text, "companion_over_letters"), Some(0));
    assert_eq!(
        failed(&no_text),
        1,
        "an unread frame fails --max: {}",
        render(&no_text)
    );
    let no_caret = frame("no-caret", &|d| rm(d, ".cursor.txt"));
    assert_eq!(
        int(&no_caret, "caret_unread"),
        Some(1),
        "{}",
        render(&no_caret)
    );
    assert_eq!(failed(&no_caret), 1, "{}", render(&no_caret));
    let failed_status = frame("err-status", &|d| {
        let put = |suffix: &str, text: &str| {
            std::fs::write(d.join(format!("{stem}.t0{suffix}")), text).expect("a sidecar");
        };
        put(".status.txt", "");
        put(".status.txt.stderr", "ERR no focused window\n");
    });
    assert_eq!(
        int(&failed_status, "status_unread"),
        Some(1),
        "{}",
        render(&failed_status)
    );
    assert_eq!(int(&failed_status, "frames"), Some(1));
    assert_eq!(failed(&failed_status), 1, "{}", render(&failed_status));
    // The open redraw-stall frame: its text and caret deliberately unread, counted apart and not failed.
    let open = frame("open", &|d| {
        rm(d, ".text.txt");
        rm(d, ".cursor.txt");
        std::fs::write(
            d.join(format!("{stem}.t0.read-notes.txt")),
            "cursor/text were not requested\n",
        )
        .expect("a note");
    });
    assert_eq!(int(&open, "open"), Some(1), "{}", render(&open));
    assert_eq!(int(&open, "text_unread"), Some(0));
    assert_eq!(failed(&open), 0, "{}", render(&open));
}

/// looks10 L10: nyan's flying head names its drawn body in `head_body=`, and the verb reads it as it reads the pet's.
/// A head clear of the line and the caret meets nothing; negative control: one laid over `fox` and the caret counts in
/// `head_over`, `on_letters` and `on_caret`; a status that names no head is counted in `heads_unread`.
#[test]
fn the_flying_head_is_read_as_the_pet_is() {
    let dir = scratch("head");
    let stem = "nyan-rainbow-dark";
    write_frame_with_head(
        &dir,
        stem,
        "t0",
        ("none", Some("404,432,80,106")),
        Some("none"),
    );
    write_frame_with_head(&dir, stem, "t120", ("none", Some("none")), Some("none"));
    write_frame(&dir, stem, "t470", "none", Some("none"));
    let clean = report(&parse_args(&[dir.to_string_lossy().into_owned()]).expect("args"))
        .expect("a report");
    let int = |out: &J, k: &str| out.get(k).and_then(J::as_i64);
    assert_eq!(int(&clean, "frames"), Some(3), "{}", render(&clean));
    assert_eq!(int(&clean, "with_body"), Some(1));
    assert_eq!(int(&clean, "with_head"), Some(1));
    assert_eq!(int(&clean, "head_over"), Some(0), "{}", render(&clean));
    assert_eq!(int(&clean, "companion_over_letters"), Some(0));
    assert_eq!(
        int(&clean, "heads_unread"),
        Some(1),
        "the t470 status names no head"
    );
    write_frame_with_head(
        &dir,
        stem,
        "mid",
        ("none", Some("348,384,100,124")),
        Some("none"),
    );
    let over = report(&parse_args(&[dir.to_string_lossy().into_owned()]).expect("args"))
        .expect("a report");
    assert_eq!(int(&over, "head_over"), Some(1), "{}", render(&over));
    assert_eq!(int(&over, "on_letters"), Some(1));
    assert_eq!(int(&over, "on_caret"), Some(1));
    assert_eq!(int(&over, "companion_over_letters"), Some(1));
    let listed = over.get("listed").and_then(J::as_arr).expect("listed");
    assert_eq!(
        listed[0].get("head_cells").map(compact),
        Some(r#"[[4, 24, "o"], [4, 25, "x"]]"#.to_string()),
        "{}",
        render(&over)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_flags_are_checked() {
    assert!(parse_args(&[]).is_err());
    assert!(parse_args(&["x".into(), "--shave".into(), "0.7".into()]).is_err());
    assert!(parse_args(&["x".into(), "--list".into(), "some".into()]).is_err());
    assert!(parse_args(&["x".into(), "--bogus".into()]).is_err());
    let a = parse_args(&["x".into(), "--list".into(), "all".into()]).expect("args");
    assert!(a.list_all);
    assert_eq!(GRID.cell_box(4, 0, 1).y0, 106);
}

/// A settings frame's cursor scene card keeps its own grid (`settings card`'s `dims` line, written as
/// `<stem>.<frame>.dims.txt` by the gallery's `card_sidecars`): the companion over the card's letters is counted
/// on the card's cells. Without the frame's own dims (the negative control) the window's grid places the card's
/// rows elsewhere, and the same body over `README.md` meets no letter.
#[test]
fn a_settings_cards_own_grid_places_its_letters() {
    let dir = scratch("card");
    let stem = "rainbow-kitty-pet-dark-settings";
    let frame = "motion-1500";
    std::fs::write(
        dir.join(format!("{stem}.dims.txt")),
        "OK 40 110 1100 800 cell_w=10 cell_h=20 pad=0 pad_top=0 head=0\n",
    )
    .expect("the window's dims");
    std::fs::write(dir.join(format!("{stem}.{frame}.png")), "").expect("the frame");
    // Row 5's `README.md` spans cols 17..26 (x 266 + 9 * col, y 148 + 5 * 16 = 228 .. 244): the body
    // over x 437..473 meets cols 19..22, `ADME`, and none of row 4's cells over it.
    let reply = "OK 11\n\
                 dims OK 7 77 693 112 cell_w=9 cell_h=16 pad=266 pad_top=148 head=0\n\
                 cursor OK 5 40 1 steady_block\n\
                 status phase=1500 pet_body=437,473,230,244 word_icons=none\n\
                 text src  README.md  build.sh\n\
                 text ~/demo $ cat README.md\n\
                 text ship it\n\
                 text ~/demo $ git add README.md && git commit\n\
                 text [main 4f2c9e1] ship it\n\
                 text ~/demo $ git add README.md && git commit\n\
                 text\n";
    for (suffix, body) in crate::trail_gallery::card_sidecars(reply).expect("the card's sidecars") {
        std::fs::write(dir.join(format!("{stem}.{frame}.{suffix}")), body).expect("a sidecar");
    }
    let png = dir.join(format!("{stem}.{frame}.png"));
    let read = read_frame(&png, SHAVE).expect("a read").expect("a status");
    assert!(read.meets.over(), "{:?}", read.meets);
    assert_eq!(
        read.meets.letters.iter().map(|l| l.2).collect::<String>(),
        "ADME",
        "{:?}",
        read.meets
    );
    std::fs::remove_file(dir.join(format!("{stem}.{frame}.dims.txt")))
        .expect("drop the card's dims");
    let read = read_frame(&png, SHAVE).expect("a read").expect("a status");
    assert!(
        read.meets.letters.is_empty(),
        "on the window's grid row 11 holds no text: {:?}",
        read.meets
    );
    let _ = std::fs::remove_dir_all(&dir);
}
