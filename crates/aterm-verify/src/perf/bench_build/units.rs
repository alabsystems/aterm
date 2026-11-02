// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fabric perf lane build-plan audit over `--unit-graph` units on the measured path.

use std::collections::{BTreeMap, BTreeSet};

use aterm_json::Value;

use crate::unitgraph::package_name;

/// One unit on the measured path: its package and the code generation it gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unit {
    pub package: String,
    pub opt_level: String,
    pub debug_assertions: bool,
    pub overflow_checks: bool,
    /// The plan's word for the unit's LTO (`true` is fat, `false` is none).
    pub lto: String,
    /// The unit's codegen units, when the profile names them.
    pub codegen_units: Option<u64>,
}

/// The measured path of one build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Audit {
    pub units: Vec<Unit>,
}

/// What an arm promises of every unit on its measured path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Promise {
    /// The opt-level, when the arm promises one.
    pub every_at: Option<&'static str>,
    /// Whether debug assertions are on.
    pub debug_assertions: bool,
    /// The plan's word for the LTO, when the arm promises one.
    pub lto: Option<&'static str>,
    /// The codegen units, when the arm promises them.
    pub codegen_units: Option<u64>,
}

impl Promise {
    /// What `profile`'s arm promises.
    pub(crate) const fn of(profile: super::Profile) -> Self {
        Promise {
            every_at: profile.every_crate_at(),
            debug_assertions: profile.debug_assertions(),
            lto: profile.lto(),
            codegen_units: profile.codegen_units(),
        }
    }
}

fn on_the_path(unit: &Value) -> bool {
    let mode = unit.get("mode").and_then(Value::as_str);
    let compile_time = unit
        .get("target")
        .and_then(|t| t.get("kind"))
        .and_then(Value::as_array)
        .is_some_and(|kinds| {
            kinds
                .iter()
                .any(|k| matches!(k.as_str(), Some("proc-macro" | "custom-build")))
        });
    mode != Some("run-custom-build") && !compile_time
}

impl Audit {
    /// Read the measured path out of a `--unit-graph` document.
    ///
    /// # Errors
    ///
    /// Text that is not a version-1 unit graph, or one whose roots or edges
    /// name no unit.
    pub(crate) fn of(graph: &str) -> Result<Audit, String> {
        let graph: Value =
            aterm_json::from_str(graph).map_err(|e| format!("the unit graph is not JSON: {e}"))?;
        if graph.get("version").and_then(Value::as_u64) != Some(1) {
            return Err("the unit graph is not version 1, the one this lane reads".to_string());
        }
        let units = graph
            .get("units")
            .and_then(Value::as_array)
            .ok_or("the unit graph lists no units")?;
        let index = |v: &Value| -> Result<usize, String> {
            v.as_u64()
                .and_then(|i| usize::try_from(i).ok())
                .filter(|i| *i < units.len())
                .ok_or_else(|| format!("the unit graph names a unit that is not in it: {v:?}"))
        };
        let mut todo: Vec<usize> = graph
            .get("roots")
            .and_then(Value::as_array)
            .ok_or("the unit graph names no roots")?
            .iter()
            .map(index)
            .collect::<Result<_, _>>()?;
        let mut seen = BTreeSet::new();
        while let Some(i) = todo.pop() {
            if !seen.insert(i) {
                continue;
            }
            for dep in units[i]
                .get("dependencies")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let j = index(dep.get("index").unwrap_or(&Value::Null))?;
                if on_the_path(&units[j]) {
                    todo.push(j);
                }
            }
        }
        let units = seen
            .into_iter()
            .map(|i| {
                let unit = &units[i];
                let profile = unit.get("profile");
                let flag = |key: &str| {
                    profile
                        .and_then(|p| p.get(key))
                        .and_then(Value::as_bool)
                        .ok_or_else(|| format!("unit {i} of the graph has no profile `{key}`"))
                };
                Ok(Unit {
                    package: package_name(
                        unit.get("pkg_id").and_then(Value::as_str).unwrap_or("?"),
                    )
                    .to_string(),
                    opt_level: profile
                        .and_then(|p| p.get("opt_level"))
                        .and_then(Value::as_str)
                        .ok_or_else(|| format!("unit {i} of the graph has no opt_level"))?
                        .to_string(),
                    debug_assertions: flag("debug_assertions")?,
                    overflow_checks: flag("overflow_checks")?,
                    lto: profile
                        .and_then(|p| p.get("lto"))
                        .and_then(Value::as_str)
                        .ok_or_else(|| format!("unit {i} of the graph has no lto"))?
                        .to_string(),
                    codegen_units: profile
                        .and_then(|p| p.get("codegen_units"))
                        .and_then(Value::as_u64),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Audit { units })
    }

    fn by_level(&self) -> BTreeMap<&str, (usize, BTreeSet<&str>)> {
        let mut levels: BTreeMap<&str, (usize, BTreeSet<&str>)> = BTreeMap::new();
        for u in &self.units {
            let entry = levels.entry(u.opt_level.as_str()).or_default();
            entry.0 += 1;
            entry.1.insert(u.package.as_str());
        }
        levels
    }

    /// One line for the ledger: how many units, at which opt-levels (naming
    /// the packages of every level but the commonest), and the two checks.
    pub(crate) fn summary(&self) -> String {
        let levels = self.by_level();
        let commonest = levels
            .iter()
            .max_by_key(|(_, (n, _))| *n)
            .map(|(level, _)| *level);
        let levels: Vec<String> = levels
            .iter()
            .map(|(level, (n, names))| {
                if Some(*level) == commonest {
                    format!("opt-level {level} ×{n}")
                } else {
                    let names: Vec<&str> = names.iter().copied().collect();
                    format!("opt-level {level} ×{n} ({})", names.join(", "))
                }
            })
            .collect();
        let switch = |name: &str, on: usize| match on {
            0 => format!("{name} off"),
            n if n == self.units.len() => format!("{name} on"),
            n => format!("{name} on in {n} of {}", self.units.len()),
        };
        let tally = |words: Vec<String>| {
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for word in words {
                *counts.entry(word).or_default() += 1;
            }
            counts
                .into_iter()
                .map(|(word, n)| format!("{word} ×{n}"))
                .collect::<Vec<_>>()
                .join(" · ")
        };
        format!(
            "{} units on the measured path: {}; {}; {}; lto {}; codegen units {}",
            self.units.len(),
            levels.join(" · "),
            switch(
                "debug assertions",
                self.units.iter().filter(|u| u.debug_assertions).count()
            ),
            switch(
                "overflow checks",
                self.units.iter().filter(|u| u.overflow_checks).count()
            ),
            tally(self.units.iter().map(|u| u.lto.clone()).collect()),
            tally(
                self.units
                    .iter()
                    .map(|u| u
                        .codegen_units
                        .map_or_else(|| "default".to_string(), |n| n.to_string()))
                    .collect()
            ),
        )
    }

    /// Why this build is not the arm it is labelled, or `None`: a unit on the
    /// path whose code generation is not what the arm's [`Promise`] says. Debug
    /// assertions off in an arm that promises them compiles out the seams the
    /// harness drives and would fail the run deep inside it; on in the shipped
    /// arm, the numbers are not the shipped code's. Pure (unit-tested).
    pub(crate) fn refusal(&self, promise: Promise) -> Option<String> {
        let short = |pick: &dyn Fn(&Unit) -> bool, of: &dyn Fn(&Unit) -> String| -> Vec<String> {
            let names: BTreeSet<String> = self
                .units
                .iter()
                .filter(|u| pick(u))
                .map(|u| format!("{} ({})", u.package, of(u)))
                .collect();
            names.into_iter().collect()
        };
        if self.units.is_empty() {
            return Some("the unit graph has nothing on the measured path".to_string());
        }
        let level = |u: &Unit| format!("opt-level {}", u.opt_level);
        let assertions = short(&|u| u.debug_assertions != promise.debug_assertions, &level);
        if !assertions.is_empty() {
            return Some(if promise.debug_assertions {
                format!(
                    "built without debug assertions, which the harness's test seams need: {}",
                    assertions.join(", ")
                )
            } else {
                format!(
                    "built with debug assertions, which the shipped code has off: {}",
                    assertions.join(", ")
                )
            });
        }
        if let Some(want) = promise.lto {
            let other = short(&|u| u.lto != want, &|u| format!("lto {}", u.lto));
            if !other.is_empty() {
                return Some(format!(
                    "this arm promises lto {want} on every crate on the measured path, and cargo \
                     plans {} of them otherwise: {}",
                    other.len(),
                    other.join(", ")
                ));
            }
        }
        if let Some(want) = promise.codegen_units {
            let other = short(&|u| u.codegen_units != Some(want), &|u| {
                format!("codegen units {:?}", u.codegen_units)
            });
            if !other.is_empty() {
                return Some(format!(
                    "this arm promises {want} codegen unit(s) on every crate on the measured \
                     path, and cargo plans {} of them otherwise: {}",
                    other.len(),
                    other.join(", ")
                ));
            }
        }
        let want = promise.every_at?;
        let below = short(&|u| u.opt_level != want, &level);
        (!below.is_empty()).then(|| {
            format!(
                "this arm promises opt-level {want} on every crate on the measured path, and \
                 cargo plans {} of them at another level: {}",
                below.len(),
                below.join(", ")
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perf::bench_build::bench_profile::Profile;

    #[test]
    fn a_package_is_named_from_its_id() {
        for (id, name) in [
            ("path+file:///w/crates/aterm-core#0.106.0", "aterm-core"),
            (
                "registry+https://github.com/rust-lang/crates.io-index#shlex@2.0.1",
                "shlex",
            ),
            ("path+file:///w/crates/aterm-log-shim#log@0.4.32", "log"),
            ("bare", "bare"),
        ] {
            assert_eq!(package_name(id), name, "{id}");
        }
    }

    fn unit(package: &str, mode: &str, kind: &str, opt: &str, deps: &[usize]) -> String {
        let deps: Vec<String> = deps.iter().map(|d| format!("{{\"index\":{d}}}")).collect();
        format!(
            "{{\"pkg_id\":\"path+file:///w/crates/{package}#0.1.0\",\"mode\":\"{mode}\",\
             \"target\":{{\"kind\":[\"{kind}\"]}},\"profile\":{{\"opt_level\":\"{opt}\",\
             \"debug_assertions\":true,\"overflow_checks\":false,\"lto\":\"false\",\
             \"codegen_units\":16}},\"dependencies\":[{}]}}",
            deps.join(",")
        )
    }

    fn graph(units: &[String], roots: &[usize]) -> String {
        let roots: Vec<String> = roots.iter().map(usize::to_string).collect();
        format!(
            "{{\"version\":1,\"units\":[{}],\"roots\":[{}]}}",
            units.join(","),
            roots.join(",")
        )
    }

    fn plan(lib_opt: &str) -> String {
        graph(
            &[
                unit("aterm", "bench", "bench", "3", &[1, 2, 5]), // 0
                unit("aterm-core", "build", "lib", lib_opt, &[3]), // 1
                unit("aterm", "build", "bin", "3", &[1]),         // 2
                unit("aterm-core", "run-custom-build", "custom-build", "3", &[4]), // 3
                unit("aterm-core", "build", "custom-build", "0", &[6]), // 4
                unit("aterm-serde-derive", "build", "proc-macro", "0", &[7]), // 5
                unit("cc", "build", "lib", "0", &[]),             // 6
                unit("syn", "build", "lib", "0", &[]),            // 7
            ],
            &[0],
        )
    }

    /// The fat-LTO, one-unit, assertions-off plan of the shipped arm, `edit` applied to its text:
    /// a first occurrence is the root bench's, a unit on the path.
    fn shipped_plan(edit: fn(String) -> String) -> String {
        let all = plan("3")
            .replace("\"debug_assertions\":true", "\"debug_assertions\":false")
            .replace("\"lto\":\"false\"", "\"lto\":\"true\"")
            .replace("\"codegen_units\":16", "\"codegen_units\":1");
        edit(all)
    }

    #[test]
    fn the_walk_keeps_what_runs_and_leaves_build_scripts_and_derives() {
        let audit = Audit::of(&plan("3")).expect("parses");
        let packages: BTreeSet<&str> = audit.units.iter().map(|u| u.package.as_str()).collect();
        assert_eq!(packages, BTreeSet::from(["aterm", "aterm-core"]));
        assert_eq!(
            audit.units.len(),
            3,
            "the bench, the binary and the library"
        );
        assert_eq!(audit.refusal(Promise::of(Profile::Opt)), None);
        assert_eq!(
            audit.summary(),
            "3 units on the measured path: opt-level 3 ×3; debug assertions on; overflow \
             checks off; lto false ×3; codegen units 16 ×3"
        );
    }

    #[test]
    fn a_crate_below_the_promise_is_named_and_the_dev_mix_is_reported() {
        let audit = Audit::of(&plan("2")).expect("parses");
        let why = audit.refusal(Promise::of(Profile::Opt)).expect("refused");
        assert!(why.contains("aterm-core (opt-level 2)"), "{why}");
        assert!(!why.contains("cc") && !why.contains("syn"), "{why}");
        assert_eq!(audit.refusal(Promise::of(Profile::Dev)), None);
        assert_eq!(
            audit.summary(),
            "3 units on the measured path: opt-level 2 ×1 (aterm-core) · opt-level 3 ×2; debug \
             assertions on; overflow checks off; lto false ×3; codegen units 16 ×3"
        );
    }

    #[test]
    fn a_unit_without_debug_assertions_is_refused_where_the_arm_needs_them() {
        let text = plan("3").replacen("\"debug_assertions\":true", "\"debug_assertions\":false", 1);
        let audit = Audit::of(&text).expect("parses");
        for promise in [Promise::of(Profile::Dev), Promise::of(Profile::Opt)] {
            let why = audit.refusal(promise).expect("refused");
            assert!(why.contains("without debug assertions"), "{why}");
        }
    }

    #[test]
    fn the_shipped_arm_holds_its_lto_codegen_units_and_assertions_to_the_plan() {
        let promise = Promise::of(Profile::Shipped);
        let audit = Audit::of(&shipped_plan(|all| all)).expect("parses");
        assert_eq!(audit.refusal(promise), None);
        assert_eq!(
            audit.summary(),
            "3 units on the measured path: opt-level 3 ×3; debug assertions off; overflow \
             checks off; lto true ×3; codegen units 1 ×3"
        );
        for (edit, says) in [
            (
                (|all: String| all.replacen("\"lto\":\"true\"", "\"lto\":\"thin\"", 1))
                    as fn(String) -> String,
                "lto true",
            ),
            (
                |all: String| all.replacen("\"codegen_units\":1", "\"codegen_units\":16", 1),
                "1 codegen unit",
            ),
            (
                |all: String| all.replacen("\"codegen_units\":1", "\"codegen_units\":null", 1),
                "1 codegen unit",
            ),
            (
                |all: String| {
                    all.replacen("\"debug_assertions\":false", "\"debug_assertions\":true", 1)
                },
                "with debug assertions",
            ),
        ] {
            let why = Audit::of(&shipped_plan(edit))
                .expect("parses")
                .refusal(promise)
                .expect("refused");
            assert!(why.contains(says), "{says}: {why}");
        }
        let plain_release =
            plan("3").replace("\"debug_assertions\":true", "\"debug_assertions\":false");
        assert!(
            Audit::of(&plain_release)
                .expect("parses")
                .refusal(promise)
                .is_some(),
            "16 units and no LTO are not the shipped code"
        );
    }

    #[test]
    fn a_graph_this_lane_cannot_read_is_an_error() {
        for bad in [
            "not json",
            "{\"version\":2,\"units\":[],\"roots\":[]}",
            "{\"version\":1,\"roots\":[0]}",
            "{\"version\":1,\"units\":[],\"roots\":[0]}",
        ] {
            assert!(Audit::of(bad).is_err(), "{bad}");
        }
        let empty = Audit::of("{\"version\":1,\"units\":[],\"roots\":[]}").expect("parses");
        assert!(
            empty.refusal(Promise::of(Profile::Dev)).is_some(),
            "nothing on the path is no build"
        );
    }
}
