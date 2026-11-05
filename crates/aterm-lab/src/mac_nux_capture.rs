// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `first-launch`'s milestone captures, the lane's side: the guest helper's `captures` follows
//! the row's records and takes the app's window through the guest's own `aterm ctl image` at
//! each milestone they name (`src/guest/captures.rs`: each stage's start, each person stage's
//! dialog up and gone, each card `setup=` names and clears, a relaunch); the lane takes its own
//! (first present, setup's end) with the helper's `capture` ([`Captures::now`]); then every frame
//! is copied back here with a contact sheet ([`Captures::finish`]), into the run's frames folder
//! ([`frames_dir`]). The design's landing gate asks for them
//! (`docs/DESIGN-init-on-by-default-2026-10-25.md` §4.9, check 3).

use std::path::{Path, PathBuf};

use super::{GUEST_HELPER, GUEST_NUX, Guest, PROCS_RECORD, Row, SETUP_READINGS, Value, squeeze};
use crate::Fail;
use crate::mac::STEP;

/// The capture record, in [`GUEST_NUX`].
const RECORD: &str = "captures.txt";
/// The guest folder the frames go to, in [`GUEST_NUX`].
const FRAMES: &str = "frames";
/// The contact sheet's file name in a row's frames folder.
pub const SHEET: &str = "contact.png";
/// A capture taken this long after what it shows came is marked late on the sheet.
const LATE_MS: u64 = 1000;

/// One capture the record names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shot {
    pub key: String,
    /// The frame's file in the guest's frames folder; `None` for a capture that failed.
    pub file: Option<String>,
    /// `<w>x<h>` in pixels ("" for a failure).
    pub size: String,
    /// When what it shows came (the record line that named it, or the lane's own moment), the
    /// guest's clock in epoch ms.
    pub trigger_ms: u64,
    /// When its capture began (the trigger for a failure).
    pub begun_ms: u64,
    /// When its line was written.
    pub done_ms: u64,
    /// What it is; for a failure, why it failed.
    pub words: String,
}

/// The `key=<ms>` word's value.
fn ms_word(word: Option<&str>, key: &str) -> Option<u64> {
    word?.strip_prefix(key)?.strip_prefix('=')?.parse().ok()
}

/// The guest helper's filename grammar: a sequence or `lane`, then a slug, then `.png`.
/// A guest record supplies data, never a shell operand or a path outside the frames directory.
fn capture_filename(file: &str) -> bool {
    let Some((prefix, key)) = file
        .strip_suffix(".png")
        .and_then(|stem| stem.split_once('-'))
    else {
        return false;
    };
    (prefix == "lane" || (prefix.len() >= 2 && prefix.bytes().all(|b| b.is_ascii_digit())))
        && key.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// The captures the record names (`capture` and `capture-error` lines), in the order what they
/// show came. An otherwise recognized capture with an invalid filename remains a missed capture,
/// with no file for the caller to use in a shell or a host path. Pure.
#[must_use]
pub fn read_shots(text: &str) -> Vec<Shot> {
    let mut out = Vec::new();
    for line in text.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some(done_ms) = words.get(1).and_then(|w| w.parse().ok()) else {
            continue;
        };
        match words.first().copied() {
            Some("capture") => {
                let (Some(key), Some(file), Some(size)) =
                    (words.get(2), words.get(3), words.get(4))
                else {
                    continue;
                };
                let (Some(trigger_ms), Some(begun_ms)) = (
                    ms_word(words.get(5).copied(), "trigger"),
                    ms_word(words.get(6).copied(), "begun"),
                ) else {
                    continue;
                };
                let admitted = capture_filename(file);
                out.push(Shot {
                    key: (*key).to_owned(),
                    file: admitted.then(|| (*file).to_owned()),
                    size: if admitted {
                        (*size).to_owned()
                    } else {
                        String::new()
                    },
                    trigger_ms,
                    begun_ms,
                    done_ms,
                    words: if admitted {
                        words[7..].join(" ")
                    } else {
                        "invalid capture filename".to_owned()
                    },
                });
            }
            Some("capture-error") => {
                let (Some(key), Some(trigger_ms)) =
                    (words.get(2), ms_word(words.get(3).copied(), "trigger"))
                else {
                    continue;
                };
                out.push(Shot {
                    key: (*key).to_owned(),
                    file: None,
                    size: String::new(),
                    trigger_ms,
                    begun_ms: trigger_ms,
                    done_ms,
                    words: words.get(4..).map(|w| w.join(" ")).unwrap_or_default(),
                });
            }
            _ => {}
        }
    }
    out.sort_by_key(|s| (s.trigger_ms, s.begun_ms));
    out
}

/// The frames the shots name, each with the shots it shows (one capture shows every milestone
/// that came while the one before it was taken), in the order their first shot came; a failed
/// shot names no frame. Pure.
#[must_use]
pub fn frames(shots: &[Shot]) -> Vec<(&str, Vec<&Shot>)> {
    let mut out: Vec<(&str, Vec<&Shot>)> = Vec::new();
    for shot in shots {
        let Some(file) = shot.file.as_deref() else {
            continue;
        };
        match out.iter_mut().find(|(f, _)| *f == file) {
            Some((_, its)) => its.push(shot),
            None => out.push((file, vec![shot])),
        }
    }
    out
}

/// Where a row's frames go on this Mac: beside the run's record (`<record stem>-frames/`), else
/// in the lab's state folder (`mac-nux/frames/`), in a folder named by the row, the run's flags
/// (`variant`, empty for none) and `stamp`. Pure.
#[must_use]
pub fn frames_dir(
    record: Option<&Path>,
    state: &Path,
    row: &str,
    variant: &str,
    stamp: &str,
) -> PathBuf {
    let root = match record {
        Some(file) => file.with_file_name(format!(
            "{}-frames",
            file.file_stem()
                .map_or_else(|| "record".into(), |s| s.to_string_lossy())
        )),
        None => state.join("mac-nux/frames"),
    };
    let name = if variant.is_empty() {
        format!("{row}-{stamp}")
    } else {
        format!("{row}-{variant}-{stamp}")
    };
    root.join(name)
}

/// The run's flags that change what a first launch meets, as a folder name's word.
#[must_use]
pub fn variant(ask: &super::Ask) -> String {
    [
        (ask.person, "person"),
        (ask.later, "later"),
        (ask.standard_user, "standard"),
        (ask.sudo_asks, "sudoasks"),
        (ask.no_clt, "noclt"),
        (ask.vanilla, "vanilla"),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .map(|(_, word)| *word)
    .collect::<Vec<_>>()
    .join("-")
}

/// The row's captures: the guest helper's `captures` following its records.
pub(super) struct Captures {
    /// The helper did not start, and why: the row's captures are NOT MEASURED.
    failed: Option<String>,
}

impl Captures {
    /// Start the helper's `captures` on the setup record's watch, the dialog census, the app's
    /// process watch and the wait's `setup=` lines, its frames folder and record made fresh, and
    /// wait until it wrote its first line ([`Guest::await_line`]).
    pub(super) fn start(g: &Guest) -> Captures {
        let started = g
            .sh(
                &format!(
                    "pkill -f '{GUEST_HELPER} captures' || true; rm -rf {GUEST_NUX}/{FRAMES}; \
                     : > {GUEST_NUX}/{RECORD}; : >> {GUEST_NUX}/{SETUP_READINGS}; \
                     ({GUEST_NUX}/{GUEST_HELPER} captures '{exe}' {GUEST_NUX}/{FRAMES} {GUEST_NUX}/{RECORD} \
                     watch={GUEST_NUX}/watch.txt census={GUEST_NUX}/census.txt procs={GUEST_NUX}/{PROCS_RECORD} \
                     setup={GUEST_NUX}/{SETUP_READINGS} </dev/null >/dev/null 2>{GUEST_NUX}/captures.err &)",
                    exe = g.aterm
                ),
                STEP,
            )
            .map_err(|f| f.msg.unwrap_or_default())
            .and_then(|_| g.await_line(&format!("{GUEST_NUX}/{RECORD}"), "captures"));
        Captures {
            failed: started.err(),
        }
    }

    /// The lane's own milestone `key` captured now (`words` say what it is): the helper's
    /// `capture`, into the same record. A capture that fails is in the record as such.
    pub(super) fn now(&self, g: &Guest, key: &str, words: &str) {
        if self.failed.is_some() {
            return;
        }
        let words: String = words
            .chars()
            .map(|c| if c == '\'' { ' ' } else { c })
            .collect();
        let _ = g.run(
            &format!(
                "{GUEST_NUX}/{GUEST_HELPER} capture '{exe}' {GUEST_NUX}/{FRAMES} {GUEST_NUX}/{RECORD} {key} '{words}'",
                exe = g.aterm
            ),
            STEP,
        );
    }

    /// Stop the helper: what comes after is no milestone.
    pub(super) fn stop(&self, g: &Guest) {
        let _ = g.run(&format!("pkill -f '{GUEST_HELPER} captures' || true"), STEP);
    }

    /// Copy each frame of the record into `dir` here (the helper stopped first), lay them out on
    /// a contact sheet ([`SHEET`]) under `title`, and say them in the row's cells, each time in ms
    /// after the open (`open_ms`, epoch ms on the guest's clock).
    pub(super) fn finish(
        self,
        g: &Guest,
        row: &mut Row,
        open_ms: u64,
        dir: &Path,
        title: &str,
    ) -> Result<(), Fail> {
        self.stop(g);
        if let Some(why) = self.failed {
            row.cell(
                "milestone captures",
                Value::Missing(format!("the guest helper's captures did not start: {why}")),
            );
            return Ok(());
        }
        let shots = read_shots(&g.read(&format!("{GUEST_NUX}/{RECORD}"))?);
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut tiles = Vec::new();
        let mut missed = Vec::new();
        let mut said = Vec::new();
        for shot in shots.iter().filter(|s| s.file.is_none()) {
            let at = shot.trigger_ms.saturating_sub(open_ms);
            missed.push(format!(
                "{} +{at} ms: {}",
                shot.key,
                squeeze(&shot.words, 120)
            ));
        }
        for (file, its) in frames(&shots) {
            let copied =
                g.vm.exec(&format!("cat '{GUEST_NUX}/{FRAMES}/{file}'"), STEP)
                    .and_then(|out| {
                        if out.status.success() && !out.stdout.is_empty() {
                            Ok(out.stdout)
                        } else {
                            Err(format!(
                                "cat: {}",
                                String::from_utf8_lossy(&out.stderr).trim()
                            ))
                        }
                    })
                    .and_then(|bytes| {
                        let here = dir.join(file);
                        crate::util::write_fresh(&here, &bytes)
                            .map_err(|e| format!("{}: {e}", here.display()))
                            .map(|()| here)
                    })
                    .and_then(|here| crate::trail_contrast::Image::read(&here));
            let frame = match copied {
                Ok(frame) => frame,
                Err(why) => {
                    for shot in &its {
                        let at = shot.trigger_ms.saturating_sub(open_ms);
                        missed.push(format!("{} +{at} ms: no frame here: {why}", shot.key));
                    }
                    continue;
                }
            };
            let mut verdicts = Vec::new();
            for (i, shot) in its.iter().enumerate() {
                let at = shot.trigger_ms.saturating_sub(open_ms);
                let lag = shot.begun_ms.saturating_sub(shot.trigger_ms);
                said.push(format!("{} +{at} ms (lag {lag} ms)", shot.key));
                if i == 0 {
                    verdicts.push((format!("+{at} ms"), true));
                    verdicts.push((format!("lag {lag} ms"), lag < LATE_MS));
                } else {
                    verdicts.push((format!("also {}", shot.key), true));
                }
            }
            let key = its.first().map_or("", |shot| shot.key.as_str());
            tiles.push(crate::system_state_sheet::Tile {
                frame,
                caption: format!("{} {key}", tiles.len() + 1),
                verdicts,
            });
        }
        let sheet = dir.join(SHEET);
        let sheet_said = if tiles.is_empty() {
            "no contact sheet (no frame)".to_owned()
        } else {
            match crate::system_state_sheet::write(
                &sheet,
                title,
                "+ms after the open; lag: from what it shows to the start of its capture (red: \
                 1 s or more); also: a milestone that came while the frame before was taken",
                &tiles,
            ) {
                Ok(()) => format!("contact sheet {}", sheet.display()),
                Err(why) => format!("no contact sheet ({why})"),
            }
        };
        println!(
            "mac nux: {} milestone frames in {}; {sheet_said}",
            tiles.len(),
            dir.display()
        );
        row.cell(
            "milestone captures",
            if said.is_empty() {
                Value::Missing(format!("no frame came back ({} named)", shots.len()))
            } else {
                Value::Text(format!(
                    "{} frames of {} milestones in {}, {sheet_said}: {}",
                    tiles.len(),
                    said.len(),
                    dir.display(),
                    said.join("; ")
                ))
            },
        );
        row.cell(
            "captures missed",
            if missed.is_empty() {
                Value::Count(0)
            } else {
                Value::Text(format!("{}: {}", missed.len(), missed.join("; ")))
            },
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_reads_back_as_each_capture_in_the_order_what_it_shows_came() {
        let text = "captures 1000 every=50\n\
                    capture 2600 stage-workstation 01-stage-workstation.png 1800x1288 trigger=2300 begun=2320 Workstation started\n\
                    capture 2200 first-present lane-first-present.png 1800x1288 trigger=2000 begun=2000 the first tab's prompt\n\
                    capture-error 14100 before-nehelper trigger=3700 aterm ctl image: ERR image: no window to capture\n\
                    capture 2700 torn\n\
                    capture x stage-spotlight 02.png 1x1 trigger=1 begun=1\n";
        let shots = read_shots(text);
        assert_eq!(
            shots
                .iter()
                .map(|s| (s.key.as_str(), s.file.as_deref(), s.trigger_ms))
                .collect::<Vec<_>>(),
            [
                ("first-present", Some("lane-first-present.png"), 2000),
                ("stage-workstation", Some("01-stage-workstation.png"), 2300),
                ("before-nehelper", None, 3700),
            ],
            "torn lines are no capture"
        );
        assert_eq!(shots[1].begun_ms, 2320);
        assert_eq!(shots[1].done_ms, 2600);
        assert_eq!(shots[1].size, "1800x1288");
        assert_eq!(shots[1].words, "Workstation started");
        assert_eq!(
            shots[2].words,
            "aterm ctl image: ERR image: no window to capture"
        );
    }

    #[test]
    fn capture_records_cannot_choose_shell_words_or_host_paths() {
        for file in [
            "../outside.png",
            "/tmp/outside.png",
            "01-nested/frame.png",
            "01-nested\\frame.png",
            "01-stage'quoted.png",
            "01-stage\"quoted.png",
            "01-stage-$(id).png",
            "01-stage-\0.png",
            "01-stage-\u{1b}.png",
            "01-stage.jpg",
            "01-stage.PNG",
            "01-stage.png.bak",
            ".png",
            "01-.png",
            "01-stage--workstation.png",
            "contact.png",
        ] {
            let shots = read_shots(&format!(
                "capture 20 first 01-stage-workstation.png 9x9 trigger=10 begun=11 valid\n\
                 capture 40 rejected {file} 9x9 trigger=30 begun=31 guest\n\
                 capture 60 last lane-first-present.png 9x9 trigger=50 begun=51 valid\n"
            ));
            assert_eq!(shots.len(), 3, "{file:?}: retain the missed capture");
            let rejected = &shots[1];
            assert!(rejected.file.is_none(), "{file:?}");
            assert!(rejected.size.is_empty(), "{file:?}");
            assert!(!rejected.words.is_empty(), "{file:?}: visible failure");
            assert_eq!(
                (rejected.trigger_ms, rejected.begun_ms, rejected.done_ms),
                (30, 31, 40)
            );
            let admitted: Vec<_> = frames(&shots).into_iter().map(|(file, _)| file).collect();
            assert_eq!(
                admitted,
                ["01-stage-workstation.png", "lane-first-present.png"],
                "{file:?}: only producer filenames reach frame copying"
            );
        }
    }

    /// A frame shows every milestone whose shot names its file; a shot of a file of its own is a
    /// frame of its own (the negative control); a failed shot is no frame.
    #[test]
    fn a_frame_shows_each_milestone_that_names_it_and_no_other() {
        let text = "capture 8700 stage-permissions-2 04-stage-permissions-2.png 9x9 trigger=5492 begun=8669 a\n\
                    capture 8700 before-nehelper 04-stage-permissions-2.png 9x9 trigger=6835 begun=8669 b\n\
                    capture 6200 stage-secrets 03-stage-secrets.png 9x9 trigger=5148 begun=6195 c\n\
                    capture-error 9900 after-nehelper trigger=9380 refused\n\
                    capture 5100 first-present lane-first-present.png 9x9 trigger=5077 begun=5077 d\n";
        let shots = read_shots(text);
        let keys = |shots: &[Shot]| -> Vec<(String, Vec<String>)> {
            frames(shots)
                .into_iter()
                .map(|(file, its)| {
                    (
                        file.to_owned(),
                        its.into_iter().map(|s| s.key.clone()).collect(),
                    )
                })
                .collect()
        };
        assert_eq!(
            keys(&shots),
            [
                ("lane-first-present.png", vec!["first-present"]),
                ("03-stage-secrets.png", vec!["stage-secrets"]),
                (
                    "04-stage-permissions-2.png",
                    vec!["stage-permissions-2", "before-nehelper"]
                ),
            ]
            .map(|(file, keys)| (
                file.to_owned(),
                keys.into_iter().map(str::to_owned).collect()
            ))
        );
        let apart: Vec<Shot> = shots
            .iter()
            .cloned()
            .map(|mut s| {
                s.file = s.file.map(|_| format!("{}.png", s.key));
                s
            })
            .collect();
        assert_eq!(
            keys(&apart).len(),
            4,
            "each shot of a file of its own is its own frame"
        );
    }

    #[test]
    fn frames_go_beside_the_record_else_in_the_state_folder() {
        let state = Path::new("/state");
        assert_eq!(
            frames_dir(
                Some(Path::new("/runs/g2-first-launch.md")),
                state,
                "first-launch",
                "person-noclt",
                "20261031-120000"
            ),
            Path::new("/runs/g2-first-launch-frames/first-launch-person-noclt-20261031-120000")
        );
        assert_eq!(
            frames_dir(None, state, "first-launch", "", "20261031-120000"),
            Path::new("/state/mac-nux/frames/first-launch-20261031-120000")
        );
    }
}
