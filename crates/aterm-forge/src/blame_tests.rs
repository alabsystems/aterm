// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn toy() -> Graph {
    let (r, a, b, c, d) = (
        PkgId::new("root", "1"),
        PkgId::new("a", "1"),
        PkgId::new("b", "1"),
        PkgId::new("c", "1"),
        PkgId::new("d", "1"),
    );
    let mut g = Graph {
        root: r.clone(),
        ..Graph::default()
    };
    for n in [&r, &a, &b, &c, &d] {
        g.nodes.insert(n.clone());
    }
    g.edges
        .entry(r.clone())
        .or_default()
        .extend([a.clone(), b.clone()]);
    g.edges.entry(a.clone()).or_default().insert(c.clone());
    g.edges.entry(b.clone()).or_default().insert(c.clone());
    g.edges.entry(c.clone()).or_default().insert(d.clone());
    g
}

#[test]
fn every_path_is_counted_and_the_shortest_come_first() {
    let g = toy();
    let d = PkgId::new("d", "1");
    assert_eq!(count_paths(&g, &d), (2, false));
    let (paths, hit) = shortest_paths(&g, &d, 10);
    assert!(!hit);
    assert_eq!(paths.len(), 2);
    for p in &paths {
        assert_eq!(p.first(), Some(&g.root));
        assert_eq!(p.last(), Some(&d));
        assert_eq!(p.len(), 4);
    }
    assert_eq!(count_paths(&g, &g.root), (1, false));
    let (paths, _) = shortest_paths(&g, &PkgId::new("c", "1"), 1);
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].len(), 3);
    let ghost = PkgId::new("ghost", "9");
    assert_eq!(shortest_paths(&g, &ghost, 10).0.len(), 0);
    assert_eq!(count_paths(&g, &ghost), (0, false));
}

#[test]
fn write_path_respects_w_line_for_short_segments() {
    let chain: Vec<PkgId> = (0..14)
        .map(|i| PkgId::new(format!("aterm-crate-{i:02}"), "0.142.0"))
        .collect();
    let mut out = String::new();
    write_path(&mut out, "      [ 1] ", &chain);
    for line in out.lines() {
        assert!(
            line.chars().count() <= W_LINE,
            "path line exceeded W_LINE ({W_LINE}): len={} {line:?}",
            line.chars().count()
        );
    }
    assert!(out.contains("aterm-crate-00 0.142.0 ->"));
    assert!(out.trim_end().ends_with("aterm-crate-13 0.142.0"));
}
