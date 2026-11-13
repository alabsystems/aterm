// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn arm(name: &str, instructions: f64, links: &[&str], images: &[&str]) -> Measured {
    Measured {
        name: name.to_string(),
        loads: owned(links),
        announced_images: owned(images),
        summary: Summary {
            n: 1,
            instructions,
            min: instructions,
            max: instructions,
            cycles: instructions / 2.0,
            wall_ms: 1.0,
            user_ms: 1.0,
            system_ms: 1.0,
            resident_mb: 1.0,
        },
    }
}

const SYS: &str = "/usr/lib/libSystem.B.dylib";
const OBJC: &str = "/usr/lib/libobjc.A.dylib";
const APPKIT: &str = "/System/Library/Frameworks/AppKit.framework/Versions/C/AppKit";
const HITOOLBOX: &str = "/System/Library/Frameworks/Carbon.framework/Versions/A/Frameworks/\
                         HIToolbox.framework/Versions/A/HIToolbox";
const CORETEXT: &str = "/System/Library/Frameworks/CoreText.framework/Versions/A/CoreText";
const FOUNDATION: &str = "/System/Library/Frameworks/Foundation.framework/Versions/C/Foundation";

/// The gated arms, each at `instructions`.
fn starts(instructions: f64) -> Vec<Measured> {
    GATED
        .iter()
        .map(|(name, _)| arm(name, instructions, &[], &[]))
        .collect()
}

/// A scratch directory under the run's `TMPDIR`, removed on drop.
fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::tempdir().expect("a scratch directory")
}

#[test]
fn the_status_arm_asks_the_first_session_ls_lists() {
    let listed = "0 0 s-93b14b966d5e7c08438d - alive aterm meta=0\n\
                  0 1 s-0123456789abcdef0123 - alive aterm meta=0\n";
    assert_eq!(sid_of_ls(listed).as_deref(), Some("s-93b14b966d5e7c08438d"));
    assert_eq!(sid_of_ls(""), None);
    assert_eq!(sid_of_ls("0 0\n"), None);
}

/// The controls alone: no stop and no `binds` control measured.
const CONTROLS_ONLY: Bases = Bases {
    none: 15e6,
    all: 32e6,
    cli: 29e6,
    binds: None,
    stops: None,
};

#[test]
fn the_split_takes_the_controls_from_the_start() {
    let split = Split::of(50e6, &CONTROLS_ONLY);
    assert_eq!(split.frameworks, 17e6);
    assert_eq!(split.deferral, 3e6);
    assert_eq!(split.own, 18e6);
    assert_eq!((split.own_by_stop, split.binding), (None, None));
    assert_eq!(split.rows().len(), 4);
    assert!((split.share(split.frameworks) - 34.0).abs() < 1e-9);
    let zero = Bases {
        none: 0.0,
        all: 0.0,
        cli: 0.0,
        ..CONTROLS_ONLY
    };
    assert_eq!(Split::of(0.0, &zero).share(1.0), 0.0);
}

/// The stops cut aterm's own start into three parts that add up to it: to `main` over
/// `all`, the prelude over `main`, the verb over the prelude; the `binds` control prices
/// the binding inside the first.
#[test]
fn the_stops_split_aterms_own_start_into_parts_that_add_up() {
    let bases = Bases {
        binds: Some(41e6),
        stops: Some((44e6, 47e6)),
        ..CONTROLS_ONLY
    };
    let split = Split::of(50e6, &bases);
    assert_eq!(split.own_by_stop, Some([12e6, 3e6, 3e6]));
    assert_eq!(split.binding, Some(9e6));
    let [image, prelude, verb] = split.own_by_stop.expect("stops");
    assert_eq!(image + prelude + verb, split.own);
    let rows = split.rows();
    assert_eq!(rows.len(), 8);
    assert_eq!(rows[4], ("  image and runtime (main - all)", 12e6));
    assert_eq!(rows[5], ("    of it, its imports (binds - all)", 9e6));
    assert_eq!(rows[7], ("  the verb (it - prelude)", 3e6));
}

/// What dyld writes first under `DYLD_PRINT_LIBRARIES`, which no stop's refusal is held to.
const DYLD: &str = "dyld[501]: <4C4C4410-5555-3144-A1B2-C3D4E5F60718> /tmp/aterm\n\
                    dyld[501]: <4C4C4411-5555-3144-A1B2-C3D4E5F60718> /usr/lib/libz.dylib\n";

/// A stop is placed by its refusal, its code and what it writes, not its code alone: an aterm
/// without the hold verb exits 2 too, from its usage refusal past the whole front door.
#[test]
fn a_stop_is_known_by_its_refusal_not_its_code_alone() {
    let [main, prelude] = STOPS;
    let ends = |stop: Stop, got: i32, stderr: &str| {
        ended_elsewhere(stop.name, stop.exit, Some(stop.says), Some(got), stderr)
    };
    assert_eq!(ends(main, 2, DYLD), None);
    let usage = format!(
        "{DYLD}aterm: unknown command '{}' (try --help)\n",
        main.verb
    );
    let why = ends(main, 2, &usage).expect("a usage refusal is not the hold verb's");
    assert!(why.contains("unknown command"), "{why}");
    assert_eq!(ends(prelude, 1, &format!("{DYLD}{}\n", prelude.says)), None);
    assert!(ends(prelude, 1, DYLD).is_some());
    assert!(ends(prelude, 0, &format!("{}\n", prelude.says)).is_some());
    // An arm with nothing to say is held to its code alone.
    assert_eq!(ended_elsewhere("version", 0, None, Some(0), "x\n"), None);
    assert!(ended_elsewhere("version", 0, None, Some(1), "x\n").is_some());
}

/// A thin arm64 Mach-O whose chained fixups import `names` in `format` (1, 2 or 3), laid
/// out as ld lays one out: the header and one `LC_DYLD_CHAINED_FIXUPS`, then its header,
/// its import table and its NUL-terminated names.
fn chained(names: &[&str], format: u32) -> Vec<u8> {
    const ARM64: u32 = 0x0100_000c;
    let le = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    let mut out = Vec::new();
    for v in [0xfeed_facf, ARM64, 0, 2, 1, 16, 0, 0] {
        le(&mut out, v);
    }
    let data = 48;
    for v in [0x8000_0034, 16, data, 0] {
        le(&mut out, v);
    }
    let stride = [0, 4, 8, 16][format as usize];
    let imports = 28;
    let symbols = imports + stride * names.len() as u32;
    for v in [0, 0, imports, symbols, names.len() as u32, format, 0] {
        le(&mut out, v);
    }
    let mut offset = 1;
    let mut text = vec![0u8];
    for name in names {
        match format {
            1 => le(&mut out, 1 | (offset << 9)),
            2 => {
                le(&mut out, 1 | (offset << 9));
                le(&mut out, 0);
            }
            _ => {
                for v in [1, offset, 0, 0] {
                    le(&mut out, v);
                }
            }
        }
        text.extend_from_slice(name.as_bytes());
        text.push(0);
        offset += name.len() as u32 + 1;
    }
    out.extend_from_slice(&text);
    out
}

/// The import table is read in all three of dyld's formats, from a thin file and from a
/// universal one's slice for the host's CPU; a file with no chained fixups, or none for that
/// CPU, is refused by why.
/// NEGATIVE CONTROL: reading format 1's name offset from bit 8 (`e >> 8`) fails with
/// `left: Err("import 2 names no symbol")`.
#[test]
fn a_mach_os_imports_are_read_from_its_chained_fixups() {
    const ARM64: u32 = 0x0100_000c;
    let names = ["_write", "_OBJC_CLASS_$_NSApplication", "__tlv_bootstrap"];
    for format in [1, 2, 3] {
        assert_eq!(
            chained_imports(&chained(&names, format), ARM64).as_deref(),
            Ok(&names.map(str::to_string)[..]),
            "format {format}"
        );
    }
    let thin = chained(&names, 1);
    let mut fat = Vec::new();
    for v in [
        0xcafe_babe_u32,
        2,
        0x0100_0007,
        3,
        4096,
        0,
        12,
        ARM64,
        0,
        4096,
    ] {
        fat.extend_from_slice(&v.to_be_bytes());
    }
    fat.extend_from_slice(&(thin.len() as u32).to_be_bytes());
    fat.extend_from_slice(&14u32.to_be_bytes());
    fat.resize(4096, 0);
    fat.extend_from_slice(&thin);
    assert_eq!(chained_imports(&fat, ARM64).map(|n| n.len()), Ok(3));
    assert_eq!(
        chained_imports(&fat, 0x0100_0012),
        Err("no slice for cputype 0x1000012".to_string())
    );
    let mut bare = thin;
    bare[32..36].copy_from_slice(&0x19u32.to_le_bytes());
    assert_eq!(
        chained_imports(&bare, ARM64),
        Err("no chained fixups (LC_DYLD_CHAINED_FIXUPS)".to_string())
    );
}

fn owned(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|p| (*p).to_string()).collect()
}

/// Each dylib aterm's load commands name becomes the flag that links it: a top-level system
/// framework by name, a `/usr/lib` library by its stem; anything else is refused by name.
#[test]
fn a_dylib_is_linked_by_the_flag_its_install_name_gives() {
    assert_eq!(
        link_flag(APPKIT).as_deref(),
        Ok("-Wl,-needed_framework,AppKit")
    );
    assert_eq!(link_flag(OBJC).as_deref(), Ok("-Wl,-needed-lobjc"));
    assert_eq!(link_flag(SYS).as_deref(), Ok("-Wl,-needed-lSystem"));
    assert_eq!(
        link_flag("/usr/lib/libc++.1.dylib").as_deref(),
        Ok("-Wl,-needed-lc++")
    );
    for refused in [
        HITOOLBOX,
        "/usr/lib/swift/libswiftCore.dylib",
        "@rpath/libfoo.dylib",
        "/usr/lib/lib.dylib",
    ] {
        let why = link_flag(refused).expect_err(refused);
        assert!(why.contains(refused), "{why}");
    }
}

/// The controls come from aterm's load commands at every run: `all` links every dylib aterm
/// does and `cli` all but the window-only frameworks, so a framework aterm starts or stops
/// linking changes both.
#[test]
fn the_controls_follow_aterms_load_commands() {
    let aterm = owned(&[APPKIT, FOUNDATION, OBJC, CORETEXT, SYS]);
    assert_eq!(cli_of(&aterm), owned(&[FOUNDATION, OBJC, SYS]));
    assert_eq!(
        link_flags(&aterm).as_deref(),
        Ok(
            "-Wl,-needed_framework,AppKit -Wl,-needed_framework,Foundation -Wl,-needed-lobjc \
             -Wl,-needed_framework,CoreText -Wl,-needed-lSystem"
        )
    );
    let security = "/System/Library/Frameworks/Security.framework/Versions/A/Security";
    let grown = owned(&[APPKIT, FOUNDATION, OBJC, CORETEXT, SYS, security]);
    assert_eq!(cli_of(&grown), owned(&[FOUNDATION, OBJC, SYS, security]));
    assert!(
        link_flags(&grown)
            .expect("flags")
            .ends_with(" -Wl,-needed_framework,Security")
    );
    assert!(link_flags(&owned(&[APPKIT, HITOOLBOX])).is_err());
}

/// A control whose own load commands are not what it was built to load (a flag the linker
/// dropped, a build without the lane's flags) fails the lane, naming what it misses and adds;
/// what the bare control loads is not counted against it.
#[test]
fn a_control_that_links_other_than_it_was_built_to_is_named() {
    let none = owned(&[SYS]);
    let want = owned(&[APPKIT, OBJC, SYS]);
    assert_eq!(
        mislinked("all", &owned(&[SYS, APPKIT, OBJC]), &want, &none),
        None
    );
    assert_eq!(mislinked("none", &none, &[], &none), None);
    let bare = mislinked("all", &none, &want, &none).expect("a finding");
    assert!(
        bare.contains("`all` control") && bare.contains("misses AppKit, libobjc.A.dylib"),
        "{bare}"
    );
    let more =
        mislinked("cli", &owned(&[SYS, APPKIT, OBJC, CORETEXT]), &want, &none).expect("a finding");
    assert!(more.contains("misses none and adds CoreText"), "{more}");
}

#[test]
fn dyld_announcements_retain_delayed_paths_and_omit_the_main_executable() {
    let told = "dyld[22789]: <9F0BFFE1-66F1-3089-90BD-8832CDA4568A> /x/target/release/ctl\n\
                dyld[22789]: <4FDC9AA6-B344-37FE-B8C6-A4C94A038F57> /usr/lib/libSystem.B.dylib\n\
                a line of the program's own\n\
                dyld[22789]: /usr/lib/libobjc.A.dylib\n\
                dyld[22789]: /System/Library/Frameworks/ServiceManagement.framework/Versions/A/ServiceManagement\n\
                dyld[22789]: move loaded to delayed: ServiceManagement\n";
    assert_eq!(
        announced_images_of(told),
        [
            SYS,
            OBJC,
            "/System/Library/Frameworks/ServiceManagement.framework/Versions/A/ServiceManagement"
        ]
    );
    assert_eq!(announced_images_of("no dyld here\n"), Vec::<String>::new());
}

#[test]
fn a_median_above_its_ceiling_fails_and_a_missing_key_is_unjudged() {
    let base = aterm_json::from_str::<Value>(&baseline_json(
        &[("version_minstr", 40.0), ("ctl_help_minstr", 40.0)],
        "c",
    ))
    .expect("the baseline parses");
    let green = judge(
        &[("version_minstr", 50.0), ("ctl_help_minstr", 41.0)],
        &base,
    );
    assert_eq!(green.status, Status::Pass);
    let red = judge(&[("version_minstr", 50.1)], &base);
    assert!(red.failed(), "{red:?}");
    let half = judge(
        &[("version_minstr", 41.0), ("ctl_status_minstr", 9.0)],
        &base,
    );
    assert_eq!(half.status, Status::Pass);
    assert_eq!(half.unjudged, ["cli-start/ctl_status_minstr (no baseline)"]);
    let none = judge(&[("ctl_status_minstr", 9.0)], &base);
    assert!(matches!(none.status, Status::NotJudged(_)), "{none:?}");
}

/// Only the starts are gated: a stop and the `binds` control are reported, but give no
/// baseline key and no trend row.
#[test]
fn the_gated_medians_are_in_millions_and_only_the_starts() {
    let arms = vec![
        arm("version", 54e6, &[], &[]),
        arm("none", 15e6, &[], &[SYS]),
        arm("ctl-status", 60e6, &[], &[]),
        arm("main", 44e6, &[], &[]),
        arm("prelude", 47e6, &[], &[]),
        arm("binds", 41e6, &[], &[SYS]),
    ];
    assert_eq!(
        gated(&arms),
        vec![("version_minstr", 54.0), ("ctl_status_minstr", 60.0)]
    );
    let dir = scratch();
    let trended: Vec<String> = conclude(&arms, &[], false, true, &dir.path().join("none"))
        .samples
        .into_iter()
        .map(|s| s.metric)
        .collect();
    assert_eq!(trended, ["version_minstr", "ctl_status_minstr"]);
    let text = report(&arms);
    assert!(text.contains("version"), "{text}");
    assert!(
        !text.contains("split"),
        "no split without the controls: {text}"
    );
}

#[test]
fn interrupted_cli_publication_preserves_history() {
    let dir = scratch();
    let path = dir.path().join(BASELINE_FILE);
    let arms = starts(50e6);
    assert_eq!(
        conclude(&arms, &[], true, true, &path).status,
        Status::Recorded(path.clone())
    );
    let old = std::fs::read(&path).unwrap();
    assert!(
        conclude_using(
            &arms,
            &[],
            true,
            true,
            &path,
            super::super::publication::interrupted
        )
        .failed()
    );
    assert_eq!(std::fs::read(&path).unwrap(), old);
}

/// A gated arm the run could not measure (the headless instance never came up) is NOT
/// JUDGED by name and why, so the verdict cannot say every lane was judged; the arms it
/// measured are still judged and trended, and a recording refuses to write a baseline
/// without it.
#[test]
fn a_gated_arm_not_measured_is_named_not_judged() {
    let dir = scratch();
    let baseline = dir.path().join(BASELINE_FILE);
    let three = [
        ("version_minstr", 50.0),
        ("ctl_help_minstr", 50.0),
        ("ctl_status_minstr", 50.0),
    ];
    let written = baseline_json(&three, "c");
    std::fs::write(&baseline, &written).expect("the baseline is written");
    let measured: Vec<Measured> = starts(50e6)
        .into_iter()
        .filter(|a| a.name != "ctl-status")
        .collect();
    let why = vec![(
        "ctl-status".to_string(),
        "the headless instance: aterm-lab instance said nothing ready".to_string(),
    )];
    let run = conclude(&measured, &why, false, true, &baseline);
    assert_eq!(run.status, Status::Pass, "{run:?}");
    assert_eq!(
        run.unjudged,
        [
            "cli-start/ctl_status_minstr (not measured: the headless instance: aterm-lab \
             instance said nothing ready)"
        ]
    );
    assert_eq!(run.samples.len(), 2, "{run:?}");
    let (text, _) = super::super::verdict(&[(LANE, &run, 1.0)]);
    assert!(!text.contains("GREEN — every lane"), "{text}");
    assert!(
        text.contains("NOT JUDGED: cli-start/ctl_status_minstr (not measured"),
        "{text}"
    );
    let unexplained = conclude(&measured, &[], false, true, &baseline);
    assert_eq!(
        unexplained.unjudged,
        ["cli-start/ctl_status_minstr (not measured: no launch of it was measured)"]
    );
    let refused = conclude(&measured, &why, true, true, &baseline);
    assert!(refused.failed(), "{refused:?}");
    assert_eq!(
        std::fs::read_to_string(&baseline).expect("the baseline reads"),
        written
    );
    let whole = conclude(&starts(50e6), &[], false, true, &baseline);
    assert!(whole.unjudged.is_empty(), "{whole:?}");
}

/// An aterm `--binary` names is judged but never feeds the trend (whose rows are keyed by
/// this tree), and a recording of it refuses before it writes; this tree's own build
/// records.
#[test]
fn only_this_trees_own_build_feeds_the_trend_or_records() {
    let dir = scratch();
    let baseline = dir.path().join(BASELINE_FILE);
    let own = conclude(&starts(50e6), &[], false, true, &baseline);
    assert_eq!(own.samples.len(), 3, "{own:?}");
    assert!(own.samples.iter().all(|s| s.inverted && s.lane == LANE));
    let named = conclude(&starts(50e6), &[], false, false, &baseline);
    assert!(named.samples.is_empty(), "{named:?}");
    assert_eq!(named.status, own.status);
    let refused = conclude(&starts(50e6), &[], true, false, &baseline);
    match &refused.status {
        Status::Fail(why) => assert!(why[0].contains("REFUSING to record"), "{why:?}"),
        other => panic!("a --binary aterm was recorded: {other:?}"),
    }
    assert!(
        !baseline.exists(),
        "the refusal wrote {}",
        baseline.display()
    );
    let recorded = conclude(&starts(50e6), &[], true, true, &baseline);
    assert_eq!(recorded.status, Status::Recorded(baseline.clone()));
    assert!(baseline.is_file());
}

#[test]
fn the_report_splits_instruction_work_with_and_without_stops() {
    let arms = vec![
        arm(
            "version",
            50e6,
            &[],
            &[SYS, FOUNDATION, CORETEXT, APPKIT, HITOOLBOX, OBJC],
        ),
        arm("none", 15e6, &[], &[SYS]),
        arm(
            "all",
            32e6,
            &[APPKIT, FOUNDATION, CORETEXT],
            &[SYS, APPKIT, HITOOLBOX, FOUNDATION, CORETEXT],
        ),
        arm("cli", 29e6, &[FOUNDATION], &[SYS, FOUNDATION, CORETEXT]),
    ];
    let text = report(&arms);
    let row = |label: &str| {
        text.lines()
            .find(|l| l.trim_start().starts_with(label))
            .unwrap_or_else(|| panic!("no `{label}` row: {text}"))
            .split_whitespace()
            .rev()
            .take(2)
            .collect::<Vec<_>>()
    };
    assert_eq!(row("frameworks (all - none)"), ["34%", "17.00"]);
    assert_eq!(row("window-only deferral (all - cli)"), ["6%", "3.00"]);
    assert_eq!(row("aterm's own (it - all)"), ["36%", "18.00"]);
    assert!(
        !text.contains("image and runtime"),
        "no stops, no rows: {text}"
    );
    let mut stopped = arms;
    stopped.push(arm("main", 44e6, &[], &[]));
    stopped.push(arm("prelude", 47e6, &[], &[]));
    let text = report(&stopped);
    let row = |label: &str| {
        text.lines()
            .find(|l| l.trim_start().starts_with(label))
            .unwrap_or_else(|| panic!("no `{label}` row: {text}"))
            .split_whitespace()
            .rev()
            .take(2)
            .collect::<Vec<_>>()
    };
    assert_eq!(row("image and runtime (main - all)"), ["24%", "12.00"]);
    assert_eq!(row("front door prelude (prelude - main)"), ["6%", "3.00"]);
    assert_eq!(row("the verb (it - prelude)"), ["6%", "3.00"]);
}

#[test]
fn a_summary_is_the_median_and_the_range() {
    let takes = Takes {
        instructions: vec![3.0, 1.0, 2.0],
        cycles: vec![1.0, 1.0, 4.0],
        wall_ms: vec![0.5, 0.7, 0.6],
        user_ms: vec![0.2, 0.1, 0.3],
        system_ms: vec![0.4, 0.4, 0.4],
        resident_mb: vec![9.0, 7.0, 8.0],
    };
    let s = takes.summary().expect("a summary");
    assert_eq!(
        (s.n, s.instructions, s.min, s.max, s.cycles),
        (3, 2.0, 1.0, 3.0, 1.0)
    );
    assert_eq!((s.user_ms, s.system_ms, s.resident_mb), (0.2, 0.4, 8.0));
    assert_eq!(Takes::default().summary(), None);
}
