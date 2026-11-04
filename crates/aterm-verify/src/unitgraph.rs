// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT A CARGO CALL WOULD BUILD, UNIT BY UNIT: a `--unit-graph` document (version 1) read into
//! each unit's exact identity, so two calls' graphs compare by the units they build
//! ([`Graph::within`], VERIFY-SHAPE's proof that a cut builds nothing the whole call does not).
//!
//! A unit's identity is its package, its target's name, kinds and crate types, its mode, every
//! field of its profile, its sorted features, its platform, and each of its dependencies' extern
//! name, `public` and `noprelude` flags and identity, recursively: what cargo hashes into the
//! unit's metadata, so a unit with another feature, profile field or dependency anywhere below it
//! is another unit. Identities are interned ([`Interner`]): two graphs read through one interner
//! hold the same number for the same unit, exactly, with no hash to collide.

use std::collections::{BTreeMap, BTreeSet};

use aterm_json::Value;

/// The package name a unit graph's `pkg_id` names: `path+file:///…/aterm-gui#0.147.0` names
/// `aterm-gui`, `registry+https://…#serde@1.0.0` names `serde`.
#[must_use]
pub fn package_name(pkg_id: &str) -> &str {
    match pkg_id.rsplit_once('#') {
        Some((_, fragment)) if fragment.contains('@') => {
            fragment.split_once('@').map_or(fragment, |(name, _)| name)
        }
        Some((url, _)) => url.rsplit('/').next().unwrap_or(url),
        None => pkg_id,
    }
}

/// The identities of every unit read so far, by their canonical text ([`Graph::read`]).
#[derive(Debug, Default)]
pub struct Interner {
    ids: BTreeMap<String, u32>,
}

/// What a unit is named by where a compiler of it runs: its package, and its crate name with
/// whether it compiles as a test (`aterm_gui test`) or the crate type it builds (`aterm_gui lib`,
/// `build_script_build bin`): what a `rustc` command line's `--crate-name`, `--test` and
/// `--crate-type` say, with `CARGO_PKG_NAME` in its environment ([`crate::memory::UnitName`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnitName {
    /// The package.
    pub package: String,
    /// `<crate name> test`, or `<crate name> <crate type>`.
    pub kind: String,
}

/// One unit of a graph: its identity, and what names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    /// Its identity in the graph's [`Interner`].
    pub id: u32,
    /// What names it where it compiles; `None` for a build script's run, which compiles nothing.
    pub name: Option<UnitName>,
    /// Whether it is a library target compiled as a test (`mode` `test`): a lib that is tested.
    pub lib_test: bool,
    /// Cargo's full package identity, including source and version.
    pub package_id: String,
    /// The target triple, or `None` for a host unit. Never inferred from its package name.
    pub platform: Option<String>,
    /// Cargo target kinds, including proc-macro and custom-build distinctions.
    pub target_kinds: Vec<String>,
    /// Cargo's compilation mode.
    pub mode: String,
    /// The resolved features of this particular unit, not a package-wide union.
    pub features: Vec<String>,
    /// Dependency indices in this graph, retaining host/target and root-dev contexts.
    pub dependencies: Vec<usize>,
}

/// WHAT ONE CALL WOULD BUILD: every unit of its `--unit-graph`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Graph {
    /// Each unit, in the graph's order.
    pub units: Vec<Unit>,
    /// Cargo's selected compilation roots, as indices into `units`.
    pub roots: Vec<usize>,
}

impl Graph {
    /// Read a `--unit-graph` document, interning each unit's identity in `interner`.
    ///
    /// # Errors
    /// Invalid JSON or a version other than 1, missing or malformed package/target/mode/profile/
    /// feature/platform/dependency context, an invalid or repeated root, an out-of-range
    /// dependency index, or a dependency cycle.
    pub fn read(text: &str, interner: &mut Interner) -> Result<Graph, String> {
        let graph: Value =
            aterm_json::from_str(text).map_err(|e| format!("the unit graph is not JSON: {e}"))?;
        if graph.get("version").and_then(Value::as_u64) != Some(1) {
            return Err("the unit graph is not version 1, the one verify reads".to_owned());
        }
        let units = graph
            .get("units")
            .and_then(Value::as_array)
            .ok_or("the unit graph lists no units")?;
        let indices = |value: &Value, what: &str| -> Result<Vec<usize>, String> {
            value
                .as_array()
                .ok_or_else(|| format!("the unit graph has no {what} array"))?
                .iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .filter(|n| *n < units.len())
                        .ok_or_else(|| format!("the unit graph's {what} names a unit not in it"))
                })
                .collect()
        };
        let roots = indices(
            graph.get("roots").ok_or("the unit graph has no roots")?,
            "roots",
        )?;
        if roots.iter().collect::<BTreeSet<_>>().len() != roots.len() {
            return Err("the unit graph repeats a root index".into());
        }
        // Validate required Cargo context before interning; missing platform is not host,
        // and a missing dependency/feature list is not an empty one.
        for (i, unit) in units.iter().enumerate() {
            let string = |value: Option<&Value>, field: &str| -> Result<(), String> {
                value
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(|_| ())
                    .ok_or_else(|| format!("unit {i} has no valid {field}"))
            };
            string(unit.get("pkg_id"), "pkg_id")?;
            string(unit.get("mode"), "mode")?;
            if !matches!(unit.get("profile"), Some(Value::Object(_))) {
                return Err(format!("unit {i} has no profile object"));
            }
            let target = unit
                .get("target")
                .ok_or_else(|| format!("unit {i} has no target"))?;
            string(target.get("name"), "target name")?;
            for (value, field, nonempty) in [
                (target.get("kind"), "target kind", true),
                (target.get("crate_types"), "crate types", true),
                (unit.get("features"), "features", false),
            ] {
                let list = value
                    .and_then(Value::as_array)
                    .ok_or_else(|| format!("unit {i} has no {field} array"))?;
                if nonempty && list.is_empty()
                    || list.iter().any(|v| v.as_str().is_none_or(str::is_empty))
                {
                    return Err(format!("unit {i} has invalid {field}"));
                }
            }
            match unit.get("platform") {
                Some(Value::Null) => {}
                Some(v) if v.as_str().is_some_and(|s| !s.is_empty()) => {}
                _ => return Err(format!("unit {i} has no valid platform")),
            }
            let deps = unit
                .get("dependencies")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("unit {i} has no dependencies array"))?;
            for dep in deps {
                string(dep.get("extern_crate_name"), "dependency extern crate name")?;
                for flag in ["public", "noprelude"] {
                    if dep.get(flag).and_then(Value::as_bool).is_none() {
                        return Err(format!("unit {i} has no dependency {flag} flag"));
                    }
                }
                let _ = dep
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n < units.len())
                    .ok_or_else(|| format!("unit {i} names a dependency not in it"))?;
            }
        }
        let mut ids: Vec<Option<u32>> = vec![None; units.len()];
        let mut path = vec![false; units.len()];
        let mut out = Vec::with_capacity(units.len());
        for i in 0..units.len() {
            let id = identity(units, i, &mut ids, &mut path, interner)?;
            let unit = &units[i];
            let target = unit.get("target");
            let strings = |key: &str| -> Vec<&str> {
                target
                    .and_then(|t| t.get(key))
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default()
            };
            let mode = unit.get("mode").and_then(Value::as_str).unwrap_or("");
            let crate_name = target
                .and_then(|t| t.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .replace('-', "_");
            let package = package_name(unit.get("pkg_id").and_then(Value::as_str).unwrap_or(""));
            let name = match mode {
                "test" => Some(format!("{crate_name} test")),
                "build" | "check" => strings("crate_types")
                    .first()
                    .map(|t| format!("{crate_name} {t}")),
                _ => None,
            }
            .map(|kind| UnitName {
                package: package.to_owned(),
                kind,
            });
            out.push(Unit {
                id,
                name,
                lib_test: mode == "test" && strings("kind").contains(&"lib"),
                package_id: unit
                    .get("pkg_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                platform: unit
                    .get("platform")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                target_kinds: strings("kind").into_iter().map(str::to_owned).collect(),
                mode: mode.to_owned(),
                features: unit
                    .get("features")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                dependencies: unit
                    .get("dependencies")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|d| {
                        d.get("index")
                            .and_then(Value::as_u64)
                            .and_then(|n| usize::try_from(n).ok())
                    })
                    .collect(),
            });
        }
        Ok(Graph { units: out, roots })
    }

    /// The identities of its units.
    #[must_use]
    pub fn ids(&self) -> BTreeSet<u32> {
        self.units.iter().map(|u| u.id).collect()
    }

    /// How many of its units `wider` does not build (`0`: every unit it builds, `wider` builds
    /// too, with the same features, profile and dependencies), both read through one interner.
    #[must_use]
    pub fn outside(&self, wider: &Graph) -> usize {
        let theirs = wider.ids();
        self.ids().difference(&theirs).count()
    }

    /// Whether `wider` builds every unit it does ([`Self::outside`]).
    #[must_use]
    pub fn within(&self, wider: &Graph) -> bool {
        self.outside(wider) == 0
    }

    /// Whether it compiles package `package`'s library as a test: its lib is tested.
    #[must_use]
    pub fn tests_lib_of(&self, package: &str) -> bool {
        self.units
            .iter()
            .any(|u| u.lib_test && u.name.as_ref().is_some_and(|n| n.package == package))
    }

    /// What names each unit it compiles, each once.
    #[must_use]
    pub fn names(&self) -> BTreeSet<&UnitName> {
        self.units.iter().filter_map(|u| u.name.as_ref()).collect()
    }
}

/// Unit `i`'s interned identity, its dependencies' first; `ids` memoises them and `path` holds
/// the units being read, which a dependency back onto one would make a cycle.
fn identity(
    units: &[Value],
    start: usize,
    ids: &mut [Option<u32>],
    path: &mut [bool],
    interner: &mut Interner,
) -> Result<u32, String> {
    if let Some(id) = ids[start] {
        return Ok(id);
    }
    let mut stack = vec![(start, 0usize)];
    'walk: while let Some((i, mut next_dep)) = stack.pop() {
        let unit = &units[i];
        let field = |key: &str| {
            unit.get(key)
                .ok_or_else(|| format!("unit {i} of the unit graph has no `{key}`"))
        };
        if next_dep == 0 {
            if ids[i].is_some() {
                continue;
            }
            if path[i] {
                return Err(format!("the unit graph's unit {i} depends on itself"));
            }
            let _ = field("target")?;
            path[i] = true;
        }
        let dep_list = unit
            .get("dependencies")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        while next_dep < dep_list.len() {
            let dep = &dep_list[next_dep];
            let j = dep
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|j| usize::try_from(j).ok())
                .filter(|j| *j < units.len())
                .ok_or_else(|| {
                    format!("unit {i} of the unit graph names a dependency not in it")
                })?;
            next_dep += 1;
            if ids[j].is_none() {
                if path[j] {
                    return Err(format!("the unit graph's unit {j} depends on itself"));
                }
                stack.push((i, next_dep));
                stack.push((j, 0));
                continue 'walk;
            }
        }
        let target = field("target")?;
        let sorted = |v: Option<&Value>| -> Vec<String> {
            let mut s: Vec<String> = v
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            s.sort();
            s
        };
        let mut deps = Vec::with_capacity(dep_list.len());
        for dep in dep_list {
            let j = dep
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|j| usize::try_from(j).ok())
                .unwrap_or(0);
            let id = ids[j].unwrap_or(u32::MAX);
            let flag = |key: &str| dep.get(key).and_then(Value::as_bool).unwrap_or(false);
            deps.push(format!(
                "{}|{}|{}|{id}",
                dep.get("extern_crate_name")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                flag("public"),
                flag("noprelude"),
            ));
        }
        deps.sort();
        let text = |v: &Value| aterm_json::to_string(v).unwrap_or_default();
        let canonical = [
            text(field("pkg_id")?),
            text(target.get("name").unwrap_or(&Value::Null)),
            sorted(target.get("kind")).join(","),
            sorted(target.get("crate_types")).join(","),
            text(field("mode")?),
            text(field("profile")?),
            sorted(unit.get("features")).join(","),
            text(unit.get("platform").unwrap_or(&Value::Null)),
            deps.join(";"),
        ]
        .join("\n");
        let next = u32::try_from(interner.ids.len()).unwrap_or(u32::MAX);
        let id = *interner.ids.entry(canonical).or_insert(next);
        ids[i] = Some(id);
        path[i] = false;
    }
    Ok(ids[start].unwrap_or(u32::MAX))
}

#[cfg(test)]
pub(crate) mod tests {
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
                    let crate_type =
                        if matches!(*kind, "test" | "bench" | "example" | "custom-build") {
                            "bin"
                        } else {
                            kind
                        };
                    let platform = platform.map_or("null".to_owned(), |p| format!("\"{p}\""));
                    let features: Vec<String> =
                        features.iter().map(|f| format!("\"{f}\"")).collect();
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
}
