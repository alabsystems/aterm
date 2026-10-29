// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `codex-measure [--package DIR] [--keep]`: MEASURE A CODEX-ALAB BUILD for aterm's rollout tap,
//! or refuse with one named class and leave the checkout as it was.
//!
//! Ruling T8: a codex build's rollout is read only once that build is measured, matched whole.
//! A build is measured when its six legs' rollouts are committed reduced in aterm-agent's step
//! testdata, since `CODEX_MEASURED_BUILDS` is the set of builds those reductions name
//! (aterm-agent's `build.rs`, which reads the files a checkout holds, so the verb reads the step
//! testdata only as HEAD holds it). The verb does what a person did by hand for 12672, 13172 and
//! 13594, except that it writes no golden: one written from its own fold would match by
//! construction, so a build it measures is held to its rollout's own account and to the keys and
//! record kinds every measured build showed. The steps it shares with `lab claude-measure` are
//! [`crate::measure`]'s, read through [`CODEX`].
//!
//! 1. the key: the build the package's `codex` runs as its rollouts name it ([`alab_build`]);
//!    NOT RUN when a codex reduction in the step testdata is not as HEAD holds it, such as a
//!    cut-short run's leftover ([`committed_in`]); exit 0, writing nothing, when HEAD holds all
//!    six of its legs; refuse STEM when it is no codex-alab build, when only some of its legs are
//!    committed, or when its reductions' names are another build's ([`admit`]);
//! 2. build aterm (`targo --unverified build -p aterm`), under the measurements' one lock from
//!    here to the end ([`hold_clean`]; each proof run holds the proof's own lock as well);
//! 3. run 1, the proof in process expecting `partial` ([`codex_alab_proof::run`]); refuse
//!    PROOF-RED on any FAIL ([`gate`]);
//! 4. each leg's one rollout (ROLLOUTS), every `session_meta` naming the key (KEY), reduced twice
//!    and a third pass byte-identical ([`crate::measure::fixed_point`], FIXED-POINT), its own
//!    line naming the key (KEY) ([`reduce_leg`]);
//! 5. the keys and record kinds against every committed codex-alab reduction (NEW-KEY) and the
//!    rollout keys the previous build's same leg showed (GONE) ([`judge`]); other drift is noted;
//! 6. the six reductions written create-new, each path said as it is made;
//! 7. the standing tests ([`STANDING`]), the crates named (STANDING,
//!    [`crate::measure::standing_verdict`]);
//! 8. aterm rebuilt and run 2 expecting `bound` (NOT-BOUND);
//! 9. the note, `docs/measured/codex-alab-proof-<count>-<date>.md`, only when all passed;
//! 10. both worlds removed (kept and named on a refusal, or with `--keep`).
//!
//! A refusal after step 6, an interrupt or a panic removes exactly the files the run created
//! ([`landed`], and [`crate::measure::Written`]'s drop), and a second interrupt, which ends the
//! process at once, removes them first ([`undo_on_hard_exit`]). Only a kill leaves them, and
//! step 1 of the next run refuses them as leftovers. An interrupt is the run's (exit 128 + the
//! signal), never a step's refusal. What stays a person's or an agent's: the CHANGELOG line, the
//! commit, the merge and the push.

use std::path::{Path, PathBuf};

use aterm_agent::harness::codex_alab::{
    ALAB_BUILD_JSON, PACKAGE_JSON, alab_build, package_entrypoint,
};
use aterm_json::Value;
use aterm_phase::anchors::{ROLLOUT_KEYS, rollout_reduction_stem};

use crate::codex_alab_proof::{self, Run};
use crate::instance::{take_flag, take_value};
use crate::measure::{
    Class, Family, Judged, MEASURED, Reduction, Refusal, STEPS, build_aterm, committed_in, gate,
    hold_clean, judge, landed, not_run, order_of, reduce_checked, reduce_leg, rows, sha256,
    standing, standing_line, today, undo_on_hard_exit, with_world, word,
};
use crate::paths::paths;
use crate::reduce::Program;
use crate::{Fail, LAB, Res};

/// What `codex-measure` reads and holds where the measurements differ ([`crate::measure`]).
pub(crate) const CODEX: Family = Family {
    program: Program::Codex,
    verb: "codex-measure",
    source: "rollout",
    read: ROLLOUT_KEYS,
    holds: is_reduction,
    measures: is_alab,
    kind: record_kind,
    name_of: leg_name,
    key_holds,
    crates: &["aterm-agent", "aterm-phase", "aterm-lab"],
    filters: &FILTERS,
    standing: &STANDING,
};

/// A rollout row's record kind: `<type>/<payload.type>`.
fn record_kind(v: &Value) -> String {
    format!(
        "{}/{}",
        word(v.get("type")),
        word(v.get("payload").and_then(|p| p.get("type")))
    )
}

/// The reduction `leg` of `build` is committed as: `<stem>-<leg>.jsonl`.
fn leg_name(build: &str, leg: &str) -> String {
    format!("{}-{leg}.jsonl", rollout_reduction_stem(build))
}

/// The legs whose rollouts are measured, as their reductions are named (`<stem>-<leg>.jsonl`).
pub(crate) const LEGS: [&str; 6] = [
    "first", "continue", "approval", "question", "usage", "carry",
];

/// The libtest filters step 7 runs: the codex step fold's tests, the anchors' (the rollout keys
/// and the reductions' names), the reducer's fixed point over every committed `codex-*` file, and
/// this verb's own, which read the step testdata with the new reductions in place.
const FILTERS: [&str; 4] = [
    "harness::codex_steps::",
    "anchors::tests::",
    "reduce::tests::a_tool_argument_name_is_never_kept_as_written",
    "codex_measure::tests::",
];

/// The standing tests step 7 must see pass, by their libtest names: the fold whole and `On`,
/// equal to the rollout's own account, a moved duration (NC4), the key lower bound, the names,
/// the fixed point with no text kept, every codex-alab build in the step testdata read as
/// measured, its six legs whole, and every key the fold reads of a committed reduction one GONE
/// holds the next build to.
pub(crate) const STANDING: [&str; 8] = [
    "harness::codex_steps::tests::every_committed_codex_reduction_folds_whole",
    "harness::codex_steps::tests::the_codex_alab_legs_fold_exact",
    "harness::codex_steps::tests::a_moved_duration_fails_the_exact_golden",
    "anchors::tests::every_rollout_key_is_in_its_committed_rollout",
    "anchors::tests::a_rollout_reduction_is_named_by_its_build",
    "reduce::tests::a_tool_argument_name_is_never_kept_as_written",
    "codex_measure::tests::every_codex_alab_build_in_the_step_testdata_is_its_six_legs",
    "codex_measure::tests::gone_holds_a_build_to_every_rollout_key_the_fold_reads",
];

/// Whether `build` is a codex-alab build, as the reductions' names and aterm-agent's `build.rs`
/// read one.
#[must_use]
pub(crate) fn is_alab(build: &str) -> bool {
    rollout_reduction_stem(build).starts_with("codex-alab-")
}

/// Whether the file `name` is a Codex rollout reduction, as aterm-agent's `build.rs` reads one.
fn is_reduction(name: &str) -> bool {
    name.starts_with("codex-") && name.ends_with(".jsonl")
}

/// STEP 1 on `build` against the committed reductions: `Ok(None)` when they hold all six of its
/// legs ([`LEGS`]), each naming it (it is measured, and nothing is to be done), else the stem its
/// reductions take.
/// # Errors
/// STEM: `build` names no codex-alab stem; the reductions that name it are not all six legs; or
/// another build's reduction holds its stem (the stem drops the `+g<sha>`, so two builds of one
/// count collide).
pub(crate) fn admit(build: &str, committed: &[Reduction]) -> Result<Option<String>, Refusal> {
    let stem = rollout_reduction_stem(build);
    if !is_alab(build) {
        return Err(Refusal::new(
            Class::Stem,
            format!(
                "{build} is no codex-alab build (`<release>-dev.<count>+g<sha>`): its reductions would be {stem}-<leg>.jsonl"
            ),
        ));
    }
    let named: Vec<&str> = committed
        .iter()
        .filter(|r| r.build.as_deref() == Some(build))
        .map(|r| r.name.as_str())
        .collect();
    if !named.is_empty() {
        let missing: Vec<String> = LEGS
            .iter()
            .map(|leg| format!("{stem}-{leg}.jsonl"))
            .filter(|n| !named.contains(&n.as_str()))
            .collect();
        if missing.is_empty() {
            return Ok(None);
        }
        return Err(Refusal::new(
            Class::Stem,
            format!(
                "{build} is measured only in part: {} name it, and no {} does",
                named.join(", "),
                missing.join(", ")
            ),
        ));
    }
    let whole = format!("{stem}.jsonl");
    let leg = format!("{stem}-");
    let taken: Vec<String> = committed
        .iter()
        .filter(|r| r.name == whole || r.name.starts_with(&leg))
        .map(|r| format!("{} ({})", r.name, r.build.as_deref().unwrap_or("no build")))
        .collect();
    if taken.is_empty() {
        Ok(Some(stem))
    } else {
        Err(Refusal::new(
            Class::Stem,
            format!(
                "{build}'s reductions would be {stem}-<leg>.jsonl, which another build's hold: {}",
                taken.join(", ")
            ),
        ))
    }
}

/// The build `pkg`'s `codex` runs, as its rollouts name it ([`alab_build`]: the marker's
/// `client_version`, else the manifest's `version`), the key the tap's running-image check uses.
/// # Errors
/// KEY: the package names no build.
pub(crate) fn package_build(pkg: &Path) -> Result<String, Refusal> {
    let manifest = std::fs::read_to_string(pkg.join(PACKAGE_JSON)).unwrap_or_default();
    let exe = package_entrypoint(&manifest).map(|e| pkg.join(e));
    exe.as_deref().and_then(alab_build).ok_or_else(|| {
        Refusal::new(
            Class::Key,
            format!(
                "{} names no build ({ALAB_BUILD_JSON}'s client_version or its manifest's version)",
                pkg.display()
            ),
        )
    })
}

/// STEP 4: the one rollout a leg's codex wrote under its `CODEX_HOME` (`home`).
/// # Errors
/// ROLLOUTS: none, or more than one.
pub(crate) fn one_rollout(leg: &str, home: &Path) -> Result<PathBuf, Refusal> {
    let all = aterm_agent::harness::codex_rollout::rollouts_by_write(home);
    match all.as_slice() {
        [(path, _)] => Ok(path.clone()),
        _ => Err(Refusal::new(
            Class::Rollouts,
            format!(
                "{leg}: {} rollouts under {} where one is reduced ({})",
                all.len(),
                home.display(),
                all.iter()
                    .map(|(p, _)| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

/// STEP 4: every `session_meta` row of the rollout `text` names `build` (`cli_version`), and one
/// does: the tap's gate reads the rollout's own build too.
/// # Errors
/// KEY: a row names another build or none, or no row is a `session_meta`.
pub(crate) fn key_holds(leg: &str, text: &str, build: &str) -> Result<(), Refusal> {
    let mut metas = 0;
    for (n, row) in rows(text).enumerate() {
        let Ok(v) = aterm_json::from_str::<Value>(row) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        metas += 1;
        let said = v
            .get("payload")
            .and_then(|p| p.get("cli_version"))
            .and_then(Value::as_str);
        if said != Some(build) {
            return Err(Refusal::new(
                Class::Key,
                format!(
                    "{leg}: row {} names cli_version {} where the package's build is {build}",
                    n + 1,
                    said.unwrap_or("none")
                ),
            ));
        }
    }
    if metas == 0 {
        return Err(Refusal::new(
            Class::Key,
            format!("{leg}: no session_meta row names the build"),
        ));
    }
    Ok(())
}

/// A run's line in the note and the report.
fn run_line(n: u8, expect: &str, run: &Run) -> String {
    format!(
        "run {n} (expecting `{expect}`): {}, {} ok, {} FAIL; fences {}; socket to last check {}",
        if run.fails.is_empty() { "PASS" } else { "FAIL" },
        run.oks,
        run.fails.len(),
        if run.fences.is_empty() {
            "none".to_string()
        } else {
            run.fences.join(", ")
        },
        run.took
            .map_or_else(|| "-".to_string(), |d| format!("{} s", d.as_secs()))
    )
}

/// One reduced leg: its name, its reduction, the rollout it came from.
struct Leg {
    name: &'static str,
    text: String,
    from: PathBuf,
}

/// STEP 4 over run 1's world `world`: each of [`LEGS`]' one rollout, reduced ([`reduce_leg`]).
fn reduce_legs(world: &Path, build: &str) -> Result<Vec<Leg>, Refusal> {
    let mut out = Vec::new();
    for leg in LEGS {
        let home = world.join("legs").join(leg).join("codex-home");
        let from = one_rollout(leg, &home)?;
        let source = std::fs::read_to_string(&from).map_err(|e| {
            Refusal::new(Class::Rollouts, format!("{leg}: {}: {e}", from.display()))
        })?;
        let text = reduce_leg(&CODEX, leg, &source, build, &reduce_checked(Program::Codex))?;
        println!(
            "codex-measure: 4. {leg}: one rollout ({} bytes), cli_version {build}, a fixed point ({} bytes; pass 3 = pass 2)",
            source.len(),
            text.len()
        );
        out.push(Leg {
            name: leg,
            text,
            from,
        });
    }
    Ok(out)
}

/// The package record's line in the note.
fn package_line(pkg: &Path) -> String {
    let record = std::fs::read_to_string(pkg.join(ALAB_BUILD_JSON))
        .ok()
        .and_then(|t| aterm_json::from_str::<Value>(&t).ok())
        .unwrap_or_default();
    let s = |path: &[&str]| {
        let mut v = Some(&record);
        for k in path {
            v = v.and_then(|v| v.get(k));
        }
        v.and_then(|v| {
            v.as_str()
                .map(str::to_string)
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
        .unwrap_or_else(|| "?".to_string())
    };
    let short = |t: String| t.chars().take(12).collect::<String>();
    format!(
        "source `{}`, upstream `{}` plus {} commits (`{}`, {}), target `{}`, compiler `{}`, packager `{}`, built {}",
        short(s(&["source"])),
        s(&["upstream", "release"]),
        s(&["upstream", "commits_since_release"]),
        short(s(&["upstream", "commit"])),
        s(&["upstream", "commit_date"]),
        s(&["target"]),
        s(&["compiler", "rustc"]),
        short(s(&["packager"])),
        s(&["built_at"]),
    )
}

/// What the note says.
struct Note<'a> {
    build: &'a str,
    pkg: &'a Path,
    /// The package's path as the note says it (`~/…` below the home).
    pkg_said: &'a str,
    date: &'a str,
    head: (&'a str, &'a str),
    runs: [&'a Run; 2],
    legs: &'a [Leg],
    stem: &'a str,
    judged: &'a Judged,
    passed: &'a [String],
}

impl Note<'_> {
    fn text(&self) -> String {
        let release = self.build.split_once('+').map_or(self.build, |(r, _)| r);
        let (commit, tree) = self.head;
        let mut t = format!(
            "<!-- SPDX-License-Identifier: Apache-2.0 -->\n<!-- Copyright 2026 Andrew Yates -->\n\
             # codex-alab {release}: measured for the session log ({date})\n\n\
             `{LAB} codex-measure` measured `{build}` on this Mac: `lab codex-alab-proof` twice in \
             process, first expecting the build unmeasured (read partial), then measured, with its six legs' rollouts \
             reduced from run 1's world between the runs. This page is the verb's own record.\n\n\
             ## What ran\n\n\
             * **The package:** `{build}` at `{pkg_said}`: {package}.\n\
             * **The aterm:** debug builds (`targo --unverified build -p aterm`) of commit `{commit}` \
             (tree `{tree}`): run 1 as committed, run 2 with the six reductions below added.\n\n\
             ## The runs\n\n",
            date = self.date,
            build = self.build,
            pkg_said = self.pkg_said,
            package = package_line(self.pkg),
        );
        for (n, (run, expect)) in self.runs.iter().zip(["partial", "bound"]).enumerate() {
            t.push_str(&format!("* {}\n", run_line(n as u8 + 1, expect, run)));
        }
        t.push_str(&format!(
            "\n## The build measured\n\n\
             Each leg's one rollout under its scratch `CODEX_HOME` names `{build}` in every \
             `session_meta`. It was reduced (`lab reduce --program codex`, its check of what it writes \
             included), then reduced again; a third pass gave the second's bytes, so the second is a \
             fixed point. They are committed as `{STEPS}/{stem}-<leg>.jsonl`:\n\n\
             | leg | bytes | rows | sha256 |\n|---|---|---|---|\n",
            build = self.build,
            stem = self.stem,
        ));
        for leg in self.legs {
            t.push_str(&format!(
                "| {} | {} | {} | `{}` |\n",
                leg.name,
                leg.text.len(),
                rows(&leg.text).count(),
                sha256(&leg.text)
            ));
        }
        let j = self.judged;
        t.push_str(&format!(
            "\n**Rollout keys.** The six legs show {} keys and {} record kinds, none that the {} \
             measured builds' reductions do not show, and no rollout key the fold reads that {}'s \
             same legs showed is gone.",
            j.keys,
            j.kinds,
            j.builds,
            j.prev.as_deref().unwrap_or("a previous build")
        ));
        if j.noted.is_empty() {
            t.push_str(" No other key, record kind or step shape moved.\n");
        } else {
            t.push_str(" What moved and does not refuse:\n\n");
            for n in &j.noted {
                t.push_str(&format!("* {n}\n"));
            }
        }
        t.push_str(&format!(
            "\n**The standing tests passed** with the reductions in place (`{}`): {}.\n",
            standing_line(&CODEX),
            self.passed
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        t
    }
}

/// `codex-measure [--package DIR] [--keep]`.
/// # Errors
/// Usage; NOT RUN (no package, a dirty checkout, the lock held, a build that failed); a refusal by
/// class; an interrupt.
pub fn cmd_codex_measure(args: &[String]) -> Res {
    crate::stop::install();
    crate::stop::at_hard_exit(undo_on_hard_exit);
    let mut a = args.to_vec();
    let keep = take_flag(&mut a, "--keep");
    let given = take_value(&mut a, "--package")?;
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "codex-measure: unexpected '{extra}' ({LAB} help)"
        )));
    }
    let p = paths();
    let repo = p.repo.clone();
    let steps = repo.join(STEPS);
    let home = p.need_store_home().map_err(not_run)?;
    let pkg = codex_alab_proof::package(given.as_deref(), home).map_err(not_run)?;
    let pkg_said = pkg.strip_prefix(home).map_or_else(
        |_| pkg.display().to_string(),
        |below| format!("~/{}", below.display()),
    );

    let build = package_build(&pkg)?;
    let committed = committed_in(&CODEX, &repo)?;
    let Some(stem) = admit(&build, &committed)? else {
        println!(
            "codex-measure: 1. {build} ({}) is measured already: HEAD holds its six legs' reductions, so CODEX_MEASURED_BUILDS names it; nothing written",
            pkg.display()
        );
        return Ok(0);
    };
    println!(
        "codex-measure: 1. {build} ({}) is not measured; its reductions will be {stem}-<leg>.jsonl",
        pkg.display()
    );
    let (_held, head, tree) = hold_clean(&CODEX, &repo)?;
    let (head, tree) = (head.as_str(), tree.as_str());

    println!("codex-measure: 2. building aterm at {head}");
    let aterm = build_aterm(&CODEX, &repo)?;
    println!("codex-measure: 3. run 1, expecting {build} unmeasured (read partial)");
    let mut run1 = codex_alab_proof::run(aterm, &pkg, Some(false))?;
    println!("codex-measure: 3. {}", run_line(1, "partial", &run1));
    gate(Class::ProofRed, &run1)?;
    let world1 = run1.world.dir.clone();
    let kept = |w: &Path| format!("; run 1's world is kept at {}", w.display());
    let legs = reduce_legs(&world1, &build).map_err(|r| {
        let mut f = Fail::from(r);
        f.msg = f.msg.map(|m| m + &kept(&world1));
        f
    })?;
    let texts: Vec<(&str, String)> = legs.iter().map(|l| (l.name, l.text.clone())).collect();
    let judged = judge(&CODEX, &build, &texts, &committed).map_err(|r| {
        let mut f = Fail::from(r);
        f.msg = f.msg.map(|m| m + &kept(&world1));
        f
    })?;
    println!(
        "codex-measure: 5. {} keys and {} record kinds, nothing new against {} measured builds' reductions, no rollout key gone against {}{}",
        judged.keys,
        judged.kinds,
        judged.builds,
        judged.prev.as_deref().unwrap_or("no previous build"),
        if judged.noted.is_empty() {
            "; nothing else moved".to_string()
        } else {
            format!("; noted: {}", judged.noted.join("; "))
        }
    );
    for leg in &legs {
        println!("    {} from {}", leg.name, leg.from.display());
    }

    let fixtures: Vec<(PathBuf, String)> = legs
        .iter()
        .map(|l| {
            (
                steps.join(format!("{stem}-{}.jsonl", l.name)),
                l.text.clone(),
            )
        })
        .collect();
    let date = today();
    let count = order_of(&build).1;
    let note_path = repo
        .join(MEASURED)
        .join(format!("codex-alab-proof-{count}-{date}.md"));
    let (written, (passed, mut run2)) = landed(&CODEX, &fixtures, |w| {
        let passed = standing(&CODEX, &repo).map_err(|f| with_world(f, &world1, None))?;
        println!("codex-measure: 8. rebuilding aterm with the reductions");
        let aterm = build_aterm(&CODEX, &repo).map_err(|f| with_world(f, &world1, None))?;
        println!("codex-measure: 8. run 2, expecting {build} bound");
        let run2 = codex_alab_proof::run(aterm, &pkg, Some(true))
            .map_err(|f| with_world(f, &world1, None))?;
        println!("codex-measure: 8. {}", run_line(2, "bound", &run2));
        gate(Class::NotBound, &run2).map_err(|f| with_world(f, &world1, None))?;
        let note = Note {
            build: &build,
            pkg: &pkg,
            pkg_said: &pkg_said,
            date: &date,
            head: (head, tree),
            runs: [&run1, &run2],
            legs: &legs,
            stem: &stem,
            judged: &judged,
            passed: &passed,
        };
        w.create(&note_path, note.text().as_bytes())
            .map_err(|e| with_world(not_run(e), &world1, Some(&run2.world.dir)))?;
        println!("codex-measure: 9. wrote {}", note_path.display());
        Ok((passed, run2))
    })?;
    if keep {
        println!(
            "codex-measure: 10. the worlds are kept (--keep): {} and {}",
            world1.display(),
            run2.world.dir.display()
        );
    } else {
        println!(
            "codex-measure: 10. removing the worlds {} and {}",
            world1.display(),
            run2.world.dir.display()
        );
        run1.world.release();
        run2.world.release();
    }
    println!(
        "MEASURED {build}: {} tests passed; wrote {}",
        passed.len(),
        written
            .iter()
            .map(|p| p.strip_prefix(&repo).unwrap_or(p).display().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "left to do by hand: a CHANGELOG line under [Unreleased], the commit, the merge and the push"
    );
    Ok(0)
}

#[cfg(test)]
#[path = "codex_measure_tests.rs"]
mod tests;
