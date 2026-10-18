// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The candidate gate (D27): a trust seal is published only when the gate compared it
//! with the published seal and answered PUBLISH-OK, recently, on this aterm's history,
//! under the committed retraction ledger.
//!
//! The gate's RUN is aterm-release's (`ship trust candidate --candidate`, hours of two
//! whole-tree lane runs): it files a [`Receipt`] under the receipts store. Whether a
//! receipt admits a publish is decided HERE, in-process and pure ([`refusals`]), and
//! both askers read this one rule: the `rustc` group's publish through
//! [`CandidateGate`] ([`ShipTrustCandidate`]), and `ship trust candidate --admits`
//! from a shell. Answering costs a directory read, a `tree_root` and two git queries —
//! no cutter build, no child to bound.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::Stdio;

use crate::ctx::Ctx;

/// What the gate answered for one seal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateAnswer {
    /// PUBLISH-OK, by the newest run for this seal: the receipt it read.
    Admitted(String),
    /// Refused, with every reason.
    Refused(Vec<String>),
    /// The gate could not be asked, or could not check.
    CannotCheck(String),
}

/// Asks the candidate gate about a seal.
pub trait CandidateGate {
    /// Whether the newest run of the gate for `seal` admits publishing it over the trust
    /// build the public channel pins (`None` when the channel could not be read).
    fn admits(&self, ctx: &Ctx, seal: &Path, public_pin: Option<u64>) -> GateAnswer;
}

/// The gate's answer, read in-process: the newest receipt for the seal's `tree_root`
/// under `<state>/candidate-gate`, held to [`refusals`] against the aterm checkout
/// the producer was built from.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShipTrustCandidate;

/// The program the same question is asked through from a shell ([`command`]).
pub const PROGRAM: &str = "targo";

/// `ship trust candidate --admits`'s argv after [`PROGRAM`]: the same question, asked
/// from a shell.
#[must_use]
pub fn command(seal: &Path, public_pin: Option<u64>) -> Vec<OsString> {
    let mut v: Vec<OsString> = ["--unverified", "ship", "trust", "candidate", "--admits"]
        .iter()
        .map(OsString::from)
        .collect();
    v.push(seal.as_os_str().to_owned());
    if let Some(pin) = public_pin {
        v.push("--base-build".into());
        v.push(pin.to_string().into());
    }
    v
}

/// A receipt's first line. Format 2 binds each excuse to the site it was audited at;
/// an older receipt admits nothing.
pub const RECEIPT_MAGIC: &str = "aterm-trust candidate-gate 2";

/// How old a PASS may be and still admit a publish: a receipt measures one aterm tree
/// on one day.
pub const MAX_AGE_DAYS: i64 = 7;

/// The retraction ledger, relative to the aterm checkout: the only source of an
/// excuse a receipt may carry.
pub const RETRACTION_LEDGER: &str = "crates/aterm-release/trust/retractions.tsv";

/// The receipts store under a lanes' state root (`Ctx::state_dir`).
#[must_use]
pub fn receipts_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("candidate-gate")
}

/// The lanes' state root this process's environment names, found as
/// [`Ctx::from_env`] finds it — a non-empty `HOME` and `XDG_STATE_HOME`, rooted by
/// [`Ctx::new`] — so the gate's run (aterm-release, which files the receipts) and this
/// reader find one store. `None` when `HOME` is unset or empty.
#[must_use]
pub fn state_dir_from_env() -> Option<PathBuf> {
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let home = env("HOME")?;
    Some(Ctx::new(&home, None, env("XDG_STATE_HOME").as_deref(), 0).state_dir)
}

/// A receipt's `key value` lines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Receipt {
    /// Its first line.
    pub magic: String,
    /// Each key's first value.
    pub fields: BTreeMap<String, String>,
}

impl Receipt {
    /// Read a receipt: the first line, then each `key value` line (the first of a key
    /// wins).
    #[must_use]
    pub fn parse(text: &str) -> Receipt {
        let mut lines = text.lines();
        let magic = lines.next().unwrap_or("").to_string();
        let mut fields = BTreeMap::new();
        for l in lines {
            let (k, v) = l.split_once(' ').unwrap_or((l, ""));
            fields.entry(k.to_string()).or_insert_with(|| v.to_string());
        }
        Receipt { magic, fields }
    }

    /// A field's value, empty when absent.
    #[must_use]
    pub fn get(&self, k: &str) -> &str {
        self.fields.get(k).map_or("", String::as_str)
    }

    /// The receipt as filed: each value on one line.
    #[must_use]
    pub fn render(&self) -> String {
        let mut t = format!("{}\n", self.magic);
        for (k, v) in &self.fields {
            t.push_str(k);
            t.push(' ');
            t.push_str(&v.replace(['\t', '\r', '\n'], " "));
            t.push('\n');
        }
        t
    }
}

/// The newest receipt under `dir/*/RECEIPT` (by its `date`) of a run whose candidate is
/// `root`: every run counts whatever it answered, so a newer refusal is never outlived by an
/// older PASS.
#[must_use]
pub fn newest_receipt(dir: &Path, root: &str) -> Option<(PathBuf, Receipt)> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let p = e.path().join("RECEIPT");
            let r = Receipt::parse(&fs::read_to_string(&p).ok()?);
            (r.get("candidate-tree-root") == root).then_some((p, r))
        })
        .max_by(|a, b| a.1.get("date").cmp(b.1.get("date")))
}

/// The aterm checkout a receipt is held to.
pub trait Aterm {
    /// Whether `commit` is an ancestor of (or is) HEAD.
    fn contains(&self, commit: &str) -> bool;
    /// The sha256 of the retraction ledger as committed at `rev`, `None` when git
    /// cannot show it.
    fn ledger_sha(&self, rev: &str) -> Option<String>;
}

/// An aterm checkout on disk, asked through git.
#[derive(Clone, Debug)]
pub struct Checkout<'a>(pub &'a Path);

impl Aterm for Checkout<'_> {
    fn contains(&self, commit: &str) -> bool {
        crate::git::run(self.0, &["merge-base", "--is-ancestor", commit, "HEAD"]).is_ok()
    }

    fn ledger_sha(&self, rev: &str) -> Option<String> {
        // The blob's exact bytes (the runner trims what it reads), hashed as the gate
        // hashed the file it read.
        let shown = aterm_uds::spawnfd::command("git")
            .arg("-C")
            .arg(self.0)
            .args(["cat-file", "blob", &format!("{rev}:{RETRACTION_LEDGER}")])
            .env("GIT_TERMINAL_PROMPT", "0")
            .stderr(Stdio::null())
            .output()
            .ok()?;
        shown
            .status
            .success()
            .then(|| atpkg::sha256_bytes(&shown.stdout))
    }
}

/// Whether `s` is a tree_root: 64 lowercase hex digits.
#[must_use]
pub fn is_tree_root(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The publisher's question: every reason `receipt`, the newest run of the gate for the
/// toolchain whose tree_root is `root`, admits no publish, against the trust build `pin` the
/// public channel pins now, at `now` (epoch seconds), in `aterm`. Empty: admitted.
#[must_use]
pub fn refusals(
    receipt: Option<&Receipt>,
    root: &str,
    pin: u64,
    now: i64,
    aterm: &dyn Aterm,
) -> Vec<String> {
    let Some(r) = receipt else {
        return vec![format!(
            "no candidate-gate receipt names this toolchain (tree_root {}): run `targo \
             --unverified ship trust candidate --candidate <it>` first",
            root.get(..12).unwrap_or(root)
        )];
    };
    if r.magic != RECEIPT_MAGIC {
        return vec![format!(
            "the receipt's first line is `{}`, not `{RECEIPT_MAGIC}`: it admits nothing — run \
             the gate again",
            r.magic
        )];
    }
    let mut out = Vec::new();
    if r.get("verdict") != "PASS" || r.get("exit") != "0" {
        out.push(format!(
            "the newest run of the gate for it did not pass (verdict {}, exit {}); its \
             comparison is {}",
            r.get("verdict"),
            r.get("exit"),
            r.get("compare")
        ));
    }
    if r.get("mode") != "run" {
        out.push(format!(
            "it records a {} of the gate, not a run of it",
            r.get("mode")
        ));
    }
    if r.get("candidate-tree-root") != root || r.get("candidate-prover") != root {
        out.push(format!(
            "it is not a run that measured this toolchain (its run's prover {})",
            r.get("candidate-prover")
        ));
    }
    if !r.get("base-from").starts_with("published") {
        out.push(format!(
            "it compared against a base named by hand ({}), not the published seal",
            r.get("base-from")
        ));
    } else if !is_tree_root(r.get("base-signed-tree-root"))
        || r.get("base-prover") != r.get("base-signed-tree-root")
    {
        out.push(format!(
            "its base run measured prover {}, not the published seal as the package client \
             verified it ({})",
            r.get("base-prover"),
            r.get("base-signed-tree-root")
        ));
    }
    if r.get("base-build") != pin.to_string() {
        out.push(format!(
            "it compared against trust build {}, and the public channel now pins {pin}: run \
             the gate again, against the published seal",
            r.get("base-build")
        ));
    }
    match aterm_types::rfc3339::parse_utc(r.get("date")) {
        None => out.push(format!(
            "its date `{}` does not read, so nothing says the measurement is current",
            r.get("date")
        )),
        Some(when) if (now - when) / 86_400 > MAX_AGE_DAYS => out.push(format!(
            "it is {} days old (the gate ran {}), past {MAX_AGE_DAYS}: run the gate again on a \
             current aterm",
            (now - when) / 86_400,
            r.get("date")
        )),
        Some(_) => {}
    }
    let commit = r.get("aterm-commit");
    let hex40 = commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit());
    if !hex40 || !aterm.contains(commit) {
        out.push(format!(
            "it measured aterm {commit}, which this checkout's HEAD does not contain: run the \
             gate on this aterm's history"
        ));
    }
    // An excuse comes only from the audited ledger: the one committed at the commit it
    // measured, still the one at HEAD.
    let read = r.get("retractions").split(' ').next().unwrap_or("");
    match hex40.then(|| aterm.ledger_sha(commit)).flatten() {
        None => out.push(format!(
            "it read the retraction ledger {read}, and this checkout cannot show \
             {RETRACTION_LEDGER} as committed at aterm {commit} to hold it to"
        )),
        Some(want) if want != read => out.push(format!(
            "it read the retraction ledger {read}, not the one committed at aterm {commit} \
             ({want}): a ledger named by hand admits nothing"
        )),
        Some(want) if aterm.ledger_sha("HEAD").as_deref() != Some(want.as_str()) => out.push(
            "the retraction ledger changed since the run, so its excuses no longer stand: run \
             the gate again under HEAD's ledger"
                .to_string(),
        ),
        Some(_) => {}
    }
    out
}

/// The physical toolchain directory: `dir`, or the parent of a `bin/` given.
///
/// # Errors
/// `dir` does not resolve.
pub fn toolchain_dir(dir: &Path) -> Result<PathBuf, String> {
    let d = fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(
        if d.join("targo").is_file() && !d.join("bin/targo").is_file() {
            d.parent().map_or(d.clone(), Path::to_path_buf)
        } else {
            d
        },
    )
}

/// atpkg's `tree_root` of a toolchain directory: the prover identity a receipt names.
///
/// # Errors
/// The tree cannot be read.
pub fn tree_root(dir: &Path) -> Result<String, String> {
    atpkg::tree::tree_root(dir).map_err(|e| format!("the tree_root of {}: {e}", dir.display()))
}

/// The gate's answer for the toolchain whose tree_root is `root`, from the receipts
/// under `receipts`: admitted (naming the receipt read), or every refusal.
#[must_use]
pub fn answer(receipts: &Path, root: &str, pin: u64, now: i64, aterm: &dyn Aterm) -> GateAnswer {
    let found = newest_receipt(receipts, root);
    let refused = refusals(found.as_ref().map(|(_, r)| r), root, pin, now, aterm);
    match found {
        Some((path, _)) if refused.is_empty() => GateAnswer::Admitted(format!(
            "PASS: {} (against the published trust {pin})",
            path.display()
        )),
        _ => GateAnswer::Refused(refused),
    }
}

impl CandidateGate for ShipTrustCandidate {
    fn admits(&self, ctx: &Ctx, seal: &Path, public_pin: Option<u64>) -> GateAnswer {
        let Some(pin) = public_pin else {
            return GateAnswer::CannotCheck(
                "the public channel's trust pin could not be read, so nothing names the \
                 published seal a receipt must have been measured against"
                    .into(),
            );
        };
        let root = match toolchain_dir(seal).and_then(|d| tree_root(&d)) {
            Ok(root) => root,
            Err(why) => {
                return GateAnswer::CannotCheck(format!(
                    "the candidate gate cannot identify this seal: {why}"
                ));
            }
        };
        answer(
            &receipts_dir(&ctx.state_dir),
            &root,
            pin,
            ctx.now_unix,
            &Checkout(&ctx.repo_root),
        )
    }
}
