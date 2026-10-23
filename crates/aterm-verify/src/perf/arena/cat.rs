// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `cat` lane (ARENA-CAT, the D1 run): `cat` 150 MB of ascii and 150 MB
//! of mixed-language unicode through each terminal, aterm against the pinned
//! ghostty tip, interleaved A/B with the order rotated per trial.
//!
//! The PTY-drain method: the terminal runs the in-terminal half
//! ([`super::inner`]), which stamps a start time once it is up, `cat`s the
//! pre-generated corpus and reports the elapsed time. The PTY backpressures
//! `cat` at the terminal's consume rate, so that time is the ingest time with
//! startup excluded.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_uds::spawnfd::{Stdio, command};

use super::{
    Aterm, COOLDOWN, Ghostty, Group, Halt, Inbox, Ledger, arena_dir, build_aterm, ghostty_tip,
    live, macos_version, pins, preflight, without_aterm_env,
};
use crate::perf::stats::median;
use crate::perf::trend::utc_date;
use crate::perf::{LaneRun, arena::Args};

/// The two ARENA-CAT corpus shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Corpus {
    Ascii,
    /// Mixed-language plaintext: multi-byte dense, wide CJK, no SGR — the
    /// UTF-8 decode and width path of a `cat book.txt`.
    Unicode,
}

impl Corpus {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Corpus::Ascii => "ascii",
            Corpus::Unicode => "unicode",
        }
    }

    /// The repeated unit; a corpus is whole units up to the target size.
    const fn unit(self) -> &'static str {
        match self {
            Corpus::Ascii => "the quick brown fox jumps over the lazy dog 0123456789\r\n",
            Corpus::Unicode => {
                "速い茶色の狐がのろまな犬を飛び越える 0123456789\r\n\
                 빠른 갈색 여우가 게으른 개를 뛰어넘는다\r\n\
                 Быстрая коричневая лиса перепрыгивает через ленивую собаку\r\n\
                 Η γρήγορη καφέ αλεπού πηδά πάνω από το τεμπέλικο σκυλί\r\n\
                 the quick brown fox 快速的棕色狐狸跳过懒狗 jumps over\r\n"
            }
        }
    }
}

/// Write `mb` MiB of `corpus` (whole units; the last may cross the target).
pub(crate) fn generate(
    corpus: Corpus,
    mb: u64,
    out: &mut impl std::io::Write,
) -> std::io::Result<u64> {
    let unit = corpus.unit().as_bytes();
    let target = mb * 1024 * 1024;
    let mut written = 0u64;
    let mut block = Vec::with_capacity(1 << 20);
    while (block.len() as u64) < (1 << 20) {
        block.extend_from_slice(unit);
    }
    let units_per_block = block.len() / unit.len();
    while written < target {
        let remaining_units = (target - written).div_ceil(unit.len() as u64) as usize;
        let n = remaining_units.min(units_per_block);
        out.write_all(&block[..n * unit.len()])?;
        written += (n * unit.len()) as u64;
    }
    Ok(written)
}

/// The pre-generated corpus in the arena cache, written once (rule zero §5:
/// never piped from a generator mid-run).
pub(crate) fn corpus_file(corpus: Corpus, mb: u64) -> Result<PathBuf, String> {
    let path = arena_dir()?.join(format!("d1_{}_{mb}.vt", corpus.name()));
    if !path.is_file() {
        eprintln!("generating the {mb} MB {} corpus", corpus.name());
        let part = path.with_extension("part");
        let mut f = std::io::BufWriter::new(
            std::fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?,
        );
        generate(corpus, mb, &mut f).map_err(|e| format!("{}: {e}", part.display()))?;
        f.flush().map_err(|e| format!("{}: {e}", part.display()))?;
        std::fs::rename(&part, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(path)
}

/// The two terminals of a head-to-head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Term {
    Aterm,
    Ghostty,
}

impl Term {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Term::Aterm => "aterm",
            Term::Ghostty => "ghostty",
        }
    }
}

/// Odd trials aterm first, even trials ghostty first (rule zero §2: never
/// all-of-A then all-of-B).
pub(crate) const fn order(trial: u32) -> [Term; 2] {
    if trial % 2 == 1 {
        [Term::Aterm, Term::Ghostty]
    } else {
        [Term::Ghostty, Term::Aterm]
    }
}

/// Launch `term` running `inner` (the in-terminal half's argv), wait for its
/// report within `ceiling`, then stop the terminal by its own handle.
pub(crate) fn run_in(
    term: Term,
    aterm: &Aterm,
    ghostty: &Ghostty,
    grid_args: (&[String], &[String]),
    inner: &[String],
    inbox: &Inbox,
    ceiling: Duration,
) -> Result<String, String> {
    let mut group = None;
    match term {
        Term::Aterm => {
            let mut cmd = command(&aterm.bin);
            without_aterm_env(&mut cmd)
                .arg("--window")
                .args(grid_args.0)
                .arg("-e")
                .args(inner)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            group = Some(Group::spawn(&mut cmd)?);
        }
        Term::Ghostty => {
            live::opening(&ghostty.app);
            // Arguments, not environment: variables do not survive `open -na`,
            // and ghostty refuses a direct CLI launch.
            let status = command("open")
                .arg("-na")
                .arg(&ghostty.app)
                .arg("--args")
                .args(grid_args.1)
                .arg("-e")
                .args(inner)
                .status()
                .map_err(|e| format!("could not run open: {e}"))?;
            if !status.success() {
                return Err(format!(
                    "open -na {} exited {status}",
                    ghostty.app.display()
                ));
            }
        }
    }
    let got = inbox.wait(ceiling);
    if let Some(g) = group {
        g.retire()?;
    }
    live::stop_app(&ghostty.app);
    got
}

/// The in-terminal half's argv for `verb`.
pub(crate) fn inner_argv(verb: &str, args: &[&Path]) -> Result<Vec<String>, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let mut v = vec![
        exe.display().to_string(),
        crate::perf::ARENA_INNER_FLAG.into(),
        verb.into(),
    ];
    v.extend(args.iter().map(|p| p.display().to_string()));
    Ok(v)
}

/// A drain report's seconds.
pub(crate) fn parse_drain(report: &str) -> Result<f64, String> {
    match report.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["drain", secs] => secs
            .parse::<f64>()
            .ok()
            .filter(|s| s.is_finite() && *s > 0.0)
            .ok_or_else(|| format!("unreadable drain report: {report:?}")),
        _ => Err(format!("the in-terminal half reported: {report:?}")),
    }
}

/// A hung drain is called after this long.
const DRAIN_CEILING: Duration = Duration::from_secs(450);
const CORPUS_MB: u64 = 150;

/// The `cat` lane.
pub(crate) fn run(args: &Args) -> LaneRun {
    super::outcome(run_inner(args))
}

fn run_inner(args: &Args) -> Result<String, Halt> {
    let trials = args.trials.unwrap_or(5);
    let (machine, ledgers) = preflight(args.dev_smoke)?;
    let ghostty = ghostty_tip(&pins()?, args.dmg.as_deref())?;
    let aterm = build_aterm()?;
    let mut rows = Vec::new();
    for corpus in [Corpus::Ascii, Corpus::Unicode] {
        let file = corpus_file(corpus, CORPUS_MB)?;
        let mut times: [Vec<f64>; 2] = [Vec::new(), Vec::new()];
        for i in 1..=trials {
            for term in order(i) {
                let inbox = Inbox::bind()?;
                let inner = inner_argv("drain", &[&file, inbox.path()])?;
                let result = run_in(
                    term,
                    &aterm,
                    &ghostty,
                    (&[], &[]),
                    &inner,
                    &inbox,
                    DRAIN_CEILING,
                )
                .and_then(|r| parse_drain(&r));
                match result {
                    Ok(secs) => {
                        eprintln!("  {} trial {i} {}: {secs:.3}s", corpus.name(), term.name());
                        times[term as usize].push(secs);
                    }
                    Err(e) => eprintln!(
                        "  {} trial {i} {}: FAILED — {e}",
                        corpus.name(),
                        term.name()
                    ),
                }
                // The cooldown between competitors is the protocol (rule zero §2).
                std::thread::sleep(COOLDOWN);
            }
        }
        let [a, g] = &times;
        if a.is_empty() || g.is_empty() {
            return Err(Halt::Fail(format!(
                "{}: a terminal finished no trial",
                corpus.name()
            )));
        }
        let (am, gm) = (median(a), median(g));
        let mbps = |s: f64| CORPUS_MB as f64 / s;
        eprintln!(
            "== {} {CORPUS_MB}MB: aterm median {am:.3}s ({:.1} MB/s) vs ghostty-tip {gm:.3}s ({:.1} MB/s) ==",
            corpus.name(),
            mbps(am),
            mbps(gm)
        );
        let raw = |v: &[f64]| {
            v.iter()
                .map(|s| format!("{s:.3}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        rows.push(format!(
            "| {} {CORPUS_MB}MB cat | aterm {am:.3}s ({:.1} MB/s) | ghostty-tip {gm:.3}s ({:.1} MB/s) | raw aterm: {} / ghostty: {} |",
            corpus.name(),
            mbps(am),
            mbps(gm),
            raw(a),
            raw(g)
        ));
    }
    let date = utc_date();
    let entry = ledgers.write(
        "d1-cat",
        &Ledger {
            date: &date,
            dim: "D1 ARENA-CAT (cat 150MB ascii + unicode, PTY-drain)",
            machine: &machine.label(),
            macos: &macos_version(),
            aterm: &aterm
                .provenance("targo --unverified build --release -p aterm, shipping default"),
            competitors: &format!(
                "ghostty tip {} (official dmg, sha256={})",
                ghostty.commit, ghostty.sha256
            ),
        },
        &format!("{}\n", rows.join("\n")),
    )?;
    Ok(format!("ledger entry {}", entry.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_corpus_is_whole_units_reaching_the_target() {
        for corpus in [Corpus::Ascii, Corpus::Unicode] {
            let mut out = Vec::new();
            let n = generate(corpus, 1, &mut out).expect("generates");
            let unit = corpus.unit().len() as u64;
            assert_eq!(n, out.len() as u64);
            assert!(
                n >= 1 << 20 && n < (1 << 20) + unit,
                "{}: {n}",
                corpus.name()
            );
            assert_eq!(n % unit, 0, "{}", corpus.name());
            assert!(std::str::from_utf8(&out).is_ok());
        }
    }

    #[test]
    fn the_order_rotates_every_trial() {
        assert_eq!(order(1), [Term::Aterm, Term::Ghostty]);
        assert_eq!(order(2), [Term::Ghostty, Term::Aterm]);
    }

    #[test]
    fn a_drain_report_is_positive_seconds() {
        assert_eq!(parse_drain("drain 1.25"), Ok(1.25));
        assert!(parse_drain("drain 0").is_err());
        assert!(parse_drain("fail cat exited 1").is_err());
    }
}
