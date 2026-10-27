// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE REFILL LAW (B09): an attach's refill offers each session every record of its lane
//! past its watermark.
//!
//! A bridge refills every session it admits from the watermark it reads for it (`seen/<sid>`,
//! where that session's addressed mail was all handled): every record of the session's lane
//! after it, up to the head the refill read to, is offered to aterm again, which takes one row
//! per offset. Lane by lane or all of a node's lanes in one read (`Bridge::refill_together`),
//! the law is the same, and it is held here against the log itself: when a refill that began
//! from a watermark ends, every record of that lane in `(watermark, head)` that the log holds
//! must have been offered, but for those below a floor retention raised (given up, said and
//! told), those it gave up, and those at or past a record aterm owes (the refill stops
//! there). A refill that failed is owed again and is not judged.

use std::collections::{BTreeMap, BTreeSet};

use crate::fx::RefillTap;

/// One session's refill in progress.
#[derive(Debug, Default)]
pub(crate) struct Mark {
    /// The lane's records' subject prefix, `/f/<F>/in/<node>/<sid>/`.
    lane: String,
    watermark: Option<u64>,
    offered: BTreeSet<u64>,
    floor: Option<u64>,
    owed: Option<u64>,
    /// The node inbox's oldest, when its start lay below it and it read its own lane up to there.
    below_oldest: Option<u64>,
}

/// The law's findings over a run.
#[derive(Debug, Default)]
pub(crate) struct Law {
    pub(crate) violations: Vec<String>,
    /// Refills from a watermark judged.
    pub(crate) judged: u64,
    /// Records those refills had to offer, and did.
    pub(crate) rows: u64,
    /// Reads of a node's whole inbox for several sessions together, and the sessions they read for.
    pub(crate) together: u64,
    pub(crate) together_sessions: u64,
    /// Sessions whose start lay below the node inbox's oldest, which read their own lanes up to
    /// it before the shared read.
    pub(crate) below_oldest: u64,
}

/// Apply one tap to a process's refills in progress, judging a refill that ends against
/// `log` (every visible record, `(offset, subject)`) and `gone` (what retention gave up).
pub(crate) fn tap(
    marks: &mut BTreeMap<String, Mark>,
    law: &mut Law,
    tap: RefillTap<'_>,
    log: impl FnOnce() -> (Vec<(u64, String)>, BTreeSet<u64>),
) {
    match tap {
        RefillTap::Began {
            sid,
            lane,
            watermark,
        } => {
            marks.insert(
                sid.to_string(),
                Mark {
                    lane: lane.trim_end_matches('>').to_string(),
                    watermark,
                    ..Mark::default()
                },
            );
        }
        RefillTap::Offered { sid, off, owed } => {
            if let Some(m) = marks.get_mut(sid) {
                m.offered.insert(off);
                if owed {
                    m.owed = Some(m.owed.map_or(off, |o| o.min(off)));
                }
            }
        }
        RefillTap::Floor { sid, floor } => {
            if let Some(m) = marks.get_mut(sid) {
                m.floor = Some(m.floor.map_or(floor, |f| f.max(floor)));
            }
        }
        RefillTap::Dropped { sid } => {
            marks.remove(sid);
        }
        RefillTap::Together { sessions } => {
            law.together += 1;
            law.together_sessions += sessions as u64;
        }
        RefillTap::BelowTheOldest { sid, oldest } => {
            law.below_oldest += 1;
            if let Some(m) = marks.get_mut(sid) {
                m.below_oldest = Some(oldest);
            }
        }
        RefillTap::Ended { sid, head } => {
            let Some(m) = marks.remove(sid) else {
                law.violations.push(format!(
                    "refill: {sid}'s refill ended at {head} without beginning"
                ));
                return;
            };
            let Some(watermark) = m.watermark else {
                return;
            };
            law.judged += 1;
            let (records, gone) = log();
            let from = watermark.saturating_add(1).max(m.floor.unwrap_or(0));
            let to = head.min(m.owed.unwrap_or(u64::MAX));
            let missed: Vec<u64> = records
                .iter()
                .filter(|(off, subject)| {
                    subject.starts_with(&m.lane) && (from..to).contains(off) && !gone.contains(off)
                })
                .map(|(off, _)| *off)
                .filter(|off| {
                    law.rows += 1;
                    !m.offered.contains(off)
                })
                .collect();
            if !missed.is_empty() {
                law.violations.push(format!(
                    "refill: {sid}'s refill from watermark @{watermark} to head @{head} (floor \
                     {:?}, owed {:?}, below the inbox's oldest {:?}) did not offer its lane's \
                     records {missed:?}",
                    m.floor, m.owed, m.below_oldest
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(records: &[(u64, &str)], gone: &[u64]) -> (Vec<(u64, String)>, BTreeSet<u64>) {
        (
            records
                .iter()
                .map(|(o, s)| (*o, (*s).to_string()))
                .collect(),
            gone.iter().copied().collect(),
        )
    }

    const A: &str = "/f/x/in/n-1/s-a/n-2/note";
    const B: &str = "/f/x/in/n-1/s-b/n-2/note";

    fn began(marks: &mut BTreeMap<String, Mark>, law: &mut Law, sid: &str, w: Option<u64>) {
        let lane = format!("/f/x/in/n-1/{sid}/>");
        tap(
            marks,
            law,
            RefillTap::Began {
                sid,
                lane: &lane,
                watermark: w,
            },
            || unreachable!("a beginning reads no log"),
        );
    }

    /// The law takes a refill that offered its lane past the watermark, and names each record
    /// one skipped: another lane's, the watermark's own, a given-up one, one below a floor, one
    /// past what aterm owes and one at or past the head are not its to offer.
    #[test]
    fn a_refill_is_held_to_its_lanes_records_past_the_watermark() {
        let records = [
            (3, A),
            (4, A),
            (5, B),
            (6, A),
            (7, A),
            (8, A),
            (9, A),
            (10, A),
        ];
        let (mut marks, mut law) = (BTreeMap::new(), Law::default());
        began(&mut marks, &mut law, "s-a", Some(3));
        for off in [4, 6, 7, 8, 9] {
            tap(
                &mut marks,
                &mut law,
                RefillTap::Offered {
                    sid: "s-a",
                    off,
                    owed: false,
                },
                || unreachable!(),
            );
        }
        tap(
            &mut marks,
            &mut law,
            RefillTap::Ended {
                sid: "s-a",
                head: 10,
            },
            || log(&records, &[]),
        );
        assert!(law.violations.is_empty(), "{:?}", law.violations);
        assert_eq!((law.judged, law.rows), (1, 5));

        began(&mut marks, &mut law, "s-a", Some(3));
        for off in [4, 8] {
            tap(
                &mut marks,
                &mut law,
                RefillTap::Offered {
                    sid: "s-a",
                    off,
                    owed: false,
                },
                || unreachable!(),
            );
        }
        tap(
            &mut marks,
            &mut law,
            RefillTap::Ended {
                sid: "s-a",
                head: 10,
            },
            || log(&records, &[7]),
        );
        assert_eq!(law.violations.len(), 1);
        assert!(
            law.violations[0].contains("records [6, 9]"),
            "{:?}",
            law.violations
        );

        let mut law = Law::default();
        began(&mut marks, &mut law, "s-a", Some(3));
        for (tapped, off) in [(true, 6), (false, 7), (true, 8)] {
            let t = if tapped {
                RefillTap::Floor {
                    sid: "s-a",
                    floor: off,
                }
            } else {
                RefillTap::Offered {
                    sid: "s-a",
                    off,
                    owed: true,
                }
            };
            tap(&mut marks, &mut law, t, || unreachable!());
        }
        tap(
            &mut marks,
            &mut law,
            RefillTap::Ended {
                sid: "s-a",
                head: 10,
            },
            || log(&records, &[]),
        );
        assert!(
            law.violations.is_empty(),
            "below the floor at 8 and past the owed record at 7 nothing is owed: {:?}",
            law.violations
        );
    }

    /// No watermark, no judgement: the refill's start is arbitrary there. A dropped refill is
    /// never judged, and an end with no beginning is a violation of its own.
    #[test]
    fn only_a_refill_from_a_watermark_that_ends_is_judged() {
        let (mut marks, mut law) = (BTreeMap::new(), Law::default());
        began(&mut marks, &mut law, "s-a", None);
        tap(
            &mut marks,
            &mut law,
            RefillTap::Ended {
                sid: "s-a",
                head: 10,
            },
            || unreachable!("no watermark reads no log"),
        );
        began(&mut marks, &mut law, "s-b", Some(1));
        tap(
            &mut marks,
            &mut law,
            RefillTap::Dropped { sid: "s-b" },
            || unreachable!(),
        );
        assert_eq!((law.judged, law.violations.len()), (0, 0));
        tap(
            &mut marks,
            &mut law,
            RefillTap::Ended {
                sid: "s-b",
                head: 10,
            },
            || unreachable!(),
        );
        assert_eq!(law.violations.len(), 1, "{:?}", law.violations);
    }
}
