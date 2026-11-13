// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

// Replies as 0.199.0's release images gave them on a Windows 11 x64 host.
const VERSION: &str = "aterm 0.199.0\nby Andrew Yates · ALab · alab.systems\nbuild: 1794421328\n\
running: C:\\dist\\aterm\\aterm.exe\ntrusts: master=155e30f5\n";
const DIMS: &str = "OK 24 80 720 552 session=0 cell_w=9 cell_h=23 font_px=17.00 scale=1.00 window=0 \
window_rows=24 window_cols=80 geometry=headless presented_seq=none";
const PERCENTILES: &str = "OK n_input=0 input_p50_ms=0.000000 input_p95_ms=0.000000 input_p99_ms=0.000000 \
n_present=0 present_p50_ms=0.000000 present_p95_ms=0.000000 present_p99_ms=0.000000 n_render=0";
const SYS_MEM: &str = "aterm-ctl: OK 3 sys v=1 view=mem scope=global as_of=19:14:22.442 age_ms=88\n\
mem     17.4 GiB used · 31.1 GiB total · thrashing: unavailable(not_sampled)\n\
classes used 17.36 = app 8.63 + wired 8.73 + compressed unavailable(not_sampled)\n\
commit  19.62 of 33.10 GiB (59%)";

fn record() -> Build {
    Build {
        version: "0.199.0".to_string(),
        commit: "1a37de31a340".to_string(),
        commit_full: "1a37de31a3401234567890abcdef1234567890ab".to_string(),
        build_number: 1_794_421_328,
        arch: "x64".to_string(),
        toolchain: "stock 1.97.1".to_string(),
        dirty: false,
        built_at: "2026-11-11T19:14:22Z".to_string(),
    }
}

#[test]
fn identity_reads_each_image() {
    let id = identity(VERSION).expect("aterm");
    assert_eq!(
        (id.name.as_str(), id.version.as_str(), id.build),
        ("aterm", "0.199.0", Some(1_794_421_328))
    );
    let gui = identity("aterm-gui 0.199.0\n").expect("aterm-gui");
    assert_eq!((gui.name.as_str(), gui.build), ("aterm-gui", None));
    // Negative controls: an alias that refuses the flag is not an identity.
    assert_eq!(identity("atpkg: unknown verb '--version'\n"), None);
    assert_eq!(identity(""), None);
}

#[test]
fn version_row_holds_the_image_to_its_record() {
    let with = version_row(VERSION, Some(&record())).expect("the record matches");
    assert!(
        with.contains("build 1794421328") && with.contains("1a37de31a340"),
        "{with}"
    );
    let without = version_row(VERSION, None).expect("a dist older than the record");
    assert!(without.ends_with("no BUILD.json"), "{without}");
    // Negative controls: the record names another build, or another version.
    let other_build = Build {
        build_number: 1_794_421_329,
        ..record()
    };
    assert!(version_row(VERSION, Some(&other_build)).is_err());
    let other_version = Build {
        version: "0.198.0".to_string(),
        ..record()
    };
    assert!(version_row(VERSION, Some(&other_version)).is_err());
    assert!(
        version_row("aterm 0.199.0\n", None).is_err(),
        "no build line"
    );
    assert!(
        version_row(&VERSION.replace("aterm 0", "aterm-gui 0"), None).is_err(),
        "another image"
    );
}

#[test]
fn dims_reads_the_grid_and_the_cell() {
    let d = dims(DIMS).expect("dims");
    assert_eq!(
        (d.rows, d.cols, d.cell_w, d.cell_h, d.font_px.as_str()),
        (24, 80, 9, 23, "17.00")
    );
    assert_eq!(d.to_string(), "24x80 cells of 9x23 px, font 17.00 px");
    // Negative controls.
    assert!(dims("ERR no session").is_err());
    assert!(
        dims(&DIMS.replace("cell_w=9", "cell_w=0")).is_err(),
        "a zero cell"
    );
    assert!(
        dims(&DIMS.replace(" cell_h=23", "")).is_err(),
        "no cell height"
    );
    assert!(dims("OK 24").is_err());
}

#[test]
fn percentiles_quotes_n_present_with_all_three() {
    let quiet = percentiles(PERCENTILES).expect("percentiles");
    assert!(
        quiet.starts_with("n_present=0 present_p50/p95/p99_ms=0.000000/0.000000/0.000000"),
        "{quiet}"
    );
    assert!(quiet.contains("headless"), "{quiet}");
    let live = percentiles(
        &PERCENTILES
            .replace("n_present=0", "n_present=412")
            .replace("present_p99_ms=0.000000", "present_p99_ms=7.25"),
    )
    .expect("percentiles");
    assert_eq!(
        live,
        "n_present=412 present_p50/p95/p99_ms=0.000000/0.000000/7.25"
    );
    // Negative controls: a missing or non-numeric figure is not quoted as `?`.
    assert!(percentiles(&PERCENTILES.replace(" present_p95_ms=0.000000", "")).is_err());
    assert!(percentiles(&PERCENTILES.replace("n_present=0", "n_present=-")).is_err());
    assert!(percentiles("ERR unknown verb").is_err());
}

#[test]
fn image_reply_keeps_a_path_with_spaces() {
    let (w, h, path) =
        image_reply("OK 744 589 \\\\?\\C:\\Users\\a b\\Temp\\s\\images\\frame.png\n")
            .expect("image");
    assert_eq!((w, h), (744, 589));
    assert_eq!(
        path,
        PathBuf::from("\\\\?\\C:\\Users\\a b\\Temp\\s\\images\\frame.png")
    );
    // Negative controls.
    assert!(image_reply("ERR path: give a bare filename in images/").is_err());
    assert!(image_reply("OK 744 589").is_err(), "no path");
    assert!(image_reply("OK wide 589 frame.png").is_err());
}

#[test]
fn frame_refuses_a_flat_or_wrong_sized_image() {
    let (w, h) = (4usize, 2usize);
    let mut drawn = [12u8, 12, 12, 255].repeat(w * h);
    drawn[4..8].copy_from_slice(&[230, 230, 230, 255]);
    assert_eq!(
        frame(&drawn, w, h, (4, 2)),
        Ok("4x2, 2 colours".to_string())
    );
    // Negative controls.
    let flat = [12u8, 12, 12, 255].repeat(w * h);
    assert!(
        frame(&flat, w, h, (4, 2))
            .expect_err("flat")
            .contains("one flat colour")
    );
    assert!(
        frame(&drawn, w, h, (4, 3)).is_err(),
        "not the size the instance said"
    );
    assert!(
        frame(&drawn[..drawn.len() - 4], w, h, (4, 2)).is_err(),
        "a short buffer"
    );
    assert!(frame(&[], 0, 0, (0, 0)).is_err(), "an empty frame");
}

#[test]
fn sys_view_counts_rows_and_unread_values() {
    assert_eq!(
        sys_view(SYS_MEM, "mem"),
        Ok("3 rows, 2 values unavailable (not_sampled 2)".to_string())
    );
    let overview =
        "global  1 of 16 cores busy\naterm-ctl: OK 5 sys v=1 view=overview as_of=19:14:22.442";
    assert_eq!(
        sys_view(overview, ""),
        Ok("5 rows, 0 values unavailable".to_string())
    );
    // Negative controls: another view's answer, an empty view, a usage error.
    assert!(sys_view(SYS_MEM, "disk").is_err());
    assert!(sys_view(&SYS_MEM.replace("OK 3 sys", "OK 0 sys"), "mem").is_err());
    assert!(sys_view("aterm-ctl: ERR usage: sys [overview|tab|aterm]", "machine").is_err());
}

#[test]
fn unavailable_values_are_counted_by_their_reason() {
    let net = "tcp total=unavailable(not_sampled) listen=unavailable(not_sampled) established=20

               link name=Wi-Fi up=0 rx_bps=0 link_bps=unavailable(unsupported)

               link name=Ethernet rx_bps=unavailable(warming) link_bps=1000000000

               cluster req_mhz=unavailable(not_sampled:requested_frequency) x=unavailable(lane_off:coalitions)";
    assert_eq!(
        unavailable(net),
        "6 values unavailable (lane_off 1, not_sampled 3, unsupported 1, warming 1)"
    );
    // Controls: nothing unavailable, and the word alone is not a value.
    assert_eq!(
        unavailable("link name=Ethernet rx_bps=7961"),
        "0 values unavailable"
    );
    assert_eq!(
        unavailable("sizes are unavailable here (no walker)"),
        "0 values unavailable"
    );
}

#[test]
fn renderer_names_the_device_and_refuses_the_cpu() {
    let journal = "aterm-gui: headless mode (--headless)\naterm-gui: GPU rendering on AMD Radeon 780M Graphics (D3d11)\n";
    assert_eq!(
        renderer("OK backend=gpu rows=30", journal),
        Ok("backend=gpu; drew on AMD Radeon 780M Graphics (D3d11)".to_string())
    );
    assert!(renderer("OK backend=gpu rows=30", "").is_ok());
    // Negative controls.
    assert!(renderer("OK backend=cpu rows=30", "aterm-gui: headless mode\n").is_err());
    let failed = format!("{journal}aterm-gui: {SURFACE_FAILED}: device lost\n");
    assert!(renderer("OK backend=gpu rows=30", &failed).is_err());
}

#[test]
fn outcome_codes_and_lines() {
    let pass: Row = ("dims".to_string(), Ok("24x80".to_string()));
    let fail: Row = ("shell".to_string(), Err("no answer".to_string()));
    assert_eq!(line(&pass), "PASS dims: 24x80");
    assert_eq!(line(&fail), "FAIL shell: no answer");
    assert_eq!(Outcome::Ran(vec![pass.clone()]).code(), 0);
    assert_eq!(Outcome::Ran(vec![pass, fail]).code(), 1);
    assert_eq!(Outcome::NotRun("no dist".to_string()).code(), 3);
    assert_eq!(one_line("a\n  b\tc", 10), "a b c");
    assert_eq!(one_line("abcdef", 3), "abc…");
}

#[test]
fn aliases_are_every_image_but_the_front_door_the_window_and_the_console_host() {
    let dist = aterm_tempfile::tempdir().expect("tempdir");
    for name in [
        "aterm.exe",
        "aterm-gui.exe",
        "OpenConsole.exe",
        "aterm-ctl.exe",
        "atpkg.EXE",
        "README.txt",
    ] {
        std::fs::write(dist.path().join(name), name.as_bytes()).expect("write");
    }
    let names: Vec<String> = aliases(dist.path())
        .expect("aliases")
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    assert_eq!(names, ["aterm-ctl.exe", "atpkg.EXE"]);
    // An alias is aterm.exe byte for byte; these stand-ins are not.
    assert_eq!(
        same_bytes(
            &dist.path().join("aterm.exe"),
            &dist.path().join("aterm-ctl.exe")
        )
        .ok(),
        Some(false)
    );
    std::fs::write(dist.path().join("aterm-ctl.exe"), b"aterm.exe").expect("write");
    assert_eq!(
        same_bytes(
            &dist.path().join("aterm.exe"),
            &dist.path().join("aterm-ctl.exe")
        )
        .ok(),
        Some(true)
    );
    std::fs::write(dist.path().join("aterm-ctl.exe"), b"aterm.exf").expect("write");
    assert_eq!(
        same_bytes(
            &dist.path().join("aterm.exe"),
            &dist.path().join("aterm-ctl.exe")
        )
        .ok(),
        Some(false)
    );
}

#[test]
fn smoke_does_not_run_on_a_folder_that_is_no_dist() {
    let empty = aterm_tempfile::tempdir().expect("tempdir");
    let opts = Options {
        dist: empty.path().to_path_buf(),
        scratch: None,
        keep: false,
        frame: None,
    };
    let mut said = Vec::new();
    let outcome = smoke(&opts, &[], &mut |l| said.push(l.to_string()));
    assert_eq!(outcome.code(), 3, "{outcome:?}");
    assert!(said.is_empty(), "nothing was judged: {said:?}");
}

#[test]
fn smoke_refuses_a_scratch_root_with_anything_in_it() {
    let dist = aterm_tempfile::tempdir().expect("tempdir");
    for name in ["aterm.exe", "aterm-gui.exe"] {
        std::fs::write(dist.path().join(name), b"stand-in").expect("write");
    }
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    std::fs::write(scratch.path().join("mine.txt"), b"keep me").expect("write");
    let opts = Options {
        dist: dist.path().to_path_buf(),
        scratch: Some(scratch.path().to_path_buf()),
        keep: false,
        frame: None,
    };
    let outcome = smoke(&opts, &[], &mut |_| {});
    assert!(
        matches!(&outcome, Outcome::NotRun(why) if why.contains("not empty")),
        "{outcome:?}"
    );
    assert!(
        scratch.path().join("mine.txt").is_file(),
        "the folder was left alone"
    );
}
