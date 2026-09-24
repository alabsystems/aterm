// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISK, BEFORE ANYTHING IS BUILT.
//!
//! WHY (2026-09-20/21). The gate builds a pinned snapshot of the caller's tree
//! in several target dirs (`target/`, `target-tippy/`, `target-drivers/`,
//! `target-xtask/`, `target-regex/`, plus the L0 gate's and the libc oracle's
//! own), and nothing bounded what they held. Measured across incremental
//! `--fast` runs: `target/` grew 36 GB -> 55 GB, `target-tippy/` sat at 16-18 GB
//! and `target-drivers/` at 16-20 GB, on a 926 GB volume that also carries
//! `$HOME/trust` (428 GB) and `~/ay` (195 GB). Two contract runs died mid-ladder
//! with `No space left on device` — one on a stage log that could not be
//! created (`aterm-verify: cannot run …/targo: No space left on device (os
//! error 28)`), one inside a build child (`error: failed to write
//! …/target/debug/deps/…/lib.rmeta: No space left on device`) — then `verify:
//! cannot write the ladder`, and the rows they had managed to print were `FAIL`
//! rows of a FINDING's severity. WHICH stage each died on was not recorded, and
//! neither was the free space they started with: no ladder from that day
//! survives, and no run before this change read the disk at all. After deleting
//! those dirs, a cold run with `CARGO_INCREMENTAL=0` in the environment passed
//! the contract; the snapshot it left measured 23 GB on 2026-09-21, and the
//! passing receipt of that day (`0a45a7446`) was written 32 min after its
//! commit. Incremental artifacts were most of the bloat, and the gate never
//! needed them — it builds each commit once ([`crate::CHILD_ENV`]).
//!
//! WHAT THIS DOES. Before the ladder is planned, [`crate::run`] reads the free
//! space on the volume holding the run's root and REFUSES — COULD NOT RUN, exit
//! 3, never a skip and never a finding — when it is under [`FLOOR_BYTES`],
//! printing the free amount, the floor, what the run's own target dirs hold and
//! the remedy: those dirs are regenerable. A run that starts under the floor
//! does not die at minute forty with a half-written ladder; it says so at
//! second one, and leaves no receipt, so the last real judgement of the commit
//! still stands.
//!
//! WHY 40 GiB. A cold, non-incremental contract run leaves ~23 GB of caches, so
//! that is what a run from empty lanes writes, and a warm run rewrites a large
//! part of it (cargo does not unlink an old artifact before writing the new
//! one). The floor is that footprint again in reserve, because a volume at zero
//! loses the ladder and the receipt — the two files the gate exists to write —
//! on top of losing the build. Below 40 GiB the gate refuses before building
//! anything; above it a cold rebuild fits with ~17 GB to spare. The floor is
//! arithmetic over a measured footprint, NOT a level either 2026-09-20 death
//! was observed at: what those runs started with is not recorded, and the
//! preflight would not necessarily have caught them. What bounds the growth
//! that caused them is `CARGO_INCREMENTAL=0`; the floor bounds the damage when
//! something else fills the volume.
//!
//! The free-space read is `df -Pk`, the POSIX spelling, because this crate has
//! no dependencies on purpose (see its `Cargo.toml`) and std has no `statvfs`.
//! Everything that decides is a pure function of numbers, tested below on
//! synthetic ones.

use std::path::{Path, PathBuf};
use std::process::Command;

/// One gibibyte.
pub const GIB: u64 = 1 << 30;

/// The free-space floor under which the gate refuses to start: 40 GiB (see
/// the module doc for the measurement it comes from).
pub const FLOOR_BYTES: u64 = 40 * GIB;

/// The lane target dirs that do not sit at the root under a `target*` name.
///
/// THE ONE LIST. [`crate::snapshot`]'s `lane_dirs` reads this rather than
/// keeping its own copy: the two were separate until 2026-09-21, and
/// `libc-oracle/target-symgate` was in one and not the other, so a refused
/// operator was told to delete "every one of them" and left a stamped,
/// regenerable lane behind.
pub const NESTED_LANE_DIRS: [&str; 3] = [
    "tools/freeze-safety-gate/target",
    "libc-oracle/target",
    "libc-oracle/target-symgate",
];

/// What the preflight read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reading {
    /// Bytes an unprivileged writer may still take on the volume.
    Free(u64),
    /// The volume could not be measured, and why.
    Unknown(String),
}

/// Read the free space on the volume holding `path`.
#[must_use]
pub fn read_free(path: &Path) -> Reading {
    let out = match Command::new("df").arg("-Pk").arg(path).output() {
        Ok(o) => o,
        Err(e) => return Reading::Unknown(format!("cannot run df -Pk: {e}")),
    };
    if !out.status.success() {
        return Reading::Unknown(format!(
            "df -Pk {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_df(&text).map_or_else(
        || {
            Reading::Unknown(format!(
                "df -Pk printed no Available column: {:?}",
                text.trim()
            ))
        },
        Reading::Free,
    )
}

/// The `Available` column of `df -Pk` output, in bytes.
///
/// ANCHOR ON `Capacity`, NEVER ON A FIELD COUNT. `-P` fixes the column ORDER —
/// `Filesystem 1024-blocks Used Available Capacity Mounted on` — and nothing
/// else. It does NOT promise six whitespace-separated fields: the first column
/// may hold spaces and so may the last. This is not hypothetical on the machine
/// the gate runs on. `df -Pk /System/Volumes/Data/home` prints
///
/// ```text
/// Filesystem    1024-blocks Used Available Capacity  Mounted on
/// map auto_home           0    0         0   100%    /System/Volumes/Data/home
/// ```
///
/// — seven fields, because macOS autofs names the source `map auto_home`. Read
/// by position, the fourth field is `Used`, not `Available`, and on a volume
/// with real numbers that returns a plausible WRONG answer: the gate would be
/// told it had the used bytes free and would start building on a full disk,
/// which is the one outcome this module exists to prevent. A number that is
/// merely absent is safe here — [`decide`] refuses on [`Reading::Unknown`] —
/// so an unreadable row must yield `None`, never a guess.
///
/// `Capacity` is the one field that is a percentage, and `Available` is the
/// field before it. An NFS or sshfs export, an autofs map, and a mount point
/// with spaces all parse correctly that way.
#[must_use]
pub fn parse_df(text: &str) -> Option<u64> {
    let row = text.lines().nth(1)?;
    let fields: Vec<&str> = row.split_whitespace().collect();
    let capacity = fields.iter().position(|f| is_percentage(f))?;
    let kib: u64 = fields.get(capacity.checked_sub(1)?)?.parse().ok()?;
    kib.checked_mul(1024)
}

/// A `df` `Capacity` cell: digits then `%`, and nothing else. Spelled out so a
/// filesystem or mount name that merely CONTAINS a `%` cannot be mistaken for
/// the column the reading is anchored to.
fn is_percentage(field: &str) -> bool {
    field
        .strip_suffix('%')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The preflight's decision, pure over the reading and the floor: `Ok` to
/// proceed, or the sentence of the COULD-NOT-RUN row. A volume that cannot be
/// measured refuses too — a gate that cannot tell whether it can finish does
/// not start, for the same reason a hook that cannot judge does not admit.
///
/// # Errors
/// The ladder row's label, naming the free amount and the floor (or why the
/// volume could not be read).
pub fn decide(reading: &Reading, floor: u64, root: &Path) -> Result<(), String> {
    match reading {
        Reading::Free(free) if *free >= floor => Ok(()),
        Reading::Free(free) => Err(format!(
            "disk: {} free on the volume holding {}, under the {} floor — nothing was built \
             (the volume filled mid-ladder twice on 2026-09-20)",
            gib(*free),
            root.display(),
            gib(floor)
        )),
        Reading::Unknown(why) => Err(format!(
            "disk: the free space on the volume holding {} could not be read ({why}), so the {} \
             floor could not be checked — nothing was built",
            root.display(),
            gib(floor)
        )),
    }
}

/// The `verify: disk …` header line: the reading and the floor, so the record
/// of every run says how much room it started with.
#[must_use]
pub fn header_line(reading: &Reading, floor: u64, root: &Path) -> String {
    match reading {
        Reading::Free(free) => format!(
            "verify: disk {} free on the volume holding {} (floor {})\n",
            gib(*free),
            root.display(),
            gib(floor)
        ),
        Reading::Unknown(why) => format!(
            "verify: disk free space on the volume holding {} could not be read ({why}; floor {})\n",
            root.display(),
            gib(floor)
        ),
    }
}

/// The caller tree's incremental caches, newest-first by size: `target*/…/incremental`
/// directories OUTSIDE this run's root.
///
/// They are the cheapest bytes on the volume to give back, and the reason is
/// structural rather than a guess: every child of this gate compiles with
/// `CARGO_INCREMENTAL=0` ([`crate::CHILD_ENV`]), so nothing in a gate run ever
/// reads them — only the caller's own `targo build` does, and it rebuilds them
/// on demand. MEASURED 2026-09-21 on this machine: the caller tree held 47 GB
/// of which 26 GB was `target.noindex/debug/incremental`; removing it returned
/// 19 GB and cost one warm dev rebuild, while removing the run's own lanes
/// would have returned 7.7 GB and cost the next contract run ~52 minutes.
/// That asymmetry is why [`remedy`] names these first.
#[must_use]
pub fn caller_incremental_dirs(caller: &Path) -> Vec<(PathBuf, u64)> {
    let mut out: Vec<(PathBuf, u64)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(caller) else {
        return out;
    };
    for target in entries.flatten() {
        if !target.file_name().to_string_lossy().starts_with("target") {
            continue;
        }
        if !target.path().symlink_metadata().is_ok_and(|m| m.is_dir()) {
            continue;
        }
        // `target*/<profile>/incremental`, one level down: debug, release, and
        // any custom profile. Deeper is not a cargo layout.
        let Ok(profiles) = std::fs::read_dir(target.path()) else {
            continue;
        };
        for profile in profiles.flatten() {
            let dir = profile.path().join("incremental");
            if dir.symlink_metadata().is_ok_and(|m| m.is_dir()) {
                let bytes = dir_bytes(&dir);
                if bytes > 0 {
                    out.push((dir, bytes));
                }
            }
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// The remedy block under the refusal: what the run's own target dirs hold,
/// each named, and the fact that every one of them is regenerable.
#[must_use]
pub fn remedy(root: &Path, floor: u64, caller: Option<&Path>) -> String {
    let mut s = String::new();
    // The free bytes first. A human under the floor reaches for whatever the
    // message names, and until 2026-09-21 the only thing it named was this
    // run's own warm lanes: the most expensive bytes on the volume, priced at
    // one cold contract run. The caller's incremental caches cost nothing to
    // lose and are usually larger.
    let free_first: Vec<(PathBuf, u64)> = caller
        .filter(|c| *c != root)
        .map(caller_incremental_dirs)
        .unwrap_or_default();
    if !free_first.is_empty() {
        let total: u64 = free_first.iter().map(|(_, b)| *b).sum();
        s.push_str(&format!(
            "  FIRST, and it costs nothing: the calling tree holds {} of incremental caches \
             that no gate run reads (every child compiles with CARGO_INCREMENTAL=0). Removing \
             them loses one warm dev rebuild, not a contract run:\n",
            gib(total)
        ));
        for (d, b) in &free_first {
            s.push_str(&format!("      {:>10}  {}\n", gib(*b), d.display()));
        }
        s.push_str("  Only if that is not enough:\n");
    }
    let dirs = target_dirs(root);
    let sized: Vec<(PathBuf, u64)> = dirs
        .iter()
        .map(|d| (d.clone(), dir_bytes(&root.join(d))))
        .collect();
    let total: u64 = sized.iter().map(|(_, b)| *b).sum();
    if sized.is_empty() {
        s.push_str(&format!(
            "  this run's root {} holds no target dirs yet: a cold run writes ~23 GB of caches, \
             and the volume does not have room for that plus its own headroom.\n",
            root.display()
        ));
    } else {
        s.push_str(&format!(
            "  this run's target dirs hold {} in total, all of it regenerable — remove them and \
             the next run rebuilds cold (~52 min and 23 GB, measured 2026-09-20 on the run whose \
             receipt is 0a45a7446: verdict PASS, merge-contract yes):\n",
            gib(total)
        ));
        for (d, b) in &sized {
            s.push_str(&format!("      {:>10}  {}/\n", gib(*b), d.display()));
        }
    }
    s.push_str(&format!(
        "  or free space elsewhere on the volume. The floor is {} because a cold, \
         non-incremental contract run leaves ~23 GB of caches, and a warm one rewrites much of \
         that before it unlinks anything, and a volume at zero loses the ladder and the receipt \
         as well as the build. Every child of this gate compiles with CARGO_INCREMENTAL=0, so \
         the dirs no longer grow run over run (target/ had reached 55 GB with it on).",
        gib(floor)
    ));
    s
}

/// The run's own target dirs, root-relative: every directory at the root named
/// `target*` (a symlink is not counted — it may point at another volume) plus
/// [`NESTED_LANE_DIRS`] where they exist. Sorted, so the remedy is stable.
#[must_use]
pub fn target_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("target"))
        .filter(|e| e.path().symlink_metadata().is_ok_and(|m| m.is_dir()))
        .map(|e| PathBuf::from(e.file_name()))
        .collect();
    out.extend(
        NESTED_LANE_DIRS
            .iter()
            .map(PathBuf::from)
            .filter(|d| root.join(d).symlink_metadata().is_ok_and(|m| m.is_dir())),
    );
    out.sort();
    out
}

/// The bytes of every regular file under `dir`, symlinks not followed. Best
/// effort: an entry that cannot be read counts nothing. Walked only for a
/// refusal, where seconds spent naming the remedy cost a run that was not
/// going to happen anyway.
#[must_use]
pub fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(m) = e.path().symlink_metadata() else {
                continue;
            };
            if m.is_dir() {
                stack.push(e.path());
            } else if m.is_file() {
                total = total.saturating_add(m.len());
            }
        }
    }
    total
}

/// `bytes` as `N.N GiB`.
#[must_use]
pub fn gib(bytes: u64) -> String {
    // Tenths of a GiB in integer arithmetic, so no float formatting decides a
    // ladder byte — widened so `bytes * 10` cannot overflow, and never
    // `bytes / (GIB / 10)`: that unit truncates short of a real tenth, and
    // rounded 40 GiB - 1 byte UP to "40.0" (measured by the test below).
    let tenths = u128::from(bytes) * 10 / u128::from(GIB);
    format!("{}.{} GiB", tenths / 10, tenths % 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MACOS: &str = "Filesystem   1024-blocks      Used Available Capacity  Mounted on\n\
                         /dev/disk3s5   971350180 862653008  83991144    92%    /System/Volumes/Data\n";
    const LINUX: &str = "Filesystem     1024-blocks      Used Available Capacity Mounted on\n\
                         /dev/nvme0n1p2   981876212 812345678 119548922      88% /\n";

    #[test]
    fn the_available_column_is_read_in_bytes_whatever_the_mount_point_is_called() {
        assert_eq!(parse_df(MACOS), Some(83_991_144 * 1024));
        assert_eq!(parse_df(LINUX), Some(119_548_922 * 1024));
        // A mount point with spaces sits last, so it cannot shift the column.
        let spaced = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                      //u@h/share 100 40 60 40% /Volumes/My Share Name\n";
        assert_eq!(parse_df(spaced), Some(60 * 1024));
    }

    /// THE COLUMN IS FOUND, NOT COUNTED — the reading that would have started a
    /// gate on a full volume.
    ///
    /// `-P` fixes the column ORDER and nothing else, so the FIRST column may
    /// hold spaces too. macOS autofs really does name one `map auto_home`
    /// (`df -Pk /System/Volumes/Data/home` on this machine), which makes the
    /// row seven fields wide; read by position the fourth is `Used`. Given real
    /// numbers that is not a missing answer but a WRONG one, roughly the used
    /// bytes reported as free — and [`decide`] would have said `Ok`. A row this
    /// code cannot anchor must yield `None`, because `None` refuses.
    #[test]
    fn a_filesystem_column_with_spaces_does_not_shift_the_reading_onto_used() {
        // The shape, verbatim from this machine (zero-sized autofs map).
        let autofs = "Filesystem    1024-blocks Used Available Capacity  Mounted on\n\
                      map auto_home           0    0         0   100%    /System/Volumes/Data/home\n";
        assert_eq!(parse_df(autofs), Some(0));
        // The same shape with real numbers: a 900 GiB volume with 12 GiB free.
        let real = "Filesystem    1024-blocks      Used Available Capacity  Mounted on\n\
                    map auto_home   943718400 931135488  12582912   99%    /Users//x\n";
        assert_eq!(
            parse_df(real),
            Some(12_582_912 * 1024),
            "counting fields from the left reads Used (888 GiB) and starts the gate"
        );
        assert!(
            decide(
                &Reading::Free(parse_df(real).expect("parsed")),
                40 * GIB,
                Path::new("/Users//x")
            )
            .is_err(),
            "12 GiB free must refuse at the 40 GiB floor"
        );
        // Spaces at BOTH ends at once.
        let both = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                    map auto_home 100 40 60 40% /Volumes/My Share Name\n";
        assert_eq!(parse_df(both), Some(60 * 1024));
    }

    /// A `%` that is not the `Capacity` cell cannot be mistaken for it, and a
    /// row with no percentage at all reads as nothing rather than as a number
    /// from the wrong column.
    #[test]
    fn only_a_digits_then_percent_cell_anchors_the_reading() {
        assert!(is_percentage("40%"));
        assert!(is_percentage("100%"));
        assert!(!is_percentage("%"));
        assert!(!is_percentage("40"));
        assert!(!is_percentage("4%0"));
        assert!(!is_percentage("/Volumes/50%off"));
        let named = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                     /dev/disk1 100 40 60 40% /Volumes/50%off\n";
        assert_eq!(parse_df(named), Some(60 * 1024));
        let no_capacity = "Filesystem 1024-blocks Used Available Mounted on\n\
                           /dev/disk1 100 40 60 /mnt\n";
        assert_eq!(parse_df(no_capacity), None);
    }

    #[test]
    fn anything_that_is_not_a_df_table_reads_as_nothing() {
        for text in [
            "",
            "Filesystem 1024-blocks Used Available Capacity Mounted on\n",
            "garbage\nmore garbage\n",
            "h\n/dev/x 1 2 notanumber 3% /\n",
        ] {
            assert_eq!(parse_df(text), None, "{text:?}");
        }
    }

    /// THE DECISION, on synthetic numbers: under the floor refuses, at or over
    /// it proceeds, and a volume that cannot be read refuses rather than being
    /// read as roomy.
    #[test]
    fn the_floor_is_a_refusal_below_it_and_nothing_at_or_above_it() {
        let root = Path::new("/Users//x/aterm-verify.noindex");
        let floor = 40 * GIB;
        assert_eq!(decide(&Reading::Free(40 * GIB), floor, root), Ok(()));
        assert_eq!(decide(&Reading::Free(400 * GIB), floor, root), Ok(()));
        let why = decide(&Reading::Free(40 * GIB - 1), floor, root).expect_err("one byte under");
        assert!(why.starts_with("disk: 39.9 GiB free"), "{why}");
        assert!(why.contains("under the 40.0 GiB floor"), "{why}");
        assert!(why.contains("nothing was built"), "{why}");
        let why = decide(&Reading::Free(12 * GIB + GIB / 2), floor, root).expect_err("well under");
        assert!(why.contains("12.5 GiB free"), "{why}");
        assert!(why.contains(root.to_str().unwrap()), "{why}");
        // A floor of zero is the knob that never refuses (a test's, never the gate's).
        assert_eq!(decide(&Reading::Free(0), 0, root), Ok(()));
        let why = decide(
            &Reading::Unknown("cannot run df -Pk: gone".into()),
            floor,
            root,
        )
        .expect_err("unmeasurable refuses");
        assert!(
            why.contains("could not be read (cannot run df -Pk: gone)"),
            "{why}"
        );
        assert!(why.contains("floor could not be checked"), "{why}");
    }

    #[test]
    fn the_header_line_names_the_reading_and_the_floor_on_one_line() {
        let root = Path::new("/r");
        // A tenth of a GiB is not a whole number of bytes, so the input sits one
        // byte above the boundary: `GIB / 10` alone is just under a real tenth.
        let line = header_line(&Reading::Free(80 * GIB + GIB / 10 + 1), 40 * GIB, root);
        assert_eq!(
            line,
            "verify: disk 80.1 GiB free on the volume holding /r (floor 40.0 GiB)\n"
        );
        let line = header_line(&Reading::Unknown("no df".into()), 40 * GIB, root);
        assert!(line.starts_with("verify: disk free space on the volume holding /r could not be read (no df; floor 40.0 GiB)"), "{line}");
        assert_eq!(line.matches('\n').count(), 1);
    }

    #[test]
    fn sizes_print_in_tenths_of_a_gib_without_a_float() {
        assert_eq!(gib(0), "0.0 GiB");
        assert_eq!(gib(GIB), "1.0 GiB");
        assert_eq!(gib(GIB / 10 - 1), "0.0 GiB");
        assert_eq!(gib(55 * GIB + GIB * 3 / 10 + 1), "55.3 GiB");
        assert_eq!(
            gib(55 * GIB + GIB / 10 * 3),
            "55.2 GiB",
            "three truncated tenths are under 0.3"
        );
        assert_eq!(gib(23 * GIB + GIB - 1), "23.9 GiB");
    }

    /// The remedy names the run's OWN dirs: root-level `target*` directories
    /// and the two nested lane dirs, never a file or a symlink that happens to
    /// carry the name, sized by the bytes their files hold.
    #[cfg(unix)]
    #[test]
    fn the_target_dirs_are_the_lane_dirs_that_exist_and_nothing_that_merely_sounds_like_one() {
        let tmp = crate::mktemp_dir("atv-disk").expect("mktemp");
        for d in [
            "target/debug/deps",
            "target-tippy/debug",
            "target-drivers",
            "libc-oracle/target",
            "tools/freeze-safety-gate/target/x",
            "crates/target-not-a-lane",
        ] {
            std::fs::create_dir_all(tmp.join(d)).expect("mkdir");
        }
        std::fs::write(tmp.join("target/debug/deps/libx.rlib"), vec![0u8; 3000]).expect("write");
        std::fs::write(tmp.join("target/debug/deps/x.d"), vec![0u8; 500]).expect("write");
        std::fs::write(tmp.join("target-tippy/debug/y.rmeta"), vec![0u8; 700]).expect("write");
        std::fs::write(tmp.join("target-notes.txt"), "a file, not a lane\n").expect("write");
        std::os::unix::fs::symlink(tmp.join("target"), tmp.join("target-elsewhere"))
            .expect("symlink");

        let dirs = target_dirs(&tmp);
        let names: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        assert_eq!(
            names,
            [
                "libc-oracle/target",
                "target",
                "target-drivers",
                "target-tippy",
                "tools/freeze-safety-gate/target"
            ]
        );
        assert_eq!(dir_bytes(&tmp.join("target")), 3500);
        assert_eq!(dir_bytes(&tmp.join("target-tippy")), 700);
        assert_eq!(dir_bytes(&tmp.join("target-drivers")), 0);
        assert_eq!(
            dir_bytes(&tmp.join("absent")),
            0,
            "an unreadable dir counts nothing"
        );

        let text = remedy(&tmp, 40 * GIB, None);
        assert!(text.contains("regenerable"), "{text}");
        assert!(
            text.contains("  target/\n") && text.contains("  target-tippy/\n"),
            "{text}"
        );
        assert!(
            !text.contains("target-notes.txt") && !text.contains("target-elsewhere"),
            "{text}"
        );
        assert!(text.contains("CARGO_INCREMENTAL=0"), "{text}");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_root_with_no_target_dirs_still_gets_a_remedy() {
        let tmp = crate::mktemp_dir("atv-disk-empty").expect("mktemp");
        let text = remedy(&tmp, 40 * GIB, None);
        assert!(text.contains("holds no target dirs yet"), "{text}");
        assert!(text.contains("40.0 GiB"), "{text}");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The free bytes come first, and they are named as free. A human under the
    /// floor deletes what the message names, so what it names first decides
    /// whether the next contract run is warm or costs ~52 minutes.
    #[test]
    fn the_remedy_names_the_callers_incremental_caches_before_its_own_warm_lanes() {
        let caller = crate::mktemp_dir("atv-disk-caller").expect("mktemp");
        let root = crate::mktemp_dir("atv-disk-root").expect("mktemp");
        std::fs::create_dir_all(caller.join("target.noindex/debug/incremental")).expect("mk");
        std::fs::write(
            caller.join("target.noindex/debug/incremental/blob"),
            vec![0u8; 4096],
        )
        .expect("write");
        std::fs::create_dir_all(root.join("target-tippy")).expect("mk");
        std::fs::write(root.join("target-tippy/x.rmeta"), vec![0u8; 512]).expect("write");

        let text = remedy(&root, 40 * GIB, Some(&caller));
        let free_at = text.find("costs nothing").expect(&text);
        let lanes_at = text.find("target-tippy/").expect(&text);
        assert!(free_at < lanes_at, "free bytes must be named first: {text}");
        assert!(text.contains("incremental"), "{text}");
        assert!(text.contains("CARGO_INCREMENTAL=0"), "{text}");
        assert!(text.contains("Only if that is not enough"), "{text}");

        // In place (no snapshot) there is no second tree, so nothing is claimed.
        let in_place = remedy(&root, 40 * GIB, None);
        assert!(!in_place.contains("costs nothing"), "{in_place}");
        assert!(in_place.contains("regenerable"), "{in_place}");

        // A caller with no incremental caches says nothing about them either.
        let bare = crate::mktemp_dir("atv-disk-bare").expect("mktemp");
        let quiet = remedy(&root, 40 * GIB, Some(&bare));
        assert!(!quiet.contains("costs nothing"), "{quiet}");

        for d in [caller, root, bare] {
            std::fs::remove_dir_all(&d).ok();
        }
    }

    /// The live read on this machine: a real number on a real volume, or a
    /// named reason — never a panic and never a silent zero.
    #[test]
    fn the_live_read_answers_with_bytes_or_with_a_reason() {
        match read_free(Path::new(".")) {
            Reading::Free(n) => assert!(n > 0, "df read zero bytes free for the cwd's volume"),
            Reading::Unknown(why) => assert!(!why.is_empty()),
        }
        let gone = read_free(Path::new("/no/such/dir/anywhere"));
        assert!(
            matches!(gone, Reading::Unknown(ref why) if why.contains("df -Pk")),
            "{gone:?}"
        );
    }
}
