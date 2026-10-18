// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the fabric lane's build compiles ON THE MEASURED PATH, read from
//! cargo's own plan, so an arm's label is a fact about its build and never an
//! assumption about `Cargo.toml`.
//!
//! WHY READ THE PLAN. A profile's opt-level is not one number: the root
//! manifest's `[profile.dev.package.<name>]` speed rows set a crate at a time,
//! a custom profile inherits the rows of the profile it inherits, and a named
//! row outranks the profile's own `package."*"`. So "this arm is optimized" is
//! checked here, crate by crate, before the lane spends minutes measuring it.
//!
//! THE SOURCE is `targo --unverified test <arm> -p aterm --test fabric_bench
//! --unit-graph -Z unstable-options`: every unit cargo would compile, with its
//! resolved profile and its dependency edges, and nothing compiled. The
//! measured path is what the bench's processes execute: the `fabric_bench` test
//! unit (which holds the in-process broker), the `aterm` binary it launches as
//! the window and the bridge, and every library either links. A build script
//! and a proc macro run inside the compiler, not in a measured process, so the
//! walk does not cross into one, nor into what only they link (`cc`, the
//! `syn` of a derive): those compile under the profile's `build-override`,
//! opt-level 0 in every arm, and say nothing about the measurement.
//!
//! WHAT THE PLAN CANNOT SHOW: the rustflags cargo appends to every rustc line,
//! where a later `-C opt-level` wins over the profile's. Those are
//! [`super::flags`]'s to hold.

use std::collections::{BTreeMap, BTreeSet};

use aterm_json::Value;

/// One unit on the measured path: its package and the code generation it gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unit {
    pub package: String,
    pub opt_level: String,
    pub debug_assertions: bool,
    pub overflow_checks: bool,
}

/// The measured path of one build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Audit {
    pub units: Vec<Unit>,
}

/// A package's name out of a cargo package id: `…/crates/aterm-core#0.106.0`
/// is `aterm-core`, `registry+…#shlex@2.0.1` is `shlex`, and
/// `…/crates/aterm-window#winit@0.30.13` (a path whose package is named
/// otherwise) is `winit`. Pure (unit-tested).
fn package_name(pkg_id: &str) -> &str {
    match pkg_id.rsplit_once('#') {
        Some((_, fragment)) if fragment.contains('@') => {
            fragment.split_once('@').map_or(fragment, |(name, _)| name)
        }
        Some((url, _)) => url.rsplit('/').next().unwrap_or(url),
        None => pkg_id,
    }
}

/// Whether the walk crosses an edge into `unit`: never into a build script's
/// run or compile, never into a proc macro.
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
    /// Read the measured path out of a `--unit-graph` document. Pure
    /// (unit-tested).
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
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Audit { units })
    }

    /// The packages at each opt-level, names sorted and deduplicated (a
    /// package's lib and its binaries are two units of one name).
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
    /// Pure (unit-tested).
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
        format!(
            "{} units on the measured path: {}; {}; {}",
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
        )
    }

    /// Why this build is not the arm it is labelled, or `None`: a unit on the
    /// path below the arm's opt-level (`every_at`, when the arm promises one),
    /// or a unit without debug assertions, which compiles out the seams the
    /// harness drives and would fail the run deep inside it. Pure
    /// (unit-tested).
    pub(crate) fn refusal(&self, every_at: Option<&str>) -> Option<String> {
        let short = |pick: &dyn Fn(&Unit) -> bool| -> Vec<String> {
            let names: BTreeSet<String> = self
                .units
                .iter()
                .filter(|u| pick(u))
                .map(|u| format!("{} (opt-level {})", u.package, u.opt_level))
                .collect();
            names.into_iter().collect()
        };
        if self.units.is_empty() {
            return Some("the unit graph has nothing on the measured path".to_string());
        }
        let no_seams = short(&|u| !u.debug_assertions);
        if !no_seams.is_empty() {
            return Some(format!(
                "built without debug assertions, which the harness's test seams need: {}",
                no_seams.join(", ")
            ));
        }
        let want = every_at?;
        let below = short(&|u| u.opt_level != want);
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

    #[test]
    fn a_package_is_named_from_its_id() {
        for (id, name) in [
            ("path+file:///w/crates/aterm-core#0.106.0", "aterm-core"),
            (
                "registry+https://github.com/rust-lang/crates.io-index#shlex@2.0.1",
                "shlex",
            ),
            ("path+file:///w/crates/aterm-window#winit@0.30.13", "winit"),
            ("bare", "bare"),
        ] {
            assert_eq!(package_name(id), name, "{id}");
        }
    }

    /// A unit of a synthetic graph: `(package, mode, kind, opt-level, deps)`.
    fn unit(package: &str, mode: &str, kind: &str, opt: &str, deps: &[usize]) -> String {
        let deps: Vec<String> = deps.iter().map(|d| format!("{{\"index\":{d}}}")).collect();
        format!(
            "{{\"pkg_id\":\"path+file:///w/crates/{package}#0.1.0\",\"mode\":\"{mode}\",\
             \"target\":{{\"kind\":[\"{kind}\"]}},\"profile\":{{\"opt_level\":\"{opt}\",\
             \"debug_assertions\":true,\"overflow_checks\":false}},\"dependencies\":[{}]}}",
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

    /// The shape a real plan has (read off `aterm`'s on 2026-10-16): the test
    /// links a library whose build script links `cc`, and a derive that links
    /// `syn`; the test depends on the binary it launches.
    fn plan(lib_opt: &str) -> String {
        graph(
            &[
                unit("aterm", "test", "test", "3", &[1, 2, 5]),    // 0
                unit("aterm-core", "build", "lib", lib_opt, &[3]), // 1
                unit("aterm", "build", "bin", "3", &[1]),          // 2
                unit("aterm-core", "run-custom-build", "custom-build", "3", &[4]), // 3
                unit("aterm-core", "build", "custom-build", "0", &[6]), // 4
                unit("aterm-serde-derive", "build", "proc-macro", "0", &[7]), // 5
                unit("cc", "build", "lib", "0", &[]),              // 6
                unit("syn", "build", "lib", "0", &[]),             // 7
            ],
            &[0],
        )
    }

    #[test]
    fn the_walk_keeps_what_runs_and_leaves_build_scripts_and_derives() {
        let audit = Audit::of(&plan("3")).expect("parses");
        let packages: BTreeSet<&str> = audit.units.iter().map(|u| u.package.as_str()).collect();
        assert_eq!(packages, BTreeSet::from(["aterm", "aterm-core"]));
        assert_eq!(audit.units.len(), 3, "the test, the binary and the library");
        // cc and syn are at 0 and say nothing: opt is satisfied.
        assert_eq!(audit.refusal(Some("3")), None);
        assert_eq!(
            audit.summary(),
            "3 units on the measured path: opt-level 3 ×3; debug assertions on; overflow \
             checks off"
        );
    }

    #[test]
    fn a_crate_below_the_promise_is_named_and_the_dev_mix_is_reported() {
        // A dev speed row inherited into an "optimized" profile.
        let audit = Audit::of(&plan("2")).expect("parses");
        let why = audit.refusal(Some("3")).expect("refused");
        assert!(why.contains("aterm-core (opt-level 2)"), "{why}");
        assert!(!why.contains("cc") && !why.contains("syn"), "{why}");
        // The dev arm promises no single level: the same build is reported.
        assert_eq!(audit.refusal(None), None);
        assert_eq!(
            audit.summary(),
            "3 units on the measured path: opt-level 2 ×1 (aterm-core) · opt-level 3 ×2; debug \
             assertions on; overflow checks off"
        );
    }

    #[test]
    fn a_unit_without_debug_assertions_is_refused_in_every_arm() {
        let text = plan("3").replacen("\"debug_assertions\":true", "\"debug_assertions\":false", 1);
        let audit = Audit::of(&text).expect("parses");
        for every_at in [None, Some("3")] {
            let why = audit.refusal(every_at).expect("refused");
            assert!(why.contains("debug assertions"), "{why}");
        }
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
            empty.refusal(None).is_some(),
            "nothing on the path is no build"
        );
    }
}
