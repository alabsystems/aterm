// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

// The selector in stats_deck.rs at 232e57e3263, before reusing its candidate buffer.
fn previous_choice(
    clauses: &[ClauseTxt],
    available: f64,
    width: impl Fn(&[(&ClauseTxt, bool)]) -> f64,
) -> Vec<(&ClauseTxt, bool)> {
    let mut ps: Vec<&ClauseTxt> = clauses.iter().filter(|c| !c.forms.is_empty()).collect();
    ps.sort_by_key(|c| c.prio);
    let keepers: Vec<&ClauseTxt> = ps.iter().copied().filter(|c| c.keep).collect();
    let others: Vec<&ClauseTxt> = ps.iter().copied().filter(|c| !c.keep).collect();
    let mut choice: Option<Vec<(&ClauseTxt, bool)>> = None;
    'sets: for n in (0..=others.len()).rev() {
        let mut kept: Vec<&ClauseTxt> = keepers
            .iter()
            .chain(others.iter().take(n))
            .copied()
            .collect();
        kept.sort_by_key(|c| c.prio);
        let mut arr: Vec<(&ClauseTxt, bool)> = kept.iter().map(|&c| (c, false)).collect();
        if width(&arr) <= available {
            choice = Some(arr);
            break;
        }
        for i in (0..arr.len()).rev() {
            if let Some(slot) = arr.get_mut(i) {
                slot.1 = true;
            }
            if width(&arr) <= available {
                choice = Some(arr);
                break 'sets;
            }
        }
    }
    choice.unwrap_or_else(|| {
        let base = if keepers.is_empty() {
            ps.iter().take(1).copied().collect()
        } else {
            keepers.clone()
        };
        base.into_iter().map(|c| (c, true)).collect()
    })
}

fn row_width(arr: &[(&ClauseTxt, bool)], separator: f64) -> f64 {
    arr.iter()
        .enumerate()
        .map(|(i, &(c, short))| {
            let form = if short {
                c.forms.len().min(2).saturating_sub(1)
            } else {
                0
            };
            let bold = matches!(c.ink, InkKind::Warn | InkKind::Critical | InkKind::Stuck);
            c.forms.get(form).map_or(0.0, |t| t.w(SMALL_PT, bold))
                + if c.mark.is_some() {
                    MARK_EM * SMALL_PT
                } else {
                    0.0
                }
                + if i > 0 { separator } else { 0.0 }
        })
        .sum()
}

fn signature<'a>(
    clauses: &'a [ClauseTxt],
    choice: impl IntoIterator<Item = (&'a ClauseTxt, bool)>,
) -> Vec<(usize, bool)> {
    choice
        .into_iter()
        .map(|(c, short)| {
            (
                clauses.iter().position(|v| std::ptr::eq(c, v)).unwrap(),
                short,
            )
        })
        .collect()
}

fn permutations(ids: &mut [u8], at: usize, visit: &mut impl FnMut(&[u8])) {
    if at == ids.len() {
        visit(ids);
        return;
    }
    for i in at..ids.len() {
        ids.swap(at, i);
        permutations(ids, at + 1, visit);
        ids.swap(at, i);
    }
}

#[test]
fn selection_matches_previous_priorities_forms_and_fallbacks() {
    let m = deck_metrics();
    let separator = Txt::new(" · ", &m).w(SMALL_PT, false);
    let mut compared = 0;
    for len in 0..=5 {
        let mut ids: Vec<u8> = (0..len).collect();
        permutations(&mut ids, 0, &mut |ids| {
            for keep in 0..1_u32 << len {
                for rotation in 0..5 {
                    let clauses: Vec<_> = ids
                        .iter()
                        .map(|&id| {
                            let forms: &[&str] = match (id + rotation) % 5 {
                                0 => &[],
                                1 => &["one form"],
                                2 => &["a longer clause", "short"],
                                3 => &["x", "a second form that is longer"],
                                _ => &["first", "second", "unused third"],
                            };
                            ClauseTxt {
                                forms: forms.iter().map(|s| Txt::new(*s, &m)).collect(),
                                prio: id % 3,
                                keep: keep & (1 << id) != 0,
                                mark: (id % 2 == 0).then_some(WellMark::Stuck),
                                ink: match id % 4 {
                                    0 => InkKind::Words,
                                    1 => InkKind::Warn,
                                    2 => InkKind::Critical,
                                    _ => InkKind::Stuck,
                                },
                            }
                        })
                        .collect();
                    let width = |arr: &[(&ClauseTxt, bool)]| row_width(arr, separator);
                    let mut rooms = vec![
                        f64::NAN,
                        f64::NEG_INFINITY,
                        f64::INFINITY,
                        -1.0,
                        -0.01,
                        0.0,
                        1.0,
                        20.0,
                        40.0,
                        90.0,
                        180.0,
                        400.0,
                    ];
                    for c in &clauses {
                        for short in [false, true] {
                            let edge = width(&[(c, short)]);
                            let epsilon = f64::EPSILON * edge.max(1.0);
                            rooms.extend([edge - epsilon, edge, edge + epsilon]);
                        }
                    }
                    for available in rooms {
                        let before =
                            signature(&clauses, previous_choice(&clauses, available, width));
                        let after = signature(&clauses, choose_details(&clauses, available, width));
                        assert_eq!(
                            before, after,
                            "ids={ids:?} keep={keep} rotation={rotation} room={available}"
                        );
                        compared += 1;
                    }
                }
            }
        });
    }
    assert!(compared > 100_000, "only {compared} selections compared");
    println!("detail selections compared: {compared}");
}

#[test]
#[ignore = "manual paired detail-selection allocation and instruction measurement"]
fn measure_detail_selection() {
    use aterm_bench::measurement_clock::counters;
    use std::hint::black_box;

    type Probe = for<'a> fn(&'a [ClauseTxt], f64, f64) -> Vec<(&'a ClauseTxt, bool)>;
    fn before(clauses: &[ClauseTxt], room: f64, separator: f64) -> Vec<(&ClauseTxt, bool)> {
        previous_choice(clauses, room, |arr| row_width(arr, separator))
    }
    fn after(clauses: &[ClauseTxt], room: f64, separator: f64) -> Vec<(&ClauseTxt, bool)> {
        choose_details(clauses, room, |arr| row_width(arr, separator))
    }
    use super::paint_tests::{Scene, scene, scene_input};
    let m = deck_metrics();
    let separator = Txt::new(" · ", &m).w(SMALL_PT, false);
    for sc in [Scene::Calm, Scene::Trouble, Scene::Costly] {
        let snap = scene(sc);
        let input = scene_input(&snap, DeckForm::Full);
        let words = DeckWords::new(&input, &m);
        for row in &words.rows {
            for (band, clauses) in row.details.iter().enumerate() {
                for room in [36.0, 90.0, 240.0, 1024.0] {
                    assert_eq!(
                        signature(clauses, before(clauses, room, separator)),
                        signature(clauses, after(clauses, room, separator))
                    );
                    let mut probes: [(&str, Probe); 2] =
                        [("reference", before), ("candidate", after)];
                    for (_, probe) in probes {
                        for _ in 0..100 {
                            black_box(probe(clauses, room, separator));
                        }
                    }
                    for sample in 0..5 {
                        if sample != 0 {
                            probes.swap(0, 1);
                        }
                        for (variant, probe) in probes {
                            let (_, allocations) = aterm_heap_count::allocations_in(|| {
                                probe(clauses, room, separator)
                            });
                            let start = counters();
                            for _ in 0..5_000 {
                                black_box(probe(
                                    black_box(clauses),
                                    black_box(room),
                                    black_box(separator),
                                ));
                            }
                            let instructions = start
                                .zip(counters())
                                .map(|(a, b)| b.instructions.saturating_sub(a.instructions));
                            println!(
                                "details scene={sc:?} resource={:?} band={band} clauses={} room={room} variant={variant} sample={sample} iterations=5000 allocations={allocations} instructions={instructions:?}",
                                row.res,
                                clauses.len()
                            );
                        }
                    }
                }
            }
        }
    }
}
