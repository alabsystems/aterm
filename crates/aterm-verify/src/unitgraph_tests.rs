// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A unit graph of `units`, each `(package, target, kind, mode, opt_level, features, deps)`.
/// A fixture unit: `(package, target, kind, mode, opt_level, features, deps)`.
pub(crate) type Fixture<'a> = (
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    &'a [&'a str],
    &'a [usize],
);

pub(crate) fn graph(units: &[Fixture<'_>]) -> String {
    graph_in(
        units,
        if units.is_empty() { &[] } else { &[0] },
        &vec![None; units.len()],
    )
}

pub(crate) fn graph_in(
    units: &[Fixture<'_>],
    roots: &[usize],
    platforms: &[Option<&str>],
) -> String {
    assert_eq!(units.len(), platforms.len());
    let units: Vec<String> = units
        .iter()
        .zip(platforms)
        .map(
            |((pkg, target, kind, mode, opt, features, deps), platform)| {
                let crate_type = if matches!(*kind, "test" | "bench" | "example" | "custom-build") {
                    "bin"
                } else {
                    kind
                };
                let platform = platform.map_or("null".to_owned(), |p| format!("\"{p}\""));
                let features: Vec<String> = features.iter().map(|f| format!("\"{f}\"")).collect();
                let deps: Vec<String> = deps
                    .iter()
                    .map(|d| {
                        let name = units.get(*d).map_or("gone", |u| u.1);
                        format!(
                            "{{\"index\":{d},\"extern_crate_name\":\"{name}\",\
                         \"public\":false,\"noprelude\":false}}"
                        )
                    })
                    .collect();
                format!(
                    "{{\"pkg_id\":\"path+file:///w/crates/{pkg}#0.1.0\",\"target\":{{\"kind\":\
                 [\"{kind}\"],\"crate_types\":[\"{crate_type}\"],\"name\":\"{target}\"}},\
                 \"profile\":{{\"name\":\"test\",\"opt_level\":\"{opt}\",\
                 \"debug_assertions\":true}},\"platform\":{platform},\"mode\":\"{mode}\",\
                 \"features\":[{}],\"dependencies\":[{}]}}",
                    features.join(","),
                    deps.join(",")
                )
            },
        )
        .collect();
    format!(
        "{{\"version\":1,\"units\":[{}],\"roots\":[{}]}}",
        units.join(","),
        roots
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Two graphs read through one interner hold one identity for one unit, and another for a
/// unit with another feature, profile field or dependency anywhere below it: a narrower call
/// whose lib and its dependency match the wider call's builds nothing outside it, one whose
/// dependency has a feature the wider's lacks builds two units outside it (the dependency and
/// the lib above it), and one at another opt-level builds its unit outside. NEGATIVE CONTROL:
/// by what names each unit alone (package, crate, mode), the featured call reads as within.
#[test]
fn a_unit_is_the_same_only_with_the_same_features_profile_and_dependencies() {
    let wide = graph(&[
        ("a", "a", "lib", "test", "0", &[], &[1, 2]),
        ("b", "b", "lib", "build", "0", &["std"], &[]),
        ("c", "c", "lib", "build", "0", &[], &[]),
    ]);
    let narrow = graph(&[
        ("b", "b", "lib", "build", "0", &["std"], &[]),
        ("c", "c", "lib", "build", "0", &[], &[]),
        ("a", "a", "lib", "test", "0", &[], &[0, 1]),
    ]);
    let featured = graph(&[
        ("a", "a", "lib", "test", "0", &[], &[1, 2]),
        ("b", "b", "lib", "build", "0", &["std", "serde"], &[]),
        ("c", "c", "lib", "build", "0", &[], &[]),
    ]);
    let optimised = graph(&[("c", "c", "lib", "build", "3", &[], &[])]);
    let mut i = Interner::default();
    let wide_g = Graph::read(&wide, &mut i).expect("wide");
    let narrow_g = Graph::read(&narrow, &mut i).expect("narrow");
    let featured_g = Graph::read(&featured, &mut i).expect("featured");
    let optimised_g = Graph::read(&optimised, &mut i).expect("optimised");
    assert!(narrow_g.within(&wide_g));
    assert_eq!(featured_g.outside(&wide_g), 2);
    assert_eq!(optimised_g.outside(&wide_g), 1);
    assert!(wide_g.tests_lib_of("a") && !wide_g.tests_lib_of("b"));
    let names: Vec<String> = wide_g
        .names()
        .into_iter()
        .map(|n| format!("{} {}", n.package, n.kind))
        .collect();
    assert_eq!(names, ["a a test", "b b lib", "c c lib"]);
    assert!(
        featured_g.names().is_subset(&wide_g.names()),
        "by package, crate and mode alone the featured call would pass"
    );
}

/// A UNION KEEPS ONE SHARED UNIT AND REMAPS BOTH GRAPHS' EDGES AND ROOTS: `a` and `c` each
/// depend on the same `b`, so the joined graph has three units, two roots and both dependencies
/// name its one `b`. NEGATIVE CONTROL: the two input graphs contain four unit rows in total.
#[test]
fn a_union_deduplicates_units_and_remaps_dependencies_and_roots() {
    let left = graph(&[
        ("a", "a", "lib", "build", "0", &[], &[1]),
        ("b", "b", "lib", "build", "0", &[], &[]),
    ]);
    let right = graph(&[
        ("c", "c", "lib", "build", "0", &[], &[1]),
        ("b", "b", "lib", "build", "0", &[], &[]),
    ]);
    let mut interner = Interner::default();
    let left = Graph::read(&left, &mut interner).expect("left");
    let right = Graph::read(&right, &mut interner).expect("right");
    assert_eq!(left.units.len() + right.units.len(), 4);
    let joined = Graph::union([left, right]);
    assert_eq!(joined.units.len(), 3);
    let roots: BTreeSet<&str> = joined
        .roots
        .iter()
        .map(|root| joined.units[*root].package_id.as_str())
        .collect();
    assert!(roots.iter().any(|root| root.contains("/a#")));
    assert!(roots.iter().any(|root| root.contains("/c#")));
    assert_eq!(roots.len(), 2);
    for unit in joined
        .units
        .iter()
        .filter(|unit| unit.package_id.contains("/a#") || unit.package_id.contains("/c#"))
    {
        assert_eq!(unit.dependencies.len(), 1);
        assert!(
            joined.units[unit.dependencies[0]]
                .package_id
                .contains("/b#")
        );
    }
}

/// A graph that is not one is refused, naming why: not JSON, another version, a dependency
/// outside it, a cycle. NEGATIVE CONTROL: the same units with the edge in range read.
#[test]
fn a_graph_that_is_not_one_is_refused() {
    let mut i = Interner::default();
    assert!(Graph::read("{", &mut i).is_err());
    assert!(
        Graph::read("{\"version\":2,\"units\":[],\"roots\":[]}", &mut i)
            .expect_err("version")
            .contains("version 1")
    );
    let outside = graph(&[("a", "a", "lib", "build", "0", &[], &[5])]);
    assert!(Graph::read(&outside, &mut i).is_err());
    let cycle = graph(&[
        ("a", "a", "lib", "build", "0", &[], &[1]),
        ("b", "b", "lib", "build", "0", &[], &[0]),
    ]);
    assert!(
        Graph::read(&cycle, &mut i)
            .expect_err("cycle")
            .contains("depends on itself")
    );
    let fine = graph(&[
        ("a", "a", "lib", "build", "0", &[], &[1]),
        ("b", "b", "lib", "build", "0", &[], &[]),
    ]);
    assert_eq!(Graph::read(&fine, &mut i).expect("reads").units.len(), 2);
    for bad in [
        fine.replace("\"roots\":[0]", "\"roots\":[2]"),
        fine.replace("\"roots\":[0]", "\"roots\":[0,0]"),
        fine.replace(",\"roots\":[0]", ""),
        fine.replace("\"platform\":null,", ""),
        fine.replace("\"platform\":null", "\"platform\":true"),
        fine.replace("\"features\":[]", "\"features\":null"),
        fine.replace("\"dependencies\":[]", "\"dependencies\":null"),
        fine.replace("\"public\":false,", ""),
        fine.replace("\"extern_crate_name\":\"b\",", ""),
        fine.replace("\"kind\":[\"lib\"]", "\"kind\":[]"),
        fine.replace("\"mode\":\"build\"", "\"mode\":\"\""),
    ] {
        assert!(Graph::read(&bad, &mut i).is_err(), "{bad}");
    }
    assert_eq!(
        package_name("registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0"),
        "serde"
    );
    assert_eq!(
        package_name("path+file:///w/crates/aterm-gui#0.147.0"),
        "aterm-gui"
    );
    // A deep acyclic dependency chain reads iteratively without overflowing a small stack.
    let depth = 4096usize;
    let edges: Vec<[usize; 1]> = (0..depth).map(|k| [k + 1]).collect();
    let chain_fixtures: Vec<Fixture<'_>> = (0..depth)
        .map(|k| {
            let deps: &[usize] = if k + 1 < depth { &edges[k] } else { &[] };
            ("chain", "chain", "lib", "build", "0", &[][..], deps)
        })
        .collect();
    let chain_json = graph(&chain_fixtures);
    let deep = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || Graph::read(&chain_json, &mut Interner::default()))
        .expect("spawn")
        .join()
        .expect("join")
        .expect("deep chain reads");
    assert_eq!(deep.units.len(), depth);
}
