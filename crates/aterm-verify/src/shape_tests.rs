// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The planner ([`super::shape`]) on fixed records and unit graphs: no test reads this user's
//! book or resolves a real call.

use super::*;
use crate::unitgraph::tests::graph;

const GIB: u64 = 1 << 30;
/// The run's budget on this Mac at 10:23 (21.3 GiB).
pub(crate) const BUDGET: u64 = 213 * GIB / 10;
const HERE: &str = "/Users//example/Developer/aterm-shape";
const QUIET: &str = "/Users//example/Developer/aterm-quiet-tell";
const VMEM: &str = "/Users//example/Developer/aterm-vmem";

fn names(c: &[&str]) -> Vec<String> {
    c.iter().map(|n| (*n).to_owned()).collect()
}

/// The unit graphs of [`Graphs`] whose peel holds, as a run's resolver ([`crate::Ctx::unit_graph`]).
///
/// # Errors
/// A piece the fixture has no graph for.
pub(crate) fn fixture_graph(_: &crate::Ctx, piece: &Piece) -> Result<String, String> {
    Graphs {
        peel_differs: false,
    }
    .of(piece)
}

/// 10:23's selection, S1.
pub(crate) fn s1() -> Vec<String> {
    names(&[
        "aterm-agent",
        "aterm-gui",
        "aterm-models-harness",
        "aterm-models",
        "aterm-types",
    ])
}

fn s1_less_gui() -> Vec<String> {
    names(&[
        "aterm-agent",
        "aterm-models",
        "aterm-models-harness",
        "aterm-types",
    ])
}

fn row(r: &mut Records, checkout: &str, crates: &[&str], variant: Option<&str>, gib: f64) {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a fixture's GiB, small and positive"
    )]
    let bytes = (gib * GIB as f64) as u64;
    let at = u64::try_from(r.rows().len()).unwrap_or(0) + 1;
    r.push(
        Key {
            stage: "TestCompile".to_owned(),
            checkout: checkout.to_owned(),
            crates: Some(names(crates)),
            variant: variant.map(str::to_owned),
        },
        bytes,
        at,
    );
}

fn unit(r: &mut Records, package: &str, kind: &str, gib: f64) {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a fixture's GiB, small and positive"
    )]
    let bytes = (gib * GIB as f64) as u64;
    r.push(
        unit_key(
            VMEM,
            &UnitName {
                package: package.to_owned(),
                kind: kind.to_owned(),
            },
        ),
        bytes,
        1,
    );
}

/// The book as 10:23 read it: the 5-crate selection's 15.16 GiB (09:59) in quiet-tell, `-p
/// aterm-gui`'s 17.63 GiB in aterm-vmem, the 4-crate selection's 12.72 GiB in quiet-tell.
pub(crate) fn book_at_1023() -> Records {
    let mut r = Records::default();
    row(
        &mut r,
        QUIET,
        &s1().iter().map(String::as_str).collect::<Vec<_>>(),
        None,
        15.16,
    );
    row(&mut r, VMEM, &["aterm-gui"], None, 14.96);
    row(&mut r, VMEM, &["aterm-gui"], None, 17.63);
    row(
        &mut r,
        QUIET,
        &[
            "aterm-agent",
            "aterm-models",
            "aterm-models-harness",
            "aterm-types",
        ],
        None,
        12.72,
    );
    r
}

/// Unit graphs shaped as this Mac's are (measured 2026-10-29): S1 builds aterm-cli with the
/// `cap` feature (aterm-models' dev-dependency asks for it), so `-p aterm-gui` alone, which
/// builds it without, builds units S1 does not; S1 less aterm-gui still builds aterm-gui's lib
/// (aterm-models dev-depends on it) and nothing S1 does not. aterm-models' lib is not tested.
pub(crate) struct Graphs {
    /// Whether S1 less aterm-gui builds aterm-agent with a feature S1's does not: no cut holds.
    pub(crate) peel_differs: bool,
}

impl Graphs {
    pub(crate) fn of(&self, piece: &Piece) -> Result<String, String> {
        let c: Vec<&str> = piece.crates.iter().map(String::as_str).collect();
        let s1: Vec<String> = {
            let mut s = s1();
            s.sort();
            s
        };
        let peel: Vec<String> = {
            let mut s = s1_less_gui();
            s.sort();
            s
        };
        // index: 0 core, 1 cli, 2 gui lib, 3 agent lib, 4 agent lib-test, 5 types lib,
        // 6 types lib-test, 7 models lib, 8 models test, 9 harness lib-test, 10 gui lib-test
        let cap: &[&str] = &["cap"];
        let s1_units = vec![
            (
                "aterm-core",
                "aterm-core",
                "lib",
                "build",
                "0",
                &[][..],
                &[][..],
            ),
            ("aterm-cli", "aterm-cli", "lib", "build", "0", cap, &[0][..]),
            (
                "aterm-gui",
                "aterm-gui",
                "lib",
                "build",
                "0",
                &[][..],
                &[0, 1][..],
            ),
            (
                "aterm-agent",
                "aterm-agent",
                "lib",
                "build",
                "0",
                &[][..],
                &[0][..],
            ),
            (
                "aterm-agent",
                "aterm-agent",
                "lib",
                "test",
                "0",
                &[][..],
                &[0][..],
            ),
            (
                "aterm-types",
                "aterm-types",
                "lib",
                "build",
                "0",
                &[][..],
                &[][..],
            ),
            (
                "aterm-types",
                "aterm-types",
                "lib",
                "test",
                "0",
                &[][..],
                &[][..],
            ),
            (
                "aterm-models",
                "aterm-models",
                "lib",
                "build",
                "0",
                &[][..],
                &[5][..],
            ),
            (
                "aterm-models",
                "derived",
                "test",
                "test",
                "0",
                &[][..],
                &[7, 2, 1][..],
            ),
            (
                "aterm-models-harness",
                "aterm-models-harness",
                "lib",
                "test",
                "0",
                &[][..],
                &[7][..],
            ),
            (
                "aterm-gui",
                "aterm-gui",
                "lib",
                "test",
                "0",
                &[][..],
                &[0, 1][..],
            ),
        ];
        if piece.crates == s1 && !piece.lib {
            return Ok(graph(&s1_units));
        }
        if piece.crates == peel && !piece.lib {
            let mut units: Vec<_> = s1_units.clone();
            units.truncate(10);
            if self.peel_differs {
                units[4].5 = &["extra"][..];
            }
            return Ok(graph(&units));
        }
        let gui_alone = vec![
            (
                "aterm-core",
                "aterm-core",
                "lib",
                "build",
                "0",
                &[][..],
                &[][..],
            ),
            (
                "aterm-cli",
                "aterm-cli",
                "lib",
                "build",
                "0",
                &[][..],
                &[0][..],
            ),
            (
                "aterm-gui",
                "aterm-gui",
                "lib",
                "build",
                "0",
                &[][..],
                &[0, 1][..],
            ),
            (
                "aterm-gui",
                "aterm-gui",
                "lib",
                "test",
                "0",
                &[][..],
                &[0, 1][..],
            ),
            ("aterm-gui", "smoke", "test", "test", "0", &[][..], &[2][..]),
        ];
        match (c.as_slice(), piece.lib) {
            (["aterm-gui"], false) => Ok(graph(&gui_alone)),
            (["aterm-gui"], true) => {
                let mut units = gui_alone;
                units.truncate(4);
                units.remove(2);
                Ok(graph(&units))
            }
            (["aterm-effects", "aterm-gui"], false) => {
                let mut units = gui_alone;
                units.push((
                    "aterm-effects",
                    "aterm-effects",
                    "lib",
                    "test",
                    "0",
                    &[][..],
                    &[0][..],
                ));
                Ok(graph(&units))
            }
            (["aterm-effects"], false) => Ok(graph(&[
                (
                    "aterm-core",
                    "aterm-core",
                    "lib",
                    "build",
                    "0",
                    &["serde"][..],
                    &[][..],
                ),
                (
                    "aterm-effects",
                    "aterm-effects",
                    "lib",
                    "test",
                    "0",
                    &[][..],
                    &[0][..],
                ),
            ])),
            _ => Err(format!("no fixture for `{}`", piece.label())),
        }
    }
}

fn plan(
    selection: &[String],
    records: &Records,
    graphs: &Graphs,
) -> (Option<Vec<Call>>, Vec<String>) {
    let mut asked = Vec::new();
    let book = Shaping {
        records,
        plain: &Records::default(),
        checkout: HERE,
        budget: BUDGET,
    };
    let calls = shape(selection, &book, &mut |p: &Piece| {
        asked.push(p.label());
        graphs.of(p)
    })
    .calls();
    (calls, asked)
}

fn labels(calls: &[Call]) -> Vec<String> {
    calls.iter().map(|c| c.part.piece.label()).collect()
}

/// 10:23'S SELECTION IS SHAPED BY THE COVER, WHICH ITS OWN ROW MISSED: its own record (15.16 GiB)
/// claims 18.2 GiB, under the 21.3 GiB budget, but `-p aterm-gui` (17.63 GiB) and the other four
/// crates (12.72 GiB) it holds sum to 30.35 GiB, which claims 36.4 GiB. `-p aterm-gui` alone
/// builds units S1 does not (aterm-cli without `cap`) and is not cut; S1 less aterm-gui builds
/// only S1's units and claims 15.3 GiB, so it is call 1 of 2, keyed as `verify -p` of those four
/// keeps; the closing call is S1, claiming the whole estimate (at least the budget: it runs with
/// no other claiming stage). NEGATIVE CONTROL: with the cover dropped from the estimate, nothing
/// is shaped, and no unit graph is read.
#[test]
fn ten_twenty_threes_selection_is_cut_by_its_complement() {
    let (calls, asked) = plan(
        &s1(),
        &book_at_1023(),
        &Graphs {
            peel_differs: false,
        },
    );
    let calls = calls.expect("shaped");
    assert_eq!(
        labels(&calls),
        [
            "-p aterm-agent -p aterm-models -p aterm-models-harness -p aterm-types",
            "-p aterm-agent -p aterm-gui -p aterm-models -p aterm-models-harness -p aterm-types",
        ]
    );
    assert_eq!(calls[0].mem, aterm_capacity::peak_claim(1272 * GIB / 100));
    assert!(calls[0].mem < BUDGET);
    assert_eq!(
        calls[0].part.variant(),
        None,
        "its record is `verify -p` of the four's"
    );
    assert!(
        calls[0]
            .part
            .said
            .contains("its 10 units are units the whole call builds"),
        "{}",
        calls[0].part.said
    );
    let closing = &calls[1].part;
    assert!(closing.closing() && closing.shaped());
    assert_eq!(
        closing.variant().as_deref(),
        Some("after -p aterm-agent -p aterm-models -p aterm-models-harness -p aterm-types")
    );
    assert!(calls[1].mem >= BUDGET, "{}", calls[1].mem);
    assert!(
        closing
            .said
            .contains("the narrower selections it holds, summed"),
        "{}",
        closing.said
    );
    assert!(asked.contains(&"-p aterm-gui".to_owned()), "{asked:?}");

    let mut own_only = Records::default();
    row(
        &mut own_only,
        QUIET,
        &s1().iter().map(String::as_str).collect::<Vec<_>>(),
        None,
        15.16,
    );
    let (calls, asked) = plan(
        &s1(),
        &own_only,
        &Graphs {
            peel_differs: false,
        },
    );
    assert_eq!(calls, None);
    assert!(
        asked.is_empty(),
        "an unshaped run reads no unit graph: {asked:?}"
    );
}

/// WITH NO CUT THAT HOLDS, ONE CALL THAT SAYS WHY: when S1 less aterm-gui builds aterm-agent with
/// a feature S1's does not, neither it nor `-p aterm-gui` is cut, and the stage is one call
/// claiming the whole estimate, naming each cut and the units it builds unlike the whole call.
/// NEGATIVE CONTROL: with the unit identity unchecked by hand, S1 was cut into three calls.
#[test]
fn a_selection_no_cut_of_which_holds_is_one_call_saying_why() {
    let (calls, _) = plan(&s1(), &book_at_1023(), &Graphs { peel_differs: true });
    let calls = calls.expect("planned");
    assert_eq!(calls.len(), 1);
    let part = &calls[0].part;
    assert!(!part.shaped() && part.closing());
    assert_eq!(calls[0].mem, aterm_capacity::peak_claim(3035 * GIB / 100));
    assert!(
        part.said
            .contains("at least the run's 21.3 GiB budget; not shaped: no cut of it holds"),
        "{}",
        part.said
    );
    assert!(
        part.said
            .contains("`-p aterm-gui` builds 4 of its 5 units unlike the whole call"),
        "{}",
        part.said
    );
    assert!(
        part.said.contains(
            "`-p aterm-agent -p aterm-models -p aterm-models-harness -p aterm-types` builds"
        ),
        "{}",
        part.said
    );
}

/// A CUT A RUN REFUSED IS NEVER TAKEN AGAIN: with the refusal row a closing call's rebuild kept
/// for S1 less aterm-gui, S1 is one call naming the refusal, which its part carries for its stage
/// to keep again; a refusal another checkout's run kept is not this one's. NEGATIVE CONTROL: with
/// the refusal unread by hand, S1 was cut; with the planner carrying none, the part's list was
/// empty.
#[test]
fn a_refused_cut_is_not_taken_again() {
    let mut r = book_at_1023();
    let whole = Piece::of(&s1(), false);
    let peel = Piece::of(&s1_less_gui(), false);
    r.push(refusal_key(HERE, &whole, &peel), 0, 9);
    let (calls, _) = plan(
        &s1(),
        &r,
        &Graphs {
            peel_differs: false,
        },
    );
    let calls = calls.expect("planned");
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0]
            .part
            .said
            .contains("was refused after a run whose closing call rebuilt"),
        "{}",
        calls[0].part.said
    );
    assert_eq!(
        calls[0].part.refused,
        std::slice::from_ref(&peel),
        "its stage keeps it again"
    );

    let mut elsewhere = book_at_1023();
    elsewhere.push(refusal_key(QUIET, &whole, &peel), 0, 9);
    let (calls, _) = plan(
        &s1(),
        &elsewhere,
        &Graphs {
            peel_differs: false,
        },
    );
    assert_eq!(calls.map(|c| c.len()), Some(2));
}

/// A ONE-CRATE SELECTION SPLITS ITS LIB-TEST OFF: `-p aterm-gui`, whose kept units (its lib-test
/// 12.7 GiB, its lib 7.7 GiB) sum past the budget's claim, is `-p aterm-gui --lib` (its lib-test
/// and what it needs, not its lib) and then `-p aterm-gui`, after it. The lib-test call claims by
/// its units' peaks (12.7 GiB and a fifth more) and keeps its record under `targets --lib`; the
/// closing call, with no record after it, by the one kept unit only it builds (its lib, 7.7 GiB).
/// aterm-models' lib is not tested, and the graph says so. NEGATIVE CONTROL: with the split off by
/// hand, `-p aterm-gui` was one call.
#[test]
fn a_one_crate_selection_splits_its_lib_test_off() {
    let mut r = Records::default();
    unit(&mut r, "aterm-gui", "aterm_gui test", 12.7);
    unit(&mut r, "aterm-gui", "aterm_gui lib", 7.7);
    let (calls, asked) = plan(
        &names(&["aterm-gui"]),
        &r,
        &Graphs {
            peel_differs: false,
        },
    );
    let calls = calls.expect("shaped");
    assert_eq!(labels(&calls), ["-p aterm-gui --lib", "-p aterm-gui"]);
    assert_eq!(calls[0].part.variant().as_deref(), Some(LIB_VARIANT));
    assert_eq!(calls[0].mem, aterm_capacity::peak_claim(127 * GIB / 10));
    assert_eq!(
        calls[1].part.variant().as_deref(),
        Some("after -p aterm-gui --lib")
    );
    assert_eq!(calls[1].mem, aterm_capacity::peak_claim(77 * GIB / 10));
    assert!(asked.contains(&"-p aterm-gui --lib".to_owned()));
    let mut i = Interner::default();
    let s1_graph = Graph::read(
        &Graphs {
            peel_differs: false,
        }
        .of(&Piece::of(&s1(), false))
        .expect("s1"),
        &mut i,
    )
    .expect("reads");
    assert!(!s1_graph.tests_lib_of("aterm-models") && s1_graph.tests_lib_of("aterm-gui"));
}

/// A CLOSING CALL WITH NO RECORD AFTER THE CUTS CLAIMS BY ITS OWN UNITS, NOT THE WHOLE: `-p
/// aterm-gui` (its lib-test 12.7 GiB, its lib 7.7 GiB, its smoke suite 1.5 GiB, summing past the
/// budget's claim) closes after its lib-test call, which built the lib-test, so the closing call
/// claims by the lib and the smoke suite summed (9.2 GiB and a fifth more), planned by that, where
/// the whole estimate (21.9 GiB) would claim past the budget and, on a Mac that cannot hold it,
/// run alone with every other session's memory claim waiting (measured 2026-11-06: 51.2 GiB
/// asked on a 48 GiB Mac, for a closing call whose heaviest unit held 21.3 GiB). With no unit it
/// builds kept, the whole estimate (its own record, 20 GiB). NEGATIVE CONTROL: with the closing call's units unread
/// by hand, it claimed the whole estimate, 26.3 GiB.
#[test]
fn a_closing_call_with_no_record_claims_by_its_own_units() {
    let mut r = Records::default();
    unit(&mut r, "aterm-gui", "aterm_gui test", 12.7);
    unit(&mut r, "aterm-gui", "aterm_gui lib", 7.7);
    unit(&mut r, "aterm-gui", "smoke test", 1.5);
    let graphs = Graphs {
        peel_differs: false,
    };
    let (calls, _) = plan(&names(&["aterm-gui"]), &r, &graphs);
    let calls = calls.expect("shaped");
    assert_eq!(labels(&calls), ["-p aterm-gui --lib", "-p aterm-gui"]);
    let closing = &calls[1];
    assert_eq!(closing.mem, aterm_capacity::peak_claim(92 * GIB / 10));
    assert!(closing.mem < BUDGET, "{}", closing.mem);
    assert_eq!(closing.part.planned, Some(92 * GIB / 10));
    assert!(
        closing
            .part
            .said
            .contains("by the units no call before it builds, its 2 heaviest units' peaks summed"),
        "{}",
        closing.part.said
    );

    let mut lib_test_only = Records::default();
    row(&mut lib_test_only, VMEM, &["aterm-gui"], None, 20.0);
    unit(&mut lib_test_only, "aterm-gui", "aterm_gui test", 12.7);
    let (calls, _) = plan(&names(&["aterm-gui"]), &lib_test_only, &graphs);
    let calls = calls.expect("shaped");
    assert_eq!(labels(&calls), ["-p aterm-gui --lib", "-p aterm-gui"]);
    assert_eq!(calls[1].mem, aterm_capacity::peak_claim(20 * GIB));
    assert_eq!(
        calls[1].part.planned, None,
        "the whole estimate plans no run"
    );
}

/// A CUT OVER ONE CRATE SPLITS ITS LIB-TEST OFF TOO: `-p aterm-effects -p aterm-gui` (21.56 GiB,
/// claiming 25.9 GiB) cuts `-p aterm-gui` (17.63 GiB, under the budget's claim), and, its lib-test
/// known (9.1 GiB), `-p aterm-gui --lib` before it; `-p aterm-effects` alone builds units the pair
/// does not and is not cut. With no unit of aterm-gui kept, the lib-test call has nothing to claim
/// by and the cut stays whole. NEGATIVE CONTROL: that second plan, two calls.
#[test]
fn a_one_crate_cut_splits_its_lib_test_off_when_its_units_are_known() {
    let pair = names(&["aterm-effects", "aterm-gui"]);
    let mut r = Records::default();
    row(&mut r, VMEM, &["aterm-effects", "aterm-gui"], None, 21.56);
    row(&mut r, VMEM, &["aterm-gui"], None, 17.63);
    let (calls, _) = plan(
        &pair,
        &r,
        &Graphs {
            peel_differs: false,
        },
    );
    let calls = calls.expect("shaped");
    assert_eq!(
        labels(&calls),
        ["-p aterm-gui", "-p aterm-effects -p aterm-gui"]
    );
    assert_eq!(calls[0].mem, aterm_capacity::peak_claim(1763 * GIB / 100));

    unit(&mut r, "aterm-gui", "aterm_gui test", 9.1);
    let (calls, _) = plan(
        &pair,
        &r,
        &Graphs {
            peel_differs: false,
        },
    );
    let calls = calls.expect("shaped");
    assert_eq!(
        labels(&calls),
        [
            "-p aterm-gui --lib",
            "-p aterm-gui",
            "-p aterm-effects -p aterm-gui"
        ]
    );
    assert_eq!(calls[0].mem, aterm_capacity::peak_claim(91 * GIB / 10));
    assert_eq!(
        calls[1].part.variant().as_deref(),
        Some("after -p aterm-gui --lib")
    );
    assert_eq!(
        calls[1].mem,
        aterm_capacity::peak_claim(1763 * GIB / 100),
        "the most of it whole bounds it"
    );
    assert_eq!(
        calls[2].part.variant().as_deref(),
        Some("after -p aterm-gui --lib; -p aterm-gui")
    );
}

/// A SHAPED RUN'S RECORDS SHAPE THE NEXT: with the closing call's record after the cut (11 GiB),
/// the cut's own, and aterm-gui's lib-test (9 GiB, a unit only the closing call builds), the
/// closing call claims by its own record (13.2 GiB), and the estimate counts the shaped run's
/// calls summed, so the selection stays shaped when nothing else names it. Without the closing
/// call's record it claims the whole estimate. NEGATIVE CONTROLS, by hand: with the shaped run's
/// sum dropped from the estimate nothing was shaped; with the closing call's record unread it
/// claimed 28.5 GiB.
#[test]
fn a_shaped_runs_records_shape_the_next() {
    let peel = s1_less_gui();
    let after = format!("{AFTER}{}", Piece::of(&peel, false).label());
    let mut r = Records::default();
    let whole = Piece::of(&s1(), false);
    let s: Vec<&str> = whole.crates.iter().map(String::as_str).collect();
    let p: Vec<&str> = peel.iter().map(String::as_str).collect();
    row(&mut r, HERE, &s, Some(&after), 11.0);
    row(&mut r, HERE, &p, None, 12.72);
    unit(&mut r, "aterm-gui", "aterm_gui test", 9.0);
    let (calls, _) = plan(
        &s1(),
        &r,
        &Graphs {
            peel_differs: false,
        },
    );
    let calls = calls.expect("shaped by its last shaped run, 23.72 GiB");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].mem, aterm_capacity::peak_claim(11 * GIB));
    assert!(
        calls[1]
            .part
            .said
            .contains("after the cuts, by the larger of the most"),
        "{}",
        calls[1].part.said
    );

    let mut no_after = Records::default();
    row(&mut no_after, HERE, &p, None, 12.72);
    row(&mut no_after, VMEM, &["aterm-gui"], None, 17.63);
    let (calls, _) = plan(
        &s1(),
        &no_after,
        &Graphs {
            peel_differs: false,
        },
    );
    let calls = calls.expect("shaped by the cover");
    assert!(calls[1].mem >= BUDGET);
}

/// A SELECTION'S OWN RECORDS OUTRANK ITS UNITS' PEAKS, as this Mac's book read after the first
/// shaped run of S1 (2026-10-29): S1 less aterm-gui held 13.02 GiB, where its units' peaks sum to
/// 21.02 GiB (claiming 25.2 GiB, past the budget), since a unit sum counts every unit its graph
/// lists, fresh or not. So the cut claims by its own record, and the closing call by its record
/// after it (15.53 GiB). `-p aterm-gui`, whose own record (17.63 GiB) claims under the budget, is
/// not shaped though its units' peaks sum past it, and no unit graph is read. NEGATIVE CONTROLS, by
/// hand: with the unit sum beside the records in a piece's estimate, S1 was one call claiming the
/// budget, the cut refused; with it beside them in the trigger, `-p aterm-gui` was shaped.
#[test]
fn a_selections_own_records_outrank_its_units_peaks() {
    let peel = s1_less_gui();
    let p: Vec<&str> = peel.iter().map(String::as_str).collect();
    let whole = Piece::of(&s1(), false);
    let s: Vec<&str> = whole.crates.iter().map(String::as_str).collect();
    let after = format!("{AFTER}{}", Piece::of(&peel, false).label());
    let mut r = Records::default();
    row(&mut r, QUIET, &s, None, 29.12);
    row(&mut r, VMEM, &["aterm-gui"], None, 17.63);
    row(&mut r, HERE, &p, None, 13.02);
    row(&mut r, HERE, &s, Some(&after), 15.53);
    unit(&mut r, "aterm-agent", "aterm_agent test", 8.41);
    unit(&mut r, "aterm-gui", "aterm_gui lib", 6.1);
    unit(&mut r, "aterm-agent", "aterm_agent lib", 2.81);
    unit(&mut r, "aterm-cli", "aterm_cli lib", 2.5);
    unit(&mut r, "aterm-core", "aterm_core lib", 1.2);
    unit(&mut r, "aterm-gui", "aterm_gui test", 15.53);
    let graphs = Graphs {
        peel_differs: false,
    };
    let (calls, _) = plan(&s1(), &r, &graphs);
    let calls = calls.expect("shaped");
    assert_eq!(
        labels(&calls),
        [Piece::of(&peel, false).label(), whole.label()]
    );
    assert_eq!(calls[0].mem, aterm_capacity::peak_claim(1302 * GIB / 100));
    assert_eq!(calls[1].mem, aterm_capacity::peak_claim(1553 * GIB / 100));

    let (calls, asked) = plan(&names(&["aterm-gui"]), &r, &graphs);
    assert_eq!(calls, None);
    assert!(asked.is_empty(), "{asked:?}");
}

fn files(names: &[&str]) -> std::collections::BTreeSet<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

fn artifact(names: &[&str], fresh: bool) -> Artifact {
    Artifact {
        filenames: names.iter().map(|n| (*n).to_owned()).collect(),
        fresh,
        package: None,
        test: false,
    }
}

/// No source changed under the run ([`changed_since`] as a fixture).
fn unchanged(_: std::time::SystemTime) -> Option<String> {
    None
}

/// THE FRESH CHECK: a cut whose built files come back fresh in the closing call stands; one whose
/// artifact the closing call rebuilt is refused, and one whose file is in none of the closing
/// call's artifacts (the whole call does not build it) too; a later cut that does not build a
/// file an earlier one built says nothing of it. A rebuild from a source that changed after the
/// cut began refuses nothing, and says so. NEGATIVE CONTROLS: with rebuilt artifacts ignored by
/// hand, the rebuilt cut stood; with the changed source unread, the edited run refused it.
#[test]
fn a_cut_the_closing_call_rebuilt_is_refused() {
    let lib = Piece::of(&names(&["aterm-gui"]), true);
    let began = std::time::SystemTime::UNIX_EPOCH;
    let built = vec![(
        lib.clone(),
        files(&["/d/libaterm_gui-1.rlib", "/d/aterm_gui-2"]),
        began,
    )];
    let both = ["/d/libaterm_gui-1.rlib", "/d/aterm_gui-2"];
    let (refused, said) = fresh_check(&built, &[artifact(&both, true)], true, &unchanged);
    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(said, "shape: fresh here: `-p aterm-gui --lib`'s 2 files");

    let (refused, said) = fresh_check(
        &built,
        &[
            artifact(&["/d/libaterm_gui-1.rlib"], true),
            artifact(&["/d/aterm_gui-2"], false),
        ],
        true,
        &unchanged,
    );
    assert_eq!(refused.len(), 1);
    assert!(
        said.contains("`-p aterm-gui --lib` refused: it rebuilt 1 of the artifacts"),
        "{said}"
    );

    let asked = std::cell::Cell::new(None);
    let (refused, said) = fresh_check(
        &built,
        &[artifact(&["/d/aterm_gui-2"], false)],
        true,
        &|since| {
            asked.set(Some(since));
            Some("crates/aterm-gui/src/lib.rs".to_owned())
        },
    );
    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(asked.get(), Some(began), "asked since the cut began");
    assert!(
        said.contains(
            "not refused, its sources changed under the run: `-p aterm-gui --lib` (it rebuilt 1 \
             of its artifacts, and `crates/aterm-gui/src/lib.rs` changed after it began)"
        ),
        "{said}"
    );

    let (refused, said) = fresh_check(
        &built,
        &[artifact(&["/d/libaterm_gui-1.rlib"], true)],
        true,
        &unchanged,
    );
    assert_eq!(refused.len(), 1, "{said}");
    assert!(
        said.contains("1 of the 2 files `-p aterm-gui --lib` built are in none"),
        "{said}"
    );

    let (refused, said) = fresh_check(&built, &[artifact(&["/d/other"], false)], false, &unchanged);
    assert!(refused.is_empty() && said.is_empty(), "{said}");

    let out = "warning: x\n{\"reason\":\"compiler-artifact\",\"filenames\":[\"/d/a\"],\"fresh\":true}\n\
               {\"reason\":\"build-finished\",\"success\":true}\n";
    assert_eq!(artifacts(out), [artifact(&["/d/a"], true)]);
}

/// A COMPILE'S MESSAGES NAME EACH ARTIFACT'S PACKAGE, WHETHER IT IS A TEST BUILD, AND EACH BUILD
/// SCRIPT IT RAN, as cargo spells them: a path package's id with its name and with only its
/// version (its folder is its name), a registry package's, the form before cargo 1.77; the
/// `out_dir` of each `build-script-executed`. NEGATIVE CONTROL: an id of no form read names no
/// package, an artifact with no `profile.test` is no test build, and a build script's message is
/// no artifact.
#[test]
fn a_compiles_messages_name_each_artifacts_package_and_each_script_run() {
    for (id, name) in [
        (
            "path+file:///w/aterm/crates/bc#bc-pkg@0.1.0",
            Some("bc-pkg"),
        ),
        ("path+file:///w/aterm/crates/atpkg#0.155.0", Some("atpkg")),
        (
            "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.200",
            Some("serde"),
        ),
        (
            "git+https://github.com/x/cargo?branch=b#0.52.0",
            Some("cargo"),
        ),
        (
            "libc 0.2.186 (registry+https://github.com/rust-lang/crates.io-index)",
            Some("libc"),
        ),
        ("path+file:///#0.1.0", None),
        ("no-fragment", None),
        ("", None),
    ] {
        assert_eq!(package_name(id).as_deref(), name, "{id}");
    }
    let out = concat!(
        r#"{"reason":"compiler-artifact","package_id":"path+file:///w/aterm/crates/atpkg#0.155.0","profile":{"test":true},"filenames":["/b/debug/build/atpkg/1111111111111111/out/atpkg-1111111111111111"],"fresh":false}"#,
        "\n",
        r#"{"reason":"build-script-executed","package_id":"path+file:///w/aterm/crates/atpkg#0.155.0","out_dir":"/b/debug/build/atpkg/2222222222222222/out"}"#,
        "\n",
        r#"{"reason":"compiler-artifact","package_id":"what","filenames":["/t/debug/atpkg"],"fresh":true}"#,
        "\n",
    );
    let read = artifacts(out);
    assert_eq!(
        read.iter()
            .map(|a| (a.package.as_deref(), a.test, a.fresh))
            .collect::<Vec<_>>(),
        vec![(Some("atpkg"), true, false), (None, false, true)]
    );
    assert_eq!(
        script_runs(out),
        vec!["/b/debug/build/atpkg/2222222222222222/out".to_owned()]
    );
}

/// Three crates whose every call builds their shared dependency `d`: `-p a`, `-p b` and the
/// whole `-p a -p b -p c`, each crate's tests an integration test over `d`.
fn disjoint_graphs(piece: &Piece) -> Result<String, String> {
    let d = ("d", "d", "lib", "build", "0", &[][..], &[][..]);
    let suite = |p: &'static str| -> crate::unitgraph::tests::Fixture<'static> {
        (p, p, "test", "test", "0", &[][..], &[0][..])
    };
    let c: Vec<&str> = piece.crates.iter().map(String::as_str).collect();
    match (c.as_slice(), piece.lib) {
        (["a"], false) => Ok(graph(&[d, suite("a")])),
        (["b"], false) => Ok(graph(&[d, suite("b")])),
        (["a", "b", "c"], false) => Ok(graph(&[d, suite("a"), suite("b"), suite("c")])),
        _ => Err(format!("no fixture for `{}`", piece.label())),
    }
}

/// A SECOND CUT THAT SHARES NO CRATE WITH THE FIRST STILL RUNS AFTER IT: `-p a -p b -p c`, whose
/// cover (`-p a` 12 GiB and `-p b` 10 GiB) claims past the budget, is cut into `-p a`, then `-p
/// b`, then the whole. `-p b` runs in the build directory `-p a` just built `d` in, so its peak
/// is the rest of its work: it keeps its record after `-p a`, never under `verify -p b`'s own
/// key, which an unshaped run of `-p b` and the next plan's estimate of the cut read. NEGATIVE
/// CONTROL: with `after` holding only the calls that share a crate, by hand, call 2 kept under
/// `verify -p b`'s key (no variant).
#[test]
fn a_second_cut_sharing_no_crate_runs_after_the_first() {
    let mut r = Records::default();
    row(&mut r, HERE, &["a"], None, 12.0);
    row(&mut r, HERE, &["b"], None, 10.0);
    row(&mut r, HERE, &["b"], Some("after -p a"), 4.0);
    unit(&mut r, "d", "lib", 9.0);
    unit(&mut r, "b", "test b", 2.5);
    let book = Shaping {
        records: &r,
        plain: &Records::default(),
        checkout: HERE,
        budget: BUDGET,
    };
    let calls = shape(&names(&["a", "b", "c"]), &book, &mut disjoint_graphs)
        .calls()
        .expect("shaped");
    assert_eq!(labels(&calls), ["-p a", "-p b", "-p a -p b -p c"]);
    assert_eq!(calls[0].part.variant(), None);
    assert_eq!(calls[1].part.variant().as_deref(), Some("after -p a"));
    assert_eq!(calls[1].mem, aterm_capacity::peak_claim(4 * GIB));
    assert_eq!(calls[2].part.variant().as_deref(), Some("after -p a; -p b"));
}

/// A LIGHT FIRST RECORD NEVER SETS THE CLOSING CALL'S CLAIM BELOW ITS HEAVIEST UNIT. A warm
/// directory's first shaped run of S1 finds every unit fresh, and its closing call keeps 32 MiB
/// under its `after` key, which held nothing. With no unit only the closing call builds kept, the
/// next plan's closing call claims the whole estimate, as with no record of it; with aterm-gui's
/// lib-test kept (15.53 GiB, which the cut does not build), at least that. NEGATIVE CONTROLS, by
/// hand: with the whole estimate's fallback off, the closing call claimed 1.03 GiB; with the unit
/// floor off, the whole estimate where 18.6 GiB is its claim.
#[test]
fn a_light_first_closing_record_claims_no_less_than_its_heaviest_unit() {
    let peel = s1_less_gui();
    let whole = Piece::of(&s1(), false);
    let s: Vec<&str> = whole.crates.iter().map(String::as_str).collect();
    let after = format!("{AFTER}{}", Piece::of(&peel, false).label());
    let graphs = Graphs {
        peel_differs: false,
    };
    let mut r = book_at_1023();
    row(&mut r, HERE, &s, None, 29.12);
    row(&mut r, HERE, &s, Some(&after), 0.032);
    let (calls, _) = plan(&s1(), &r, &graphs);
    let calls = calls.expect("shaped");
    assert_eq!(
        labels(&calls),
        [Piece::of(&peel, false).label(), whole.label()]
    );
    assert!(calls[1].mem >= BUDGET, "claimed {}", calls[1].mem);
    assert_eq!(
        calls[1].part.planned, None,
        "the whole estimate plans no run of it"
    );
    assert!(
        calls[1]
            .part
            .said
            .contains("no unit only it builds is kept, which would bound the most"),
        "{}",
        calls[1].part.said
    );

    unit(&mut r, "aterm-gui", "aterm_gui test", 15.53);
    let (calls, _) = plan(&s1(), &r, &graphs);
    let calls = calls.expect("shaped");
    assert_eq!(calls[1].mem, aterm_capacity::peak_claim(1553 * GIB / 100));
    assert!(calls[1].mem < BUDGET);
    assert_eq!(calls[1].part.planned, Some(1553 * GIB / 100));
    assert!(
        calls[1]
            .part
            .said
            .contains("its heaviest unit no call before it builds (`aterm_gui test`, 15.5 GiB)"),
        "{}",
        calls[1].part.said
    );
}

/// A LIGHT FIRST RECORD NEVER SETS A CUT'S CLAIM BELOW ITS HEAVIEST UNIT: S1 less aterm-gui's
/// only record is its first, a fresh run's 31 MiB under `verify -p` of its four crates, and the cut
/// still claims at least aterm-agent's lib-test (8.41 GiB). NEGATIVE CONTROL, by hand: with the
/// unit floor off, it claimed 1.03 GiB, beside any other stage of the run.
#[test]
fn a_light_first_cut_record_claims_no_less_than_its_heaviest_unit() {
    let peel = s1_less_gui();
    let p: Vec<&str> = peel.iter().map(String::as_str).collect();
    let whole = Piece::of(&s1(), false);
    let s: Vec<&str> = whole.crates.iter().map(String::as_str).collect();
    let graphs = Graphs {
        peel_differs: false,
    };
    let mut r = Records::default();
    row(&mut r, QUIET, &s, None, 29.12);
    row(&mut r, VMEM, &["aterm-gui"], None, 17.63);
    row(&mut r, HERE, &p, None, 0.031);
    unit(&mut r, "aterm-agent", "aterm_agent test", 8.41);
    unit(&mut r, "aterm-gui", "aterm_gui test", 15.53);
    let (calls, _) = plan(&s1(), &r, &graphs);
    let calls = calls.expect("shaped");
    assert_eq!(
        labels(&calls),
        [Piece::of(&peel, false).label(), whole.label()]
    );
    assert_eq!(calls[0].mem, aterm_capacity::peak_claim(841 * GIB / 100));
    assert_eq!(calls[0].part.planned, Some(841 * GIB / 100));
    assert!(
        calls[0].part.said.contains("(`aterm_agent test`, 8.4 GiB)"),
        "{}",
        calls[0].part.said
    );
}

/// A REBUILT ARTIFACT'S SOURCES ARE READ FROM ITS DEP-INFO: `deps/libaterm_gui-1.rlib` and the
/// test `deps/aterm_gui-2` are compiled from what `deps/aterm_gui-1.d` and `deps/aterm_gui-2.d`
/// list, a source relative to the workspace root resolved under it. One modified after `since`
/// is found; one older is not, nor a fresh artifact's, nor a build script's output the dep-info
/// names by its absolute path, however new. NEGATIVE CONTROL, by hand: with absolute sources read
/// too, the new `OUT_DIR` file was found.
#[test]
fn a_rebuilt_artifacts_changed_source_is_read_from_its_dep_info() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let root = dir.path();
    let deps = root.join("build/deps");
    std::fs::create_dir_all(&deps).expect("deps");
    std::fs::create_dir_all(root.join("crates/aterm-gui/src")).expect("src");
    let out = root.join("build/out/gen.rs");
    std::fs::create_dir_all(out.parent().expect("its folder")).expect("out");
    let since = std::time::SystemTime::now();
    let hour = std::time::Duration::from_secs(3600);
    let touch = |path: &std::path::Path, at: std::time::SystemTime| {
        std::fs::write(path, "").expect("written");
        std::fs::File::options()
            .write(true)
            .open(path)
            .and_then(|f| f.set_modified(at))
            .expect("its time");
    };
    touch(&root.join("crates/aterm-gui/src/lib.rs"), since - hour);
    touch(&root.join("crates/aterm-gui/src/new part.rs"), since - hour);
    touch(&out, since + hour);
    std::fs::write(
        deps.join("aterm_gui-1.d"),
        format!(
            "{}: crates/aterm-gui/src/lib.rs {}\n\ncrates/aterm-gui/src/lib.rs:\n",
            deps.join("aterm_gui-1.d").display(),
            out.display()
        ),
    )
    .expect("dep-info");
    std::fs::write(
        deps.join("aterm_gui-2.d"),
        format!(
            "{}: crates/aterm-gui/src/new\\ part.rs\n",
            deps.join("aterm_gui-2").display()
        ),
    )
    .expect("dep-info");
    let lib = deps.join("libaterm_gui-1.rlib").display().to_string();
    let test = deps.join("aterm_gui-2").display().to_string();
    let rebuilt = [artifact(&[&lib], false), artifact(&[&test], false)];
    assert_eq!(changed_since(root, &rebuilt, since), None);

    touch(&root.join("crates/aterm-gui/src/new part.rs"), since + hour);
    assert_eq!(
        changed_since(root, &rebuilt, since).as_deref(),
        Some("crates/aterm-gui/src/new part.rs")
    );
    assert_eq!(
        changed_since(root, &[artifact(&[&test], true)], since),
        None,
        "a fresh artifact was not rebuilt"
    );

    let outer = aterm_tempfile::tempdir().expect("outer");
    let nested = outer.path().join("ws");
    std::fs::create_dir_all(nested.join("build/deps")).expect("nested deps");
    touch(&outer.path().join("outside.rs"), since + hour);
    std::fs::write(
        nested.join("build/deps/esc-1.d"),
        format!(
            "{}: ../outside.rs\n",
            nested.join("build/deps/esc-1").display()
        ),
    )
    .expect("escape dep-info");
    let esc = nested.join("build/deps/esc-1").display().to_string();
    assert_eq!(
        changed_since(&nested, &[artifact(&[&esc], false)], since),
        None,
        "a relative parent-dir traversal escapes the workspace root"
    );
    #[cfg(unix)]
    {
        let link_d = nested.join("build/deps/link-1.d");
        std::os::unix::fs::symlink(deps.join("aterm_gui-2.d"), &link_d).expect("symlink .d");
        let linked = nested.join("build/deps/link-1").display().to_string();
        assert_eq!(
            changed_since(root, &[artifact(&[&linked], false)], since),
            None,
            "a symlinked dep-info file is ignored"
        );
    }
}
