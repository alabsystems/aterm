// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_calendar_is_civil() {
    assert_eq!(days_from_civil(1970, 1, 1), 0);
    assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    assert_eq!(days_from_civil(1969, 12, 31), -1);
    assert_eq!(days_from_civil(2026, 10, 22), 20_748);
    for days in [-800_000, -1, 0, 59, 11_016, 20_748, 1_000_000] {
        let (y, m, d) = civil_from_days(days);
        assert_eq!(days_from_civil(y, m, d), days, "{y}-{m}-{d}");
    }
}

const US_SPRING_2026: i64 = 1_772_953_200;
const US_FALL_2026: i64 = 1_793_512_800;

#[test]
fn a_posix_rule_says_the_offset_either_side_of_each_change() {
    let us = "EST5EDT,M3.2.0,M11.1.0";
    assert_eq!(posix_offset_at(us, US_SPRING_2026 - 1), Some(-5 * 3600));
    assert_eq!(posix_offset_at(us, US_SPRING_2026), Some(-4 * 3600));
    assert_eq!(posix_offset_at(us, US_FALL_2026 - 1), Some(-4 * 3600));
    assert_eq!(posix_offset_at(us, US_FALL_2026), Some(-5 * 3600));
    assert_eq!(posix_offset_at("EST5EDT", US_SPRING_2026), Some(-4 * 3600));
    let sydney = "AEST-10AEDT,M10.1.0,M4.1.0/3";
    assert_eq!(posix_offset_at(sydney, 1_768_478_400), Some(11 * 3600));
    assert_eq!(posix_offset_at(sydney, 1_784_116_800), Some(10 * 3600));
    assert_eq!(posix_offset_at(sydney, 1_775_318_400 - 1), Some(11 * 3600));
    assert_eq!(posix_offset_at(sydney, 1_775_318_400), Some(10 * 3600));
    assert_eq!(posix_offset_at("IST-5:30", 0), Some(19_800));
    assert_eq!(posix_offset_at("<+0545>-5:45", 0), Some(20_700));
    assert_eq!(posix_offset_at("<-03>3", 0), Some(-3 * 3600));
    for broken in [
        "",
        "E5",
        "EST",
        "EST5EDT,M13.1.0,M11.1.0",
        "EST5EDT,M3.2.0",
        "EST5x",
    ] {
        assert_eq!(posix_offset_at(broken, 0), None, "{broken:?}");
    }
}

fn tzif(version: u8, times: &[i64], kinds: &[u8], utoffs: &[i32], footer: &str) -> Vec<u8> {
    let header = |out: &mut Vec<u8>| {
        out.extend_from_slice(b"TZif");
        out.push(version);
        out.extend_from_slice(&[0; 15]);
        for count in [0, 0, 0, times.len(), utoffs.len(), 4] {
            out.extend_from_slice(&u32::try_from(count).unwrap().to_be_bytes());
        }
    };
    let block = |out: &mut Vec<u8>, wide: bool| {
        for t in times {
            if wide {
                out.extend_from_slice(&t.to_be_bytes());
            } else {
                out.extend_from_slice(&i32::try_from(*t).unwrap().to_be_bytes());
            }
        }
        out.extend_from_slice(kinds);
        for utoff in utoffs {
            out.extend_from_slice(&utoff.to_be_bytes());
            out.extend_from_slice(&[0, 0]);
        }
        out.extend_from_slice(b"ZZZ\0");
    };
    let mut out = Vec::new();
    header(&mut out);
    block(&mut out, false);
    if version >= b'2' {
        header(&mut out);
        block(&mut out, true);
        out.push(b'\n');
        out.extend_from_slice(footer.as_bytes());
        out.push(b'\n');
    }
    out
}

#[test]
fn a_tzif_file_says_the_type_in_force_and_its_footer_after() {
    let times = [-1_000, 0, 1_000];
    let kinds = [1, 0, 1];
    let utoffs = [3600, 7200];
    for version in [0, b'2', b'3'] {
        let file = tzif(version, &times, &kinds, &utoffs, "<+05>-5");
        assert_eq!(
            tzif_offset_at(&file, -5_000),
            Some(3600),
            "before the first"
        );
        assert_eq!(tzif_offset_at(&file, -1_000), Some(7200));
        assert_eq!(tzif_offset_at(&file, 999), Some(3600));
        let last = if version == 0 { 7200 } else { 5 * 3600 };
        assert_eq!(tzif_offset_at(&file, 1_000), Some(last), "{version}");
        assert_eq!(tzif_offset_at(&file, 1 << 40), Some(last), "{version}");
    }
    assert_eq!(
        tzif_offset_at(&tzif(b'2', &[], &[], &[0], "EST5"), 5),
        Some(-5 * 3600)
    );
    assert_eq!(
        tzif_offset_at(&tzif(0, &[], &[], &[-600], ""), 5),
        Some(-600)
    );
    assert_eq!(
        tzif_offset_at(b"not a zone file at all, but long enough", 0),
        None
    );
    assert_eq!(tzif_offset_at(&tzif(0, &[0], &[2], &[0, 1], ""), 5), None);
    let whole = tzif(b'2', &times, &kinds, &utoffs, "EST5");
    assert_eq!(tzif_offset_at(&whole[..60], 5), None);
}

#[test]
fn only_a_plain_zone_name_reads_a_zone_file() {
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let root = dir.path().join("zoneinfo");
    std::fs::create_dir_all(root.join("Etc")).expect("tree");
    std::fs::write(
        root.join("Etc/Fixed"),
        tzif(b'2', &[], &[], &[0], "<+03>-3"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("outside"),
        tzif(b'2', &[], &[], &[0], "EST5"),
    )
    .unwrap();
    assert_eq!(zone_offset_in(&root, "Etc/Fixed", 0), Some(3 * 3600));
    assert_eq!(zone_offset_in(&root, "Etc/Missing", 0), None);
    for refused in [
        "../outside",
        "/etc/localtime",
        "",
        "Etc/Fi xed",
        "Etc/..",
        "a\nb",
    ] {
        assert!(!plain_zone_name(refused), "{refused:?}");
        assert_eq!(zone_offset_in(&root, refused, 0), None, "{refused:?}");
    }
}

/// The offset the system's own `date` (the reader this module replaced, kept as its oracle)
/// shows at `unix` (`TZ=<zone>` when one is named): its wall clock less the instant. Not its
/// `+%z`: macOS's prints the zone's present standard offset where that differs from the
/// instant's (London in 1970, on British Standard Time, printed `03:09:37 BST` and `+0000`),
/// and drops a local mean time's seconds. `None` where it printed no wall clock.
#[cfg(unix)]
fn date_says(zone: Option<&str>, unix: i64) -> Option<i64> {
    let mut date = crate::spawnfd::command(crate::os_path!("date"));
    if let Some(zone) = zone {
        date.env("TZ", zone);
    }
    if cfg!(target_os = "macos") {
        date.arg("-r").arg(unix.to_string());
    } else {
        date.arg("-d").arg(format!("@{unix}"));
    }
    let out = date.arg("+%Y %m %d %H %M %S").output().expect("run date");
    assert!(out.status.success(), "date: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    let f: Vec<i64> = text
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let [y, mo, d, h, mi, s] = f[..] else {
        return None;
    };
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s - unix)
}

/// Instants from 1955 to 2100, a year and a half apart (so each lands in another season), with
/// the US changes of 2026 either side: past a fat file's last transition (2037) only the
/// footer answers.
#[cfg(unix)]
fn instants() -> Vec<i64> {
    let mut at: Vec<i64> = (-10..=50)
        .map(|i| i * 47_000_000 + 7_777)
        .chain([
            US_SPRING_2026 - 1,
            US_SPRING_2026,
            US_FALL_2026 - 1,
            US_FALL_2026,
        ])
        .chain([2_208_988_800 + 15_000_000, 4_102_444_800 - 9_000_000])
        .collect();
    at.sort_unstable();
    at
}

/// THE ZONE FILE SAYS WHAT `date` SHOWS ([`date_says`]), for zones in both hemispheres, a
/// half-hour and a 45-minute zone, a zone that dropped daylight saving and UTC, at every
/// instant of [`instants`]. NEGATIVE CONTROL: the same file with
/// its footer rule cut to standard time disagrees with `date` past the last transition, so
/// the comparison reads the footer and can fail.
#[cfg(unix)]
#[test]
fn a_named_zone_says_what_date_says() {
    let zones = [
        "America/New_York",
        "Europe/London",
        "Australia/Sydney",
        "Asia/Kolkata",
        "Asia/Kathmandu",
        "America/St_Johns",
        "America/Sao_Paulo",
        "UTC",
    ];
    let present: Vec<&str> = zones
        .into_iter()
        .filter(|z| Path::new(ZONEINFO).join(z).is_file())
        .collect();
    if present.len() < 4 {
        crate::host::could_not_run(format!(
            "this host's {ZONEINFO} lacks the zones the comparison reads (it has {present:?})"
        ));
    }
    let at = instants();
    for zone in &present {
        for &unix in &at {
            assert_eq!(
                zone_offset_at(zone, unix),
                date_says(Some(zone), unix),
                "{zone} at {unix}"
            );
        }
    }
    let file = std::fs::read(Path::new(ZONEINFO).join("America/New_York")).expect("zone file");
    let footer = b"\nEST5EDT,M3.2.0,M11.1.0\n";
    let Some(cut) = file.windows(footer.len()).position(|w| w == footer) else {
        crate::host::could_not_run("this host's America/New_York has no US footer rule");
    };
    let mut tampered = file[..cut].to_vec();
    tampered.extend_from_slice(b"\nEST5\n");
    let summer_2040 = 2_225_000_000;
    assert_eq!(
        date_says(Some("America/New_York"), summer_2040),
        Some(-4 * 3600)
    );
    assert_eq!(tzif_offset_at(&file, summer_2040), Some(-4 * 3600));
    assert_eq!(tzif_offset_at(&tampered, summer_2040), Some(-5 * 3600));
}

/// The host's zone by name: `$TZ`'s (a leading `:` dropped, a path cut after its
/// `zoneinfo/`), else the zone `/etc/localtime` links to; `None` when neither names a
/// plain zone.
#[cfg(unix)]
fn host_zone_name() -> Option<String> {
    let named = |spelled: &str| {
        let spelled = spelled.strip_prefix(':').unwrap_or(spelled);
        let zone = spelled.rsplit_once("zoneinfo/").map_or(spelled, |(_, z)| z);
        plain_zone_name(zone).then(|| zone.to_owned())
    };
    match std::env::var("TZ") {
        Ok(tz) if !tz.is_empty() => named(&tz),
        _ => named(std::fs::read_link("/etc/localtime").ok()?.to_str()?),
    }
}

/// THE LOCAL CLOCK SAYS WHAT `date` SHOWS under the process's own zone, at every instant
/// of [`instants`], and the host's zone read by its name ([`host_zone_name`]: macOS always
/// links `/etc/localtime` to one) from its TZif file agrees. NEGATIVE CONTROL: a zone a
/// day's half away disagrees at every instant: the comparison tells zones apart.
#[cfg(unix)]
#[test]
fn the_local_clock_says_what_date_says() {
    let at = instants();
    for &unix in &at {
        assert_eq!(
            local_offset_at(unix),
            date_says(None, unix),
            "local at {unix}"
        );
    }
    assert!(local_offset_now().is_some());
    let host = host_zone_name().filter(|zone| zone_offset_at(zone, 0).is_some());
    if cfg!(target_os = "macos") && std::env::var_os("TZ").is_none() {
        assert!(
            host.is_some(),
            "macOS links /etc/localtime to a zone it has"
        );
    }
    if let Some(zone) = host {
        for &unix in &at {
            assert_eq!(
                zone_offset_at(&zone, unix),
                local_offset_at(unix),
                "{zone}, the host's zone, at {unix}"
            );
        }
    }
    let far = if local_offset_at(0).unwrap_or(0) >= 0 {
        "Pacific/Pago_Pago"
    } else {
        "Pacific/Kiritimati"
    };
    if Path::new(ZONEINFO).join(far).is_file() {
        for &unix in &at {
            assert_ne!(
                zone_offset_at(far, unix),
                local_offset_at(unix),
                "{far} at {unix}"
            );
        }
    }
}
