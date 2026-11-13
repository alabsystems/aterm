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
    /// Whether Cargo plans incremental compilation for this unit.
    pub incremental: bool,
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
    /// Whether overflow checks are on.
    pub overflow_checks: bool,
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
            overflow_checks: matches!(profile, super::Profile::Dev),
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
                    incremental: flag("incremental")?,
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
            "{} units on the measured path: {}; {}; {}; {}; lto {}; codegen units {}",
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
            switch(
                "incremental compilation",
                self.units.iter().filter(|u| u.incremental).count()
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
        let overflows = short(&|u| u.overflow_checks != promise.overflow_checks, &|u| {
            format!(
                "overflow checks {}",
                if u.overflow_checks { "on" } else { "off" }
            )
        });
        if !overflows.is_empty() {
            return Some(format!(
                "this arm promises overflow checks {} on every crate on the measured path, and cargo plans {} otherwise: {}",
                if promise.overflow_checks { "on" } else { "off" },
                overflows.len(),
                overflows.join(", ")
            ));
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
#[path = "units_tests.rs"]
mod tests;
