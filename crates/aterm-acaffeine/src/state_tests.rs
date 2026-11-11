// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::model::HOUR;

const NOW: Clocks = Clocks {
    since_boot: 300_000,
    wall: 1_790_000_000,
};
const DAY: u64 = 24 * HOUR;
const BOOT: u128 = 0x4D5A_1B2C_3E4F_5061_7283_94A5_B6C7_D8E9;

#[test]
fn saved_on_reads_the_mode_as_written_not_as_restored() {
    let dir = crate::test_scratch_dir("state");
    let file = dir.path().join("acaffeine.state");
    assert!(!saved_on(&file, NOW), "no file");
    assert_eq!(
        load(&file, NOW),
        Mode::Forever,
        "no file, which a restore reads as forever"
    );
    let at = |mode, boot| {
        std::fs::write(&file, encode(&Saved { mode, boot }, NOW)).expect("write");
        saved_on(&file, NOW)
    };
    assert!(!at(Mode::Off, Some(BOOT)));
    assert!(!at(Mode::Off, None), "an off with no boot");
    assert_eq!(load(&file, NOW), Mode::Off, "as a restore reads it too");
    assert!(at(Mode::Forever, None));
    assert!(at(Mode::Forever, Some(BOOT)));
    let span = |deadline| Mode::Until {
        deadline,
        span: HOUR,
    };
    assert!(at(span(NOW.since_boot + HOUR), Some(BOOT)));
    assert!(
        !at(span(NOW.since_boot), Some(BOOT)),
        "a span that has ended"
    );
}

/// Keep Awake with Lid Closed round-trips through its file; a missing file, an older
/// build's state (which named no lid), a mode file and any other word read off.
/// NEGATIVE CONTROL: the `on` line itself reads on, so the off answers are no constant.
#[test]
fn the_lid_choice_round_trips_and_anything_else_is_off() {
    for on in [false, true] {
        assert_eq!(decode_lid(&encode_lid(on)), on, "{on}");
    }
    assert_eq!(encode_lid(true), "lid=on\n");
    assert_eq!(encode_lid(false), "lid=off\n");
    let old = encode(
        &Saved {
            mode: Mode::Forever,
            boot: Some(BOOT),
        },
        NOW,
    );
    for text in [
        "",
        old.as_str(),
        "lid=yes\n",
        "lid=On\n",
        "lid=on \nlid=off\n",
        "lid\n",
        "#lid=on\n",
    ] {
        assert!(!decode_lid(text), "{text:?}");
    }
    assert!(decode_lid("lid=off\nlid=on\n"), "the last line says it");
    assert!(decode_lid("  lid=on  \n"), "CONTROL");
    let dir = crate::test_scratch_dir("lid");
    let file = dir.path().join("state").join("acaffeine.lid");
    assert!(
        !load_lid(&file),
        "no file: an install from before the choice"
    );
    save_lid(&file, true).expect("save on");
    assert!(load_lid(&file));
    save_lid(&file, false).expect("save off");
    assert!(!load_lid(&file));
    std::fs::write(&file, old).expect("an old file");
    assert!(!load_lid(&file));
}

#[test]
fn every_mode_round_trips() {
    for mode in [
        Mode::Off,
        Mode::Forever,
        Mode::Until {
            deadline: NOW.since_boot + HOUR,
            span: HOUR,
        },
    ] {
        for boot in [None, Some(BOOT)] {
            let saved = Saved { mode, boot };
            assert_eq!(decode(&encode(&saved, NOW), NOW), Some(saved), "{saved:?}");
        }
    }
}

#[test]
fn the_text_is_the_documented_lines() {
    let saved = Saved {
        mode: Mode::Until {
            deadline: 303_000,
            span: 3600,
        },
        boot: Some(BOOT),
    };
    assert_eq!(
        encode(&saved, NOW),
        "mode=until\nends=303000\nspan=3600\ndeadline=1790003000\nboot=4D5A1B2C-3E4F-5061-7283-94A5B6C7D8E9\n"
    );
}

#[test]
fn a_boot_is_a_uuid_and_nothing_else() {
    assert_eq!(boot_text(BOOT), "4D5A1B2C-3E4F-5061-7283-94A5B6C7D8E9");
    assert_eq!(boot_text(1), "00000000-0000-0000-0000-000000000001");
    for boot in [1, BOOT, u128::MAX] {
        assert_eq!(parse_boot(&boot_text(boot)), Some(boot), "{boot:x}");
    }
    assert_eq!(
        parse_boot("4d5a1b2c-3e4f-5061-7283-94a5b6c7d8e9"),
        Some(BOOT),
        "either case"
    );
    for bad in [
        "",
        "1789990000",
        "4D5A1B2C-3E4F-5061-7283-94A5B6C7D8E",
        "4D5A1B2C-3E4F-5061-7283-94A5B6C7D8E9A",
        "+D5A1B2C-3E4F-5061-7283-94A5B6C7D8E9",
        "4D5A1B2C-3E4F-5061-7283-94A5B6C7D8EG",
        "00000000-0000-0000-0000-000000000000",
    ] {
        assert_eq!(parse_boot(bad), None, "{bad:?}");
    }
    assert_eq!(decode("mode=forever\nboot=1789990000\n", NOW), None);
    let zero = format!("mode=off\nboot={}\n", boot_text(0));
    assert_eq!(decode(&zero, NOW), None);
    assert_eq!(restore(decode(&zero, NOW), NOW), Mode::Forever);
    let this = format!("mode=off\nboot={}\n", boot_text(BOOT));
    assert_eq!(restore(decode(&this, NOW), NOW), Mode::Off, "CONTROL");
}

#[test]
fn a_file_this_crate_did_not_write_is_none() {
    for text in [
        "",
        "mode=on\n",
        "mode=until\nspan=3600\n",
        "mode=until\ndeadline=x\nspan=3600\n",
        "mode=until\nends=x\nspan=3600\n",
        "mode=until\nends=303000\n",
        "mode=forever\nboot=-1\n",
        "garbage\n",
        "mode\n",
    ] {
        assert_eq!(decode(text, NOW), None, "{text:?}");
    }
    assert_eq!(
        decode("# note\n\nmode=forever\nflavour=mocha\n", NOW),
        Some(Saved {
            mode: Mode::Forever,
            boot: None
        })
    );
}

/// Missing or unreadable state is forever; a saved mode is restored as saved, settled: an off
/// lasts, through a restart too, and a span that has ended is off, not forever.
#[test]
fn missing_state_is_forever_an_off_lasts_and_an_ended_span_is_off() {
    assert_eq!(restore(None, NOW), Mode::Forever);
    for boot in [Some(BOOT), Some(BOOT ^ 1), None] {
        let saved = |mode| Some(Saved { mode, boot });
        assert_eq!(
            restore(saved(Mode::Forever), NOW),
            Mode::Forever,
            "{boot:?}"
        );
        assert_eq!(restore(saved(Mode::Off), NOW), Mode::Off, "{boot:?}");
        let timed = Mode::Until {
            deadline: NOW.since_boot + 10,
            span: HOUR,
        };
        assert_eq!(restore(saved(timed), NOW), timed, "{boot:?}");
        assert_eq!(restore(saved(timed), NOW + 10), Mode::Off, "{boot:?}");
        let nothing = Mode::Until {
            deadline: NOW.since_boot + HOUR,
            span: 0,
        };
        assert_eq!(
            restore(saved(nothing), NOW),
            Mode::Off,
            "a hand-edited span of nothing ends as it starts: {boot:?}"
        );
    }
    let dir = crate::test_scratch_dir("state");
    let path = dir.path().join("acaffeine.state");
    let earlier = |mode: &str| format!("mode={mode}\nboot={}\n", boot_text(BOOT ^ 1));
    std::fs::write(&path, earlier("off")).expect("an earlier boot's off");
    assert_eq!(load(&path, NOW), Mode::Off, "an off outlasts a restart");
    std::fs::write(&path, earlier("forever")).expect("an earlier boot's on");
    assert_eq!(load(&path, NOW), Mode::Forever);
}

/// An `ends` counts on the boot clock of the file's own boot: a span saved in an earlier boot
/// (or where the boot cannot be read) is read on the wall clock, never as more than its span
/// away, so a restart neither ends it early nor keeps it past its end.
#[test]
fn a_span_from_an_earlier_boot_is_read_on_the_wall_clock() {
    let text = |ends: u64, deadline: u64, boot: u128| {
        format!(
            "mode=until\nends={ends}\nspan={HOUR}\ndeadline={deadline}\nboot={}\n",
            boot_text(boot)
        )
    };
    let read = |text: &str, this: Option<u128>| {
        parse(text, NOW, |saved| this.is_some() && saved == this)
            .map(|(saved, wall_only)| (restore(Some(saved), NOW), wall_only))
    };
    let half_an_hour_on_its_clock = text(NOW.since_boot + 1800, NOW.wall + 600, BOOT);
    assert_eq!(
        read(&half_an_hour_on_its_clock, Some(BOOT)),
        Some((
            Mode::Until {
                deadline: NOW.since_boot + 1800,
                span: HOUR
            },
            false
        )),
        "CONTROL: this boot's `ends`"
    );
    for this in [Some(BOOT ^ 1), None] {
        assert_eq!(
            read(&half_an_hour_on_its_clock, this),
            Some((
                Mode::Until {
                    deadline: NOW.since_boot + 600,
                    span: HOUR
                },
                true
            )),
            "{this:?}"
        );
        let gone = text(NOW.since_boot + 1800, NOW.wall - 1, BOOT);
        assert_eq!(read(&gone, this), Some((Mode::Off, true)), "{this:?}");
    }
}

/// A saved span keeps its time through the wall clock set back or forward a day, and the file
/// keeps its end on the wall clock in `deadline`.
#[test]
fn a_saved_span_keeps_its_time_through_a_change_of_the_date() {
    let saved = Saved {
        mode: Mode::chosen(crate::model::Choice::Hours1, NOW),
        boot: Some(BOOT),
    };
    let text = encode(&saved, NOW);
    assert!(text.contains("\ndeadline=1790003600\n"), "{text}");
    for wall in [NOW.wall - DAY + 600, NOW.wall + 600, NOW.wall + DAY + 600] {
        let later = Clocks {
            since_boot: NOW.since_boot + 600,
            wall,
        };
        let mode = restore(decode(&text, later), later);
        assert_eq!(mode.remaining(later), Some(HOUR - 600), "{wall}");
    }
}

/// A file with no `ends` is read against the wall clock now, never as more than its span away,
/// and from then on runs on the boot clock.
#[test]
fn a_file_with_no_ends_restores_bounded_by_its_span() {
    let earlier = |deadline: u64| {
        format!(
            "mode=until\ndeadline={deadline}\nspan=3600\nboot={}\n",
            boot_text(BOOT)
        )
    };
    let read = |text: &str| decode(text, NOW).map(|saved| saved.mode);
    let left = |text: &str| restore(decode(text, NOW), NOW).remaining(NOW);
    let ten_minutes_in = earlier(NOW.wall + HOUR - 600);
    assert_eq!(left(&ten_minutes_in), Some(HOUR - 600));
    let set_back = earlier(NOW.wall + DAY + HOUR - 600);
    assert_eq!(
        read(&set_back),
        Some(Mode::chosen(crate::model::Choice::Hours1, NOW)),
        "a clock set back a day after the save"
    );
    assert_eq!(left(&set_back), Some(HOUR));
    assert_eq!(left(&earlier(NOW.wall - 1)), None, "a deadline gone");
    let mode = restore(decode(&ten_minutes_in, NOW), NOW);
    let back = Clocks {
        since_boot: NOW.since_boot + 60,
        wall: NOW.wall - DAY + 60,
    };
    assert_eq!(mode.remaining(back), Some(HOUR - 660));
    let saved = encode(
        &Saved {
            mode,
            boot: Some(BOOT),
        },
        NOW,
    );
    assert!(saved.contains(&format!("\nends={}\n", NOW.since_boot + HOUR - 600)));
}

/// A file with no `ends` is one to save back, whatever its boot: re-read after the date is set
/// back it would read a full span, while the mode saved reads the time it had. So is one whose
/// `ends` counts on an earlier boot's clock; this boot's `ends` is read as it is.
#[test]
fn a_file_with_no_ends_is_one_to_save_back() {
    let earlier = |deadline: u64| {
        format!(
            "mode=until\ndeadline={deadline}\nspan={}\nboot={}\n",
            8 * HOUR,
            boot_text(BOOT)
        )
    };
    let wall_only =
        |text: &str, at: Clocks| parse(text, at, |_| true).map(|(_, wall_only)| wall_only);
    let left = |text: &str, at: Clocks| restore(decode(text, at), at).remaining(at);
    let back = Clocks {
        since_boot: NOW.since_boot + 600,
        wall: NOW.wall - DAY + 600,
    };
    for (deadline, left_now) in [(NOW.wall + HOUR, Some(HOUR)), (NOW.wall - 1, None)] {
        let text = earlier(deadline);
        assert_eq!(wall_only(&text, NOW), Some(true), "{deadline}");
        assert_eq!(left(&text, NOW), left_now, "{deadline}");
        assert_eq!(left(&text, back), Some(8 * HOUR), "{deadline}: read anew");
        let saved = encode(
            &Saved {
                mode: restore(decode(&text, NOW), NOW),
                boot: Some(BOOT),
            },
            NOW,
        );
        assert_eq!(wall_only(&saved, back), Some(false), "{deadline}");
        assert_eq!(
            left(&saved, back),
            left_now.map(|left| left - 600),
            "{deadline}"
        );
    }
    for text in ["mode=off\n", "mode=forever\n"] {
        assert_eq!(wall_only(text, NOW), Some(false), "{text:?}");
    }
    let dir = crate::test_scratch_dir("state");
    let path = dir.path().join("acaffeine.state");
    assert_eq!(take(&path, NOW), (Mode::Forever, None), "no file");
    std::fs::write(&path, earlier(NOW.wall + HOUR)).expect("a file with no ends");
    assert_eq!(take(&path, NOW).1, Some(earlier(NOW.wall + HOUR)));
    let this = crate::instance::boot_id();
    let other = this.map_or(BOOT, |this| this ^ 1);
    for (boot, resaved) in [(this, this.is_none()), (Some(other), true)] {
        write(
            &path,
            &Saved {
                mode: Mode::chosen(crate::model::Choice::Hours8, NOW),
                boot,
            },
            NOW,
        )
        .expect("write");
        assert_eq!(take(&path, NOW).1.is_some(), resaved, "{boot:?}");
    }
}

/// A file of this boot whose restore changes its mode is saved back once: a deadline a day
/// away comes in to its hour, and one gone is off. One past its span by no more than
/// [`DRIFT_SECS`] is brought in but left as it is. An earlier boot's span is read on the wall
/// clock, its hour, and saved back on this boot's.
#[cfg(target_os = "macos")]
#[test]
fn a_restore_that_changes_this_boots_mode_is_saved_back() {
    let this = crate::instance::boot_id().expect("this boot");
    let file = |ends: u64, boot: u128| {
        format!(
            "mode=until\nends={ends}\nspan={HOUR}\ndeadline={}\nboot={}\n",
            NOW.wall_at(ends),
            boot_text(boot)
        )
    };
    let dir = crate::test_scratch_dir("state");
    let path = dir.path().join("acaffeine.state");
    let hour = Mode::chosen(crate::model::Choice::Hours1, NOW);
    for (ends, boot, want, resaved) in [
        (NOW.since_boot + DAY, this, hour, true),
        (NOW.since_boot - 1, this, Mode::Off, true),
        (NOW.since_boot + HOUR, this, hour, false),
        (NOW.since_boot + HOUR + DRIFT_SECS, this, hour, false),
        (NOW.since_boot + HOUR + DRIFT_SECS + 1, this, hour, true),
        (NOW.since_boot + DAY, this ^ 1, hour, true),
    ] {
        let text = file(ends, boot);
        std::fs::write(&path, &text).expect("state file");
        assert_eq!(
            take(&path, NOW),
            (want, resaved.then_some(text)),
            "{ends} {boot:x}"
        );
    }
    std::fs::write(&path, file(NOW.since_boot + DAY, this)).expect("state file");
    assert_eq!(take_saving(&path, NOW).expect("saved back"), hour);
    let later = NOW + HOUR / 2;
    assert_eq!(load(&path, later).remaining(later), Some(HOUR / 2));
    assert_eq!(take(&path, later).1, None, "saved once");
}

/// A writer's change landing before the save-back is kept; a writer landing at every
/// save-back ends the takes at the last of [`TAKE_ROUNDS`], unsaved.
#[cfg(target_os = "macos")]
#[test]
fn a_writers_change_landing_before_the_save_back_is_kept() {
    let this = boot_text(crate::instance::boot_id().expect("this boot"));
    let earlier = |deadline: u64| {
        format!(
            "mode=until\ndeadline={deadline}\nspan={}\nboot={this}\n",
            8 * HOUR
        )
    };
    let dir = crate::test_scratch_dir("state");
    let path = dir.path().join("acaffeine.state");
    std::fs::write(&path, earlier(NOW.wall + HOUR)).expect("a file with no ends");
    let mut landed = 0;
    let taken = take_saving_with(&path, NOW, |path| {
        landed += 1;
        save_at(path, Mode::Off, NOW).expect("the writer's off");
    })
    .expect("taken");
    assert_eq!(taken, Mode::Off);
    assert_eq!(landed, 1, "the writer's file needs no save-back");
    assert_eq!(
        std::fs::read_to_string(&path).expect("state"),
        format!("mode=off\nboot={this}\n")
    );
    std::fs::write(&path, earlier(NOW.wall + HOUR)).expect("a file with no ends");
    let mut landed = 0;
    let taken = take_saving_with(&path, NOW, |path| {
        landed += 1;
        std::fs::write(path, earlier(NOW.wall + HOUR - landed)).expect("another");
    })
    .expect("taken");
    assert_eq!(landed, TAKE_ROUNDS as u64);
    assert_eq!(taken.remaining(NOW), Some(HOUR - (landed - 1)));
    assert_eq!(
        std::fs::read_to_string(&path).expect("state"),
        earlier(NOW.wall + HOUR - landed)
    );
}

/// A writer's `on 1h` landing before a save-back, on clocks up to [`DRIFT_SECS`] past the
/// take's, is left as written; further past, the take's hour is saved in.
#[cfg(target_os = "macos")]
#[test]
fn a_writers_span_taken_at_clocks_behind_its_own_is_left_as_written() {
    let this = boot_text(crate::instance::boot_id().expect("this boot"));
    let earlier = format!(
        "mode=until\ndeadline={}\nspan={}\nboot={this}\n",
        NOW.wall + HOUR,
        8 * HOUR
    );
    let dir = crate::test_scratch_dir("state");
    let path = dir.path().join("acaffeine.state");
    let hour = Mode::chosen(crate::model::Choice::Hours1, NOW);
    for lead in [0, 1, 2, DRIFT_SECS, DRIFT_SECS + 1] {
        std::fs::write(&path, &earlier).expect("a file with no ends");
        let writers = NOW + lead;
        let on = Mode::chosen(crate::model::Choice::Hours1, writers);
        let (mut landed, mut text) = (0, String::new());
        let taken = take_saving_with(&path, NOW, |path| {
            landed += 1;
            if landed == 1 {
                save_at(path, on, writers).expect("the writer's on 1h");
                text = std::fs::read_to_string(path).expect("the writer's file");
            }
        })
        .expect("taken");
        assert_eq!(taken, hour, "{lead}");
        let after = std::fs::read_to_string(&path).expect("state");
        if lead <= DRIFT_SECS {
            assert_eq!(after, text, "{lead}");
            assert_eq!(load(&path, writers), on, "{lead}");
        } else {
            assert_eq!(after, encode(&this_boots(hour), NOW), "{lead}");
        }
    }
}

/// A save holds its directory from the flush over its last read and its rename, then lets go;
/// with the hold taken elsewhere, a save whose wait is up renames without it.
#[cfg(unix)]
#[test]
fn a_save_renames_under_its_directorys_hold() {
    let dir = crate::test_scratch_dir("state");
    let path = dir.path().join("acaffeine.state");
    let free = || hold(dir.path(), Duration::ZERO).is_some();
    let forever = Saved {
        mode: Mode::Forever,
        boot: Some(BOOT),
    };
    let (mut at_landing, mut at_rename) = (false, true);
    let written = write_unless(
        &path,
        &forever,
        NOW,
        HOLD_WAIT,
        || at_landing = free(),
        || {
            at_rename = free();
            false
        },
    );
    assert!(written.expect("written"));
    assert!(at_landing, "CONTROL: free before the hold");
    assert!(!at_rename, "held over the last read and the rename");
    assert!(free(), "let go once saved");
    let off = Saved {
        mode: Mode::Off,
        ..forever
    };
    let elsewhere = hold(dir.path(), Duration::ZERO).expect("free");
    let written = write_unless(&path, &off, NOW, Duration::ZERO, || {}, || false);
    assert!(written.expect("written without it"));
    drop(elsewhere);
    assert_eq!(read(&path, NOW), Some(off));
}

/// A hold another description has is waited for while it lasts, and no longer than the wait.
#[cfg(unix)]
#[test]
fn a_hold_waits_for_its_holder_and_no_longer() {
    let dir = crate::test_scratch_dir("state");
    let mut elsewhere = Some(hold(dir.path(), Duration::ZERO).expect("free"));
    let none = hold_with(dir.path(), Duration::ZERO, || {
        panic!("a pause with no wait")
    });
    assert!(none.is_none(), "no wait");
    let mut pauses = 0;
    let held = hold_with(dir.path(), Duration::from_secs(60), || {
        pauses += 1;
        elsewhere = None;
    });
    assert!(held.is_some(), "taken once its holder let go");
    assert_eq!(pauses, 1);
    assert!(hold(dir.path(), Duration::ZERO).is_none(), "and held");
    let mut max_elsewhere = held;
    let taken_max = hold_with(dir.path(), Duration::MAX, || {
        max_elsewhere = None;
    });
    assert!(
        taken_max.is_some(),
        "Duration::MAX wait never overflows Instant"
    );
    let file = dir.path().join("not-a-dir");
    std::fs::write(&file, b"file").expect("file");
    assert!(hold_dir(&file, Duration::ZERO, || {}).is_err());
    let link = dir.path().join("dir-link");
    let real = dir.path().join("real-dir");
    std::fs::create_dir(&real).expect("real-dir");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    assert!(hold_dir(&link, Duration::ZERO, || {}).is_err());
}

/// A refresh reads the file only for a timed mode: off, and on with no end, keep no end on
/// the wall clock.
#[test]
fn a_refresh_reads_the_file_only_for_a_timed_mode() {
    let dir = crate::test_scratch_dir("state");
    let path = dir.path().join("acaffeine.state");
    let hour = Mode::chosen(crate::model::Choice::Hours1, NOW);
    for (mode, want) in [(Mode::Off, 0), (Mode::Forever, 0), (hour, 1)] {
        save_at(&path, mode, NOW).expect("saved");
        let mut reads = 0;
        let read = |path: &Path| {
            reads += 1;
            read_text(path)
        };
        let saved = refresh_with(&path, mode, NOW, read, || {});
        assert!(!saved.expect("refreshed"), "{mode:?}");
        assert_eq!(reads, want, "{mode:?}");
    }
}

/// A refresh saves again only the mug's own timed mode of this boot whose `deadline` is
/// missing or more than [`DRIFT_SECS`] from where the wall clock now puts its end: drift up
/// to that is two readings of the clocks, and a file of another boot, with no boot, or
/// holding another mode (a writer's, or the mug's own with its `mode` edited by hand) is not
/// the mug's save.
#[test]
fn only_the_mugs_own_save_drifted_past_its_margin_is_saved_again() {
    let hour = Mode::chosen(crate::model::Choice::Hours1, NOW);
    let text = |mode, boot| encode(&Saved { mode, boot }, NOW);
    let ours = text(hour, Some(BOOT));
    let at = |off| Clocks {
        wall: NOW.wall + off,
        ..NOW
    };
    assert!(!drifted(&ours, hour, Some(BOOT), NOW));
    assert!(!drifted(&ours, hour, Some(BOOT), at(DRIFT_SECS)));
    assert!(drifted(&ours, hour, Some(BOOT), at(DRIFT_SECS + 1)));
    let no_deadline: String = ours
        .lines()
        .filter(|line| !line.starts_with("deadline="))
        .map(|line| format!("{line}\n"))
        .collect();
    assert!(drifted(&no_deadline, hour, Some(BOOT), NOW));
    for (file, boot) in [
        (text(hour, Some(BOOT ^ 1)), Some(BOOT)),
        (text(hour, None), Some(BOOT)),
        (text(hour, None), None),
        (ours.clone(), None),
        (
            text(
                Mode::chosen(crate::model::Choice::Hours1, NOW + 60),
                Some(BOOT),
            ),
            Some(BOOT),
        ),
        (
            text(
                Mode::Until {
                    deadline: NOW.since_boot + HOUR,
                    span: 2 * HOUR,
                },
                Some(BOOT),
            ),
            Some(BOOT),
        ),
        (text(Mode::Forever, Some(BOOT)), Some(BOOT)),
        (ours.replacen("mode=until", "mode=off", 1), Some(BOOT)),
    ] {
        assert!(!drifted(&file, hour, boot, at(DAY)), "{file:?} {boot:?}");
    }
}

/// A writer whose change lands before a refresh's save keeps it: the mode the mug holds,
/// saved before the date was set back a day, is not saved again over the writer's off.
#[cfg(target_os = "macos")]
#[test]
fn a_writers_change_landing_before_a_refresh_is_kept() {
    let this = boot_text(crate::instance::boot_id().expect("this boot"));
    let dir = crate::test_scratch_dir("state");
    let path = dir.path().join("acaffeine.state");
    let mode = Mode::chosen(crate::model::Choice::Hours8, NOW);
    save_at(&path, mode, NOW).expect("the mug's save");
    let back = Clocks {
        since_boot: NOW.since_boot + 600,
        wall: NOW.wall - DAY + 600,
    };
    let mut landed = 0;
    let saved = refresh_with(&path, mode, back, read_text, || {
        landed += 1;
        save_at(&path, Mode::Off, back).expect("the writer's off");
    });
    assert!(!saved.expect("refreshed"));
    assert_eq!(
        std::fs::read_to_string(&path).expect("state"),
        format!("mode=off\nboot={this}\n")
    );
    assert_eq!(landed, 1);
}

/// The boot clock counts time asleep: [`clocks`] reads between two direct reads of
/// `continuous_ms`, so it is neither the wall clock nor one that stops in sleep.
#[cfg(target_os = "macos")]
#[test]
fn the_boot_clock_counts_time_asleep() {
    let direct = || aterm_uds::process::continuous_ms().expect("the continuous clock") / 1000;
    let wall = || aterm_time::unix_secs().expect("after the epoch");
    let (before, wall_before) = (direct(), wall());
    let now = clocks().expect("both clocks");
    let (after, wall_after) = (direct(), wall());
    assert!(
        (before..=after).contains(&now.since_boot),
        "{before} {now:?} {after}"
    );
    assert!(
        (wall_before..=wall_after).contains(&now.wall),
        "{wall_before} {now:?} {wall_after}"
    );
}

#[test]
fn write_then_read_and_an_oversized_file_reads_as_none() {
    let dir = crate::test_scratch_dir("state");
    let path = dir.path().join("nested").join("acaffeine.state");
    assert_eq!(read(&path, NOW), None);
    let saved = Saved {
        mode: Mode::Forever,
        boot: Some(BOOT),
    };
    write(&path, &saved, NOW).expect("write");
    assert_eq!(read(&path, NOW), Some(saved));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let leftovers: Vec<_> = std::fs::read_dir(path.parent().expect("parent"))
        .expect("read_dir")
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(leftovers, [std::ffi::OsString::from("acaffeine.state")]);
    std::fs::write(
        &path,
        format!("mode=forever\n#{}\n", "x".repeat(READ_CAP as usize)),
    )
    .expect("big");
    assert_eq!(read(&path, NOW), None);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::{MetadataExt as _, symlink};

        let outside = dir.path().join("keep");
        std::fs::write(&outside, b"unrelated bytes").expect("outside");
        let old_stage = path.with_file_name(format!("acaffeine.state.{}.tmp", std::process::id()));
        for linked in [false, true] {
            if linked {
                std::fs::hard_link(&outside, &old_stage).expect("hardlink");
            } else {
                symlink(&outside, &old_stage).expect("symlink");
            }
            let before = std::fs::metadata(&outside).expect("outside metadata");
            write(&path, &saved, NOW).expect("write past an unowned old stage");
            assert_eq!(read(&path, NOW), Some(saved));
            assert_eq!(
                std::fs::read(&outside).expect("outside bytes"),
                b"unrelated bytes"
            );
            let after = std::fs::metadata(&outside).expect("outside metadata");
            assert_eq!(
                (after.dev(), after.ino(), after.mode(), after.len()),
                (before.dev(), before.ino(), before.mode(), before.len())
            );
            let stage = std::fs::symlink_metadata(&old_stage).expect("old stage retained");
            assert_eq!(stage.file_type().is_symlink(), !linked);
            std::fs::remove_file(&old_stage).expect("own old stage");
        }
        let alias = dir.path().join("state-link");
        symlink(&path, &alias).expect("state link");
        assert_eq!(read(&alias, NOW), None);
        assert_eq!(read(path.parent().expect("parent"), NOW), None);
        let fifo = dir.path().join("state-fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("no NUL");
        // SAFETY: the name is an exclusively owned scratch path; the call creates a FIFO.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
        assert_eq!(read(&fifo, NOW), None);
        assert_eq!(
            read(&path, NOW),
            Some(saved),
            "the ordinary state still reads"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_sweep_takes_what_cut_short_writes_left_beside_each_file_written_through_one() {
    let dir = crate::test_scratch_dir("state");
    let paths = crate::paths::Paths {
        state_dir: dir.path().join("state"),
        home: dir.path().join("home"),
        logs_dir: dir.path().join("logs"),
        local_volume: |_| true,
    };
    std::fs::create_dir_all(&paths.state_dir).expect("state dir");
    let ago = std::time::SystemTime::now() - Duration::from_secs(2 * 60 * 60);
    let plant = |name: &str, salt: &str, dated: bool| {
        let temp = paths
            .state_dir
            .join(format!(".{name}.tmp.4294967295.{salt}"));
        let file = std::fs::File::create(&temp).expect("plant");
        if dated {
            file.set_modified(ago).expect("date");
        }
        temp
    };
    let written = [
        "acaffeine.state",
        "acaffeine.lid",
        "acaffeine.bridge",
        "acaffeine.trouble",
        "caffeine.state",
        "caffeine.bridge",
    ];
    let cut_short: Vec<_> = written
        .iter()
        .map(|name| plant(name, "0123abcd", true))
        .collect();
    let young = plant("acaffeine.state", "89abcdef", false);
    let other = plant("acaffeine.lock", "0123abcd", true);
    let swept = sweep_temps(&paths);
    let files: Vec<_> = swept.iter().map(|(file, _)| file.clone()).collect();
    assert_eq!(files, written.map(|name| paths.state_dir.join(name)));
    for (file, gone) in swept {
        assert_eq!(gone.expect("swept"), 1, "{}", file.display());
    }
    for temp in &cut_short {
        assert!(!temp.exists(), "{}", temp.display());
    }
    assert!(young.exists(), "written within the hour");
    assert!(
        other.exists(),
        "the lock is written in place, so this is none of its"
    );
}
