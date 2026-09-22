// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PACKAGING CONTRACT AND THE ALIGNMENT VERDICT (design
//! `docs/DESIGN-aterm-wrapper-2026-09-17.md` §1.2, §1.3, §3.1-§3.8): what a
//! harness tree declares about itself, what the probes measure against the
//! installed program, and the per-capability verdict that comes out.
//!
//! # Three things live here, in this order
//!
//! 1. [`Contract`] — `harness.toml` read and ADMITTED. Admission is
//!    fail-closed and WHOLE-FILE: one unknown key, one value outside a closed
//!    vocabulary, one argument the shim may not carry, and nothing is
//!    admitted — the shape of [`atpkg::shim_env::ShimEnv::admit`], whose
//!    refusal is a `Reject::ShimEnv` that refuses the manifest entire. A
//!    half-admitted contract is a harness that runs with a capability its
//!    author did not declare, which is the one outcome worse than not running.
//! 2. [`ProbeResult`] and [`run_probes`] — §3.2's adaptation contract. A probe
//!    prints ONE JSON object and exits 0 always, so the verdict rides in the
//!    JSON. Everything else — a crash, a non-zero exit, silence, a timeout,
//!    unparseable output, an id that disagrees with the file it came from —
//!    reads [`ProbeStatus::Error`]. **Never `ok`, and never `absent`**:
//!    `absent` is a MEASUREMENT ("the flag is not in `--help`"), and a probe
//!    that fell over measured nothing.
//! 3. [`evaluate`] and [`Verdict`] — §3.5's intersection rule and §3.6's
//!    per-capability degradation. A failing probe takes down exactly the
//!    capabilities whose `needs` name it, and says which probe did it. The
//!    signed verdict and the local one are combined by [`Verdict::narrow`],
//!    which can only ever narrow.
//!
//! # The central law (design §0.2), as it lands here
//!
//! aterm's own view is the SPINE; vendor hooks are ENRICHMENT. So a harness
//! with ZERO probes run is not broken — it is [`Verdict::Pending`], every
//! capability off, and [`Alignment::wire`] says so in one word. A tree that
//! cannot be found at all is the same answer. Nothing in this module can make
//! the wrapped program un-launchable, because nothing in this module launches
//! it: `__harness` (design §1.4) execs the plain target on any failure, and
//! this module only ever hands it a word.
//!
//! # Where the spellings live
//!
//! The per-program ROW of design §3.8 (`harness for claude 2026091701:
//! aligned (7/7)`) is spelled in `atpkg::harness` and NOWHERE else — that
//! module is the single home of the harness row vocabulary. This module owns
//! the WIRE word that keys it ([`Alignment::wire`], [`Verdict::parse_wire`]):
//! `aligned(7/7)`, `degraded(5/7):usage-hud,model-failover`,
//! `unsupported:<reason>`, `pending`. `atpkg::harness::row` parses that word
//! and renders the row. Two vocabularies, one home each, and the seam between
//! them is a string both sides have a test for.
//!
//! # Nothing polls
//!
//! [`run_probes`] spawns a child, reads its stdout on one thread and blocks on
//! `recv_timeout` on this one. That is a deadline, not a cadence: there is no
//! sleep, no tick and no retry loop in this file. Everything else here is a
//! pure function of its arguments, with the clock and every path injected.
//!
//! STATUS (docs/README.md honesty ratchet): unit-tested, including a refusal
//! for each malformed contract shape, a real probe run over `/bin/sh` scripts
//! in a temp directory (one that prints `ok`, one that prints `absent`, one
//! that exits non-zero, one that prints nothing and one that hangs past the
//! budget), the single-capability degradation, and the intersection rule's
//! narrow-only property. What is deliberately NOT here, because it needs the
//! owner's publishing ceremony (stage note): the atpkg index row, the signing
//! lane and the `__harness` shim prelude. Everything here works from a
//! dev-linked tree named by `$ATERM_HARNESS_ROOT` or `--tree`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use aterm_json::{Map, Value};

use super::truncate_bytes;

// ---------------------------------------------------------------------------
// The ONE ABI rule (design §3.1)
// ---------------------------------------------------------------------------

/// The newest harness ABI this aterm speaks.
pub const HARNESS_ABI: u32 = 1;

/// The oldest harness ABI this aterm still speaks. It rises only when a future
/// aterm DROPS an old grammar; design §3.1 starts it at 1.
pub const HARNESS_ABI_MIN: u32 = 1;

/// The one ABI rule: `HARNESS_ABI_MIN <= abi <= HARNESS_ABI`.
///
/// A miss in EITHER direction is `unsupported`, never `blocked by aterm`:
/// aterm is not an index program, so no dependency row can spell it (§3.1).
#[must_use]
pub fn abi_supported(abi: u32) -> bool {
    (HARNESS_ABI_MIN..=HARNESS_ABI).contains(&abi)
}

// ---------------------------------------------------------------------------
// Bounds. Every one of these is a refusal, not a truncation.
// ---------------------------------------------------------------------------

/// The most bytes a `harness.toml` may be. A contract is a declaration, not a
/// payload; a megabyte of it is a mistake or an attack.
pub const MAX_CONTRACT_BYTES: usize = 64 * 1024;
/// The most lines the reader will walk.
pub const MAX_CONTRACT_LINES: usize = 1024;
/// The most blocks (`[table]` / `[[table]]` headers, plus the root) one file
/// may carry.
pub const MAX_BLOCKS: usize = 96;
/// The most keys one block may carry.
pub const MAX_KEYS: usize = 64;
/// The most elements one inline array may carry.
pub const MAX_ARRAY: usize = 64;
/// The most `wraps_any` names — a preference-ordered family, not a catalogue.
pub const MAX_WRAPS: usize = 8;
/// The most declared egress hostnames (design §1.2: "≤16").
pub const MAX_EGRESS: usize = 16;
/// The most `[[tool]]` rows (design §1.2: "≤16 rows").
pub const MAX_TOOLS: usize = 16;
/// The most `[shim] env` entries — [`atpkg::shim_env::MAX_SHIM_ENV`]'s value,
/// mirrored (see [`Shim`]).
pub const MAX_SHIM_ENV: usize = 8;
/// The longest rendered `NAME=VALUE` — `atpkg::shim_env::MAX_ENTRY_BYTES`'s
/// value, mirrored, and re-checked AFTER substitution (design §1.2).
pub const MAX_ENTRY_BYTES: usize = 256;
/// The most `[shim] args` tokens.
pub const MAX_SHIM_ARGS: usize = 16;
/// The most capabilities one harness may declare.
pub const MAX_CAPS: usize = 32;
/// The most probe ids one capability may need.
pub const MAX_NEEDS: usize = 64;
/// The most stdout bytes one probe may print.
pub const MAX_PROBE_STDOUT: usize = 64 * 1024;
/// The most probe files one tree may carry.
pub const MAX_PROBE_FILES: usize = 128;
/// The whole probe run's deadline (design §1.4: "bounded to 5 s").
pub const PROBE_BUDGET: Duration = Duration::from_secs(5);
/// The longest a single probe may take, whatever is left of the budget.
pub const PROBE_ONE_BUDGET: Duration = Duration::from_secs(3);
/// The byte cap on any quoted, tree-controlled text in a rendered reason.
pub const REASON_CAP: usize = 200;
/// The floor on the reap wait AFTER a probe's stdout reached EOF. A process
/// that has closed stdout is exiting; this is the grace it gets to finish
/// doing so before the row says it never did.
const REAP_FLOOR: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// A deliberately minimal TOML reader
// ---------------------------------------------------------------------------

/// One admitted value of the contract's value grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Val {
    Str(String),
    Int(i64),
    Bool(bool),
    Arr(Vec<String>),
}

/// One `[table]` / `[[table]]` block, in file order. The root block is named
/// `""`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Block {
    pub(super) name: String,
    pub(super) array: bool,
    pub(super) keys: Vec<(String, Val)>,
}

impl Block {
    pub(super) fn get(&self, key: &str) -> Option<&Val> {
        self.keys.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

/// Read the contract's TOML SUBSET, or name the first line that leaves it.
///
/// Deliberately NOT a TOML parser, and the reason is the CONTRACT, not the
/// dependency: `aterm-toml` is a first-party workspace crate that fifteen
/// crates here already use, and this crate itself took `aterm-json` for the
/// same job on the JSON side — so "there is no parser" would be false. What
/// is true is that a general parser accepts the whole language, while the
/// contract's grammar is closed on purpose and a reader that skips what it
/// does not understand admits a file it did not read. The subset is small and
/// closed — a
/// `[table]` or `[[table]]` header at column 0, `key = value` under it, `#`
/// comments, blank lines, and four value shapes (a double-quoted string with
/// no escapes, a decimal integer, `true`/`false`, and a ONE-LINE array of
/// double-quoted strings). Anything else — a multi-line array, a dotted key,
/// an inline table, a backslash escape, a single-quoted string — is a REFUSAL
/// naming the line, never a silent skip. A reader that skips what it does not
/// understand admits a file it did not read.
pub(super) fn read_toml(text: &str, file: &str) -> Result<Vec<Block>, String> {
    read_toml_in(text, file, false)
}

/// The same reader with ONE relaxation: a `[table]` header may carry dots,
/// so `[cap.limits.actions]` is one name rather than a refusal.
///
/// It is a separate entry point rather than a flag on the contract's own
/// reader because the two files are different contracts. `harness.toml` is
/// SIGNED and tree-controlled, and its grammar closes on flat names on
/// purpose — `[programs.claude]` there is a refusal, and stays one. The
/// harness's OWN `config.toml` (design §3.7, §5.8.7) is the owner's file and
/// its published keys are dotted (`[cap.limits]`, `[cap.limits.actions]`), so
/// a reader that refused a dot could not read the file the design specifies.
/// KEYS are unchanged on both paths: a dotted key is still a refusal, because
/// `cap.limits.level = 3` written at the root would otherwise read as a key
/// nothing looks up.
pub(super) fn read_toml_dotted(text: &str, file: &str) -> Result<Vec<Block>, String> {
    read_toml_in(text, file, true)
}

/// The value of `[<table>] <key>` in `text`, when it is a bare integer.
///
/// THE reader, not a second one. It exists so that `harness`'s handful of
/// numeric `aterm.toml` knobs (`disk.warn_free_gib`,
/// `disk.target_stale_days`) go through this module's fail-closed grammar
/// rather than through a hand-rolled scanner — `cli.rs` carried a comment
/// saying "this crate has no TOML number reader", which was never true of the
/// crate, only of that file.
///
/// `None` for a file that does not admit, a table or key that is absent, and
/// a value that is not an integer. That is the §1.4 fallback: a read verb
/// behind an unreadable `aterm.toml` answers from the shipped defaults rather
/// than refusing, and `harness config get` is where a refusal is printed in
/// full.
#[must_use]
pub(super) fn toml_int(text: &str, table: &str, key: &str) -> Option<i64> {
    let blocks = read_toml_dotted(text, "aterm.toml").ok()?;
    blocks
        .iter()
        .filter(|b| b.name == table && !b.array)
        .find_map(|b| match b.get(key) {
            Some(Val::Int(v)) => Some(*v),
            _ => None,
        })
}

fn read_toml_in(text: &str, file: &str, dotted_headers: bool) -> Result<Vec<Block>, String> {
    if text.len() > MAX_CONTRACT_BYTES {
        return Err(refusal_len_in(
            file,
            "the file",
            text.len(),
            MAX_CONTRACT_BYTES,
        ));
    }
    let mut blocks: Vec<Block> = vec![Block {
        name: String::new(),
        array: false,
        keys: Vec::new(),
    }];
    for (n, raw) in text.lines().enumerate() {
        if n >= MAX_CONTRACT_LINES {
            return Err(refusal_len_in(file, "the file", n + 1, MAX_CONTRACT_LINES));
        }
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let at = n + 1;
        if let Some(rest) = line.strip_prefix("[[") {
            let name = rest
                .strip_suffix("]]")
                .ok_or_else(|| line_refusal(file, at, line, "an unclosed [[table]] header"))?;
            let name = admit_name(name.trim(), dotted_headers, file, at, line)?;
            push_block(&mut blocks, name, true, file, at, line)?;
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let name = rest
                .strip_suffix(']')
                .ok_or_else(|| line_refusal(file, at, line, "an unclosed [table] header"))?;
            let name = admit_name(name.trim(), dotted_headers, file, at, line)?;
            push_block(&mut blocks, name, false, file, at, line)?;
            continue;
        }
        let (lhs, rhs) = line
            .split_once('=')
            .ok_or_else(|| line_refusal(file, at, line, "neither a header nor key = value"))?;
        let key = admit_name(lhs.trim(), false, file, at, line)?;
        let value = admit_value(rhs.trim(), file, at, line)?;
        let block = blocks
            .last_mut()
            .ok_or_else(|| line_refusal(file, at, line, "no open block"))?;
        if block.keys.iter().any(|(k, _)| *k == key) {
            return Err(line_refusal(
                file,
                at,
                line,
                "a duplicate key in this block",
            ));
        }
        if block.keys.len() >= MAX_KEYS {
            return Err(line_refusal(
                file,
                at,
                line,
                "more keys than a block may carry",
            ));
        }
        block.keys.push((key, value));
    }
    Ok(blocks)
}

fn push_block(
    blocks: &mut Vec<Block>,
    name: String,
    array: bool,
    file: &str,
    at: usize,
    line: &str,
) -> Result<(), String> {
    if blocks.len() >= MAX_BLOCKS {
        return Err(line_refusal(
            file,
            at,
            line,
            "more blocks than a contract may carry",
        ));
    }
    // A plain `[table]` may appear once; `[[table]]` is an array and repeats.
    if !array
        && blocks
            .iter()
            .any(|b| b.name == name && !b.name.is_empty() && !b.array)
    {
        return Err(line_refusal(file, at, line, "a repeated [table] header"));
    }
    if blocks.iter().any(|b| b.name == name && b.array != array) {
        return Err(line_refusal(
            file,
            at,
            line,
            "a name used as both [table] and [[table]]",
        ));
    }
    blocks.push(Block {
        name,
        array,
        keys: Vec::new(),
    });
    Ok(())
}

/// Everything before the first `#` that is not inside a double-quoted string.
fn strip_comment(raw: &str) -> &str {
    let bytes = raw.as_bytes();
    let mut quoted = false;
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'"' => quoted = !quoted,
            b'#' if !quoted => return &raw[..i],
            _ => {}
        }
    }
    raw
}

/// A bare name: `[a-z0-9_-]+`, not empty. Table headers and keys share it, so
/// `[programs.claude]` is refused here rather than silently read as one name —
/// the contract's grammar has no dotted tables.
///
/// `dots` admits `.` INSIDE the name (never at either edge, never doubled),
/// and is set only by [`read_toml_dotted`] for the harness's own
/// `config.toml`, whose published table names are dotted.
fn admit_name(name: &str, dots: bool, file: &str, at: usize, line: &str) -> Result<String, String> {
    if name.is_empty() || name.len() > 64 {
        return Err(line_refusal(
            file,
            at,
            line,
            "a name that is empty or over 64 bytes",
        ));
    }
    if !name.bytes().all(|b| {
        b.is_ascii_lowercase()
            || b.is_ascii_digit()
            || b == b'_'
            || b == b'-'
            || (dots && b == b'.')
    }) {
        return Err(line_refusal(
            file,
            at,
            line,
            if dots {
                "a name outside [a-z0-9_.-] (quoted names are not in this grammar)"
            } else {
                "a name outside [a-z0-9_-] (dotted and quoted names are not in this grammar)"
            },
        ));
    }
    // An edge dot or a doubled dot would name an EMPTY path element, which
    // no lookup can ever match — a refusal rather than a row that silently
    // answers to nothing.
    if dots && (name.starts_with('.') || name.ends_with('.') || name.contains("..")) {
        return Err(line_refusal(
            file,
            at,
            line,
            "a dotted name with an empty element",
        ));
    }
    Ok(name.to_string())
}

fn admit_value(rhs: &str, file: &str, at: usize, line: &str) -> Result<Val, String> {
    if rhs == "true" {
        return Ok(Val::Bool(true));
    }
    if rhs == "false" {
        return Ok(Val::Bool(false));
    }
    if let Some(inner) = rhs.strip_prefix('[') {
        let inner = inner.strip_suffix(']').ok_or_else(|| {
            line_refusal(file, at, line, "an array that does not close on one line")
        })?;
        let mut out = Vec::new();
        for element in inner.split(',') {
            let element = element.trim();
            if element.is_empty() {
                continue;
            }
            if out.len() >= MAX_ARRAY {
                return Err(line_refusal(
                    file,
                    at,
                    line,
                    "more array elements than allowed",
                ));
            }
            out.push(admit_string(element, file, at, line)?);
        }
        return Ok(Val::Arr(out));
    }
    if rhs.starts_with('"') {
        return Ok(Val::Str(admit_string(rhs, file, at, line)?));
    }
    rhs.parse::<i64>().map(Val::Int).map_err(|_| {
        line_refusal(
            file,
            at,
            line,
            "a value that is not a double-quoted string, an integer, a bool or an array",
        )
    })
}

/// A double-quoted string with NO escapes. A backslash is refused rather than
/// passed through: every value in this grammar is a flag, a hostname, an id or
/// a digest, and none of them contains one.
fn admit_string(raw: &str, file: &str, at: usize, line: &str) -> Result<String, String> {
    let inner = raw
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .ok_or_else(|| {
            line_refusal(file, at, line, "a value that is not a double-quoted string")
        })?;
    if inner.contains('"') || inner.contains('\\') {
        return Err(line_refusal(
            file,
            at,
            line,
            "a string carrying a quote or a backslash",
        ));
    }
    if inner.chars().any(char::is_control) {
        return Err(line_refusal(file, at, line, "a control byte in a string"));
    }
    Ok(inner.to_string())
}

fn line_refusal(file: &str, at: usize, line: &str, why: &str) -> String {
    format!("{file} line {at}: {why} — {:?}", truncate_bytes(line, 120))
}

fn refusal_len(what: &str, got: usize, max: usize) -> String {
    refusal_len_in(CONTRACT_FILE, what, got, max)
}

fn refusal_len_in(file: &str, what: &str, got: usize, max: usize) -> String {
    format!("{file}: {what} is {got}, at most {max} allowed")
}

/// The file name the contract's own refusals are prefixed with. The reader
/// itself takes the name as an argument, because `accounts.toml` (design
/// §5.6) uses the same grammar and a refusal naming the wrong file is a wrong
/// answer.
pub(super) const CONTRACT_FILE: &str = "harness.toml";

// ---------------------------------------------------------------------------
// The contract
// ---------------------------------------------------------------------------

/// The wrapping policy a harness declares (design §1.2, §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Hooks and a plugin tree only — nothing of the program is rewritten.
    HooksOnly,
    /// The program's own config files are laid beside it.
    Config,
    /// An elisp layer (design §6.2).
    Elisp,
    /// The harness ships source the program loads.
    Source,
    /// The binary-patch lane (design §7).
    Patch,
}

impl Policy {
    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Policy::HooksOnly => "hooks-only",
            Policy::Config => "config",
            Policy::Elisp => "elisp",
            Policy::Source => "source",
            Policy::Patch => "patch",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "hooks-only" => Policy::HooksOnly,
            "config" => Policy::Config,
            "elisp" => Policy::Elisp,
            "source" => Policy::Source,
            "patch" => Policy::Patch,
            _ => return None,
        })
    }
}

/// The per-program patch-lane column (design §1.2, §7). The default is
/// [`PatchPolicy::Deny`] and a contract that omits the key gets it: the tie
/// breaks toward the answer that rewrites nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PatchPolicy {
    /// No patch recipe may run.
    #[default]
    Deny,
    /// Recipes in `recipes/` may run.
    Allow,
    /// Only a source-level recipe may run.
    Source,
}

impl PatchPolicy {
    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PatchPolicy::Deny => "deny",
            PatchPolicy::Allow => "allow",
            PatchPolicy::Source => "source",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "deny" => PatchPolicy::Deny,
            "allow" => PatchPolicy::Allow,
            "source" => PatchPolicy::Source,
            _ => return None,
        })
    }
}

/// The CLOSED actuator vocabulary of design §4.3. `signal` is deliberately
/// absent: L5 is human-only and is in no harness's vocabulary in ABI 1.
pub const ACTUATORS: &[&str] = &["meta", "notice", "decide", "turn", "key:escape", "restart"];

/// The CLOSED `[shim] args` vocabulary of design §1.2. A flag outside it —
/// `--dangerously-skip-permissions`, `--bare`, `--safe-mode`,
/// `--permission-mode` above all — is a whole-file refusal.
pub const SHIM_FLAGS: &[&str] = &[
    "--plugin-dir",
    "--settings",
    "--model",
    "--fallback-model",
    "-c",
    "-p",
    "-l",
];

/// The two substitutions `[shim]` values may carry, and nothing else.
pub const SUBST_ROOT: &str = "${HARNESS_ROOT}";
/// See [`SUBST_ROOT`].
pub const SUBST_STATE: &str = "${HARNESS_STATE}";

/// Names the rendered `[shim] env` never sets — the shell's own, the loader's
/// and the language runtimes' own loader switches.
///
/// MIRROR of `atpkg::shim_env::NEVER_SET` + `NEVER_SET_PREFIXES`, kept by hand
/// because `aterm-agent` does not depend on `atpkg` and this stage does not
/// add that edge (it would pull the whole supply-chain graph into the agent
/// crate for one list). Design §1.2 calls for exactly this: the host "reuses
/// `ShimEnv::admit`'s shape rule but **not verbatim**", because it substitutes
/// and RE-BOUNDS afterwards. WHAT MUST STAY EQUAL, named so a later coherence
/// test can pin it: this list, [`NEVER_SET_PREFIXES`], [`MAX_SHIM_ENV`] and
/// [`MAX_ENTRY_BYTES`].
pub const NEVER_SET: &[&str] = &[
    "PATH",
    "HOME",
    "IFS",
    "ENV",
    "BASH_ENV",
    "SHELLOPTS",
    "SHELL",
    "PWD",
    "USER",
    "LOGNAME",
    "TMPDIR",
    "PATHEXT",
    "COMSPEC",
    "SYSTEMROOT",
    "NODE_OPTIONS",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "PERL5OPT",
    "RUBYOPT",
];

/// See [`NEVER_SET`].
pub const NEVER_SET_PREFIXES: &[&str] = &["LD_", "DYLD_"];

/// The `[shim]` block: what the `__harness` prelude would export and inject.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Shim {
    /// Admitted, UNSUBSTITUTED `NAME=VALUE` pairs in contract order.
    pub env: Vec<(String, String)>,
    /// Admitted, UNSUBSTITUTED argument tokens in contract order.
    pub args: Vec<String>,
}

impl Shim {
    /// Substitute `${HARNESS_ROOT}` / `${HARNESS_STATE}` and RE-ADMIT the
    /// result (design §1.2: "then re-checks the 256-byte bound on the
    /// **rendered** value"). The sh shim writer single-quotes every value, so
    /// an unsubstituted `${…}` would be exported literally — which is why the
    /// rendered form, not the declared one, is what gets checked.
    ///
    /// # Errors
    /// A rendered value leaves the shape rule (over the byte bound, a control
    /// byte, a quote or percent sign, an edge space), or a rendered argument
    /// still carries a `${`.
    pub fn render(&self, root: &Path, state: &Path) -> Result<Shim, String> {
        let root = root.display().to_string();
        let state = state.display().to_string();
        let sub = |s: &str| s.replace(SUBST_ROOT, &root).replace(SUBST_STATE, &state);
        let mut env = Vec::with_capacity(self.env.len());
        for (name, value) in &self.env {
            let value = sub(value);
            admit_env_value(name, &value)?;
            env.push((name.clone(), value));
        }
        let mut args = Vec::with_capacity(self.args.len());
        for arg in &self.args {
            let arg = sub(arg);
            if arg.contains("${") {
                return Err(format!(
                    "[shim] args {arg:?}: an unknown substitution survived rendering"
                ));
            }
            args.push(arg);
        }
        Ok(Shim { env, args })
    }
}

/// One declared capability (design §1.2's `[[capability]]` block).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    /// The id every hooks.json line names as its third argument.
    pub id: String,
    /// The probe ids this capability stands on. ALL must read `ok`.
    pub needs: Vec<String>,
    /// Probe ids consulted only when the owner opted the behaviour in.
    pub optional: Vec<String>,
    /// The highest actuator level this capability may reach (design §4.3).
    pub max_level: u8,
    /// The declared token bucket, verbatim (`"60/1h"`), or empty.
    pub budget: String,
}

/// One `[[tool]]` row: a NON-INDEX binary a capability depends on, resolved on
/// the session PATH at probe time and never pinned by atpkg (design §1.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRow {
    /// A bare name with no `/`.
    pub bin: String,
    /// The lowest acceptable dotted version, or empty for presence only.
    pub min: String,
    /// The capability ids (or `<cap>.<sub>` sub-ids) this row gates.
    pub caps: Vec<String>,
}

impl ToolRow {
    /// The probe id a `[[tool]]` row contributes: `tool.<bin>` — the spelling
    /// design §3.6 uses in the degradation table (`tool.node`).
    #[must_use]
    pub fn probe_id(&self) -> String {
        let mut s = String::from("tool.");
        s.push_str(&self.bin);
        s
    }
}

/// An ADMITTED `harness.toml`. The only way to build one is [`Contract::admit`],
/// so a host holding a `Contract` never has to re-check the rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    /// The harness ABI the host must speak. Checked against the one ABI rule
    /// by [`abi_supported`] at VERDICT time, not at admission — see
    /// [`Contract::admit`].
    pub abi: u32,
    /// The preference-ordered program family this harness wraps.
    pub wraps_any: Vec<String>,
    /// How it wraps.
    pub policy: Policy,
    /// The patch-lane column.
    pub patch: PatchPolicy,
    /// Every declared capability, in contract order.
    pub capabilities: Vec<Capability>,
    /// The capabilities whose failure means `unsupported`, not `degraded`.
    pub required: Vec<String>,
    /// The actuator vocabulary this harness may reach, ⊆ [`ACTUATORS`].
    pub actuators: Vec<String>,
    /// Declared outbound hostnames; empty means none.
    pub egress: Vec<String>,
    /// `sha256:<64 hex>` over `probes/`, keying the alignment sidecar.
    pub probe_set: String,
    /// The prelude's env and args.
    pub shim: Shim,
    /// The non-index tools capabilities depend on.
    pub tools: Vec<ToolRow>,
}

impl Contract {
    /// Read and ADMIT a `harness.toml`, or name the first thing that breaks
    /// the rule. The refusal is the whole file's: nothing partial is returned.
    ///
    /// **The ABI is admitted as a POSITIVE INTEGER here, and its RANGE is
    /// checked at verdict time.** Design §1.2 states admission as including
    /// the range, and §3.8 needs the row `unsupported — needs aterm ABI 2,
    /// running 1`. Both cannot hold if a range miss is a parse refusal: the
    /// number the row must print would be the one thing thrown away. So the
    /// structural rule is here and the range is [`evaluate`]'s first act,
    /// which renders exactly that row and NOTHING else — no capability, no
    /// prelude. Reported as a deviation.
    ///
    /// # Errors
    /// Any departure from design §1.2's admission: an unknown key, a value
    /// outside a closed vocabulary, a `[shim] args` flag that is not in
    /// [`SHIM_FLAGS`], an env entry the shim may never set, an egress list
    /// over [`MAX_EGRESS`], a `probe_set` that is not `sha256:<64 hex>`, a
    /// `[[capability]]` id that is not in `capabilities`, a `required` name
    /// that is not a declared capability, a `[[tool]]` row of an unknown probe
    /// kind, or any line outside the value grammar.
    pub fn admit(text: &str) -> Result<Contract, String> {
        let blocks = read_toml(text, CONTRACT_FILE)?;
        let root = blocks
            .iter()
            .find(|b| b.name.is_empty())
            .ok_or_else(|| String::from("harness.toml: no root block"))?;
        for (key, _) in &root.keys {
            if !ROOT_KEYS.contains(&key.as_str()) {
                return Err(unknown_key("", key));
            }
        }
        for block in &blocks {
            if !block.name.is_empty() && !BLOCK_NAMES.contains(&block.name.as_str()) {
                return Err(format!(
                    "harness.toml: unknown block [{}]; this grammar has {}",
                    block.name,
                    BLOCK_NAMES.join(", ")
                ));
            }
        }

        let abi = int(root, "abi")?.ok_or_else(|| missing("abi"))?;
        let abi = u32::try_from(abi)
            .ok()
            .filter(|a| *a >= 1)
            .ok_or_else(|| String::from("harness.toml: abi must be a positive integer"))?;

        let wraps_any = arr(root, "wraps_any")?.unwrap_or_default();
        if wraps_any.is_empty() || wraps_any.len() > MAX_WRAPS {
            return Err(format!(
                "harness.toml: wraps_any names {} programs; 1..={MAX_WRAPS} allowed",
                wraps_any.len()
            ));
        }
        for name in &wraps_any {
            if !is_program_name(name) {
                return Err(format!(
                    "harness.toml: wraps_any {name:?} is not a program name"
                ));
            }
        }

        let policy = string(root, "policy")?
            .as_deref()
            .map_or(Some(Policy::HooksOnly), Policy::parse)
            .ok_or_else(|| {
                String::from(
                    "harness.toml: policy must be hooks-only | config | elisp | source | patch",
                )
            })?;
        let patch = string(root, "patch")?
            .as_deref()
            .map_or(Some(PatchPolicy::Deny), PatchPolicy::parse)
            .ok_or_else(|| String::from("harness.toml: patch must be deny | allow | source"))?;

        let declared = arr(root, "capabilities")?.unwrap_or_default();
        if declared.is_empty() || declared.len() > MAX_CAPS {
            return Err(format!(
                "harness.toml: capabilities names {} ids; 1..={MAX_CAPS} allowed",
                declared.len()
            ));
        }
        for id in &declared {
            if !is_cap_id(id) {
                return Err(format!(
                    "harness.toml: capability id {id:?} is not [a-z0-9-]"
                ));
            }
        }
        if let Some(dup) = first_duplicate(&declared) {
            return Err(format!(
                "harness.toml: capability {dup:?} is declared twice"
            ));
        }

        let actuators = arr(root, "actuators")?.unwrap_or_default();
        for a in &actuators {
            if !ACTUATORS.contains(&a.as_str()) {
                return Err(format!(
                    "harness.toml: actuator {a:?} is outside the closed vocabulary ({})",
                    ACTUATORS.join(", ")
                ));
            }
        }

        let egress = arr(root, "egress")?.unwrap_or_default();
        if egress.len() > MAX_EGRESS {
            return Err(refusal_len("egress", egress.len(), MAX_EGRESS));
        }
        for host in &egress {
            if !is_hostname(host) {
                return Err(format!(
                    "harness.toml: egress {host:?} is not an exact hostname"
                ));
            }
        }

        let probe_set = string(root, "probe_set")?.unwrap_or_default();
        if !probe_set.is_empty() && !is_sha256(&probe_set) {
            return Err(String::from(
                "harness.toml: probe_set must be sha256:<64 hex>",
            ));
        }

        let required = arr(root, "required")?.unwrap_or_default();
        for id in &required {
            if !declared.contains(id) {
                return Err(format!(
                    "harness.toml: required {id:?} is not a declared capability"
                ));
            }
        }

        let shim = admit_shim(&blocks)?;
        let capabilities = admit_capabilities(&blocks, &declared)?;
        let tools = admit_tools(&blocks, &capabilities)?;

        Ok(Contract {
            abi,
            wraps_any,
            policy,
            patch,
            capabilities,
            required,
            actuators,
            egress,
            probe_set,
            shim,
            tools,
        })
    }

    /// The capability with this id, if it is declared.
    #[must_use]
    pub fn capability(&self, id: &str) -> Option<&Capability> {
        self.capabilities.iter().find(|c| c.id == id)
    }

    /// Whether this harness wraps `program`.
    #[must_use]
    pub fn wraps(&self, program: &str) -> bool {
        self.wraps_any.iter().any(|p| p == program)
    }

    /// The first eight hex characters of `probe_set`, which is what the signed
    /// alignment row carries (design §3.4's `#<probe_set[:8]>`). Empty when no
    /// `probe_set` is declared.
    #[must_use]
    pub fn probe_set_short(&self) -> String {
        probe_set_short(&self.probe_set)
    }
}

/// The root keys this grammar knows. An unknown one refuses the file.
const ROOT_KEYS: &[&str] = &[
    "abi",
    "wraps_any",
    "policy",
    "patch",
    "capabilities",
    "required",
    "actuators",
    "egress",
    "probe_set",
];

/// The block names this grammar knows.
const BLOCK_NAMES: &[&str] = &["shim", "capability", "tool"];

const SHIM_KEYS: &[&str] = &["env", "args"];
const CAP_KEYS: &[&str] = &["id", "needs", "optional", "max_level", "budget"];
const TOOL_KEYS: &[&str] = &["bin", "probe", "min", "caps"];

fn admit_shim(blocks: &[Block]) -> Result<Shim, String> {
    let Some(block) = blocks.iter().find(|b| b.name == "shim") else {
        return Ok(Shim::default());
    };
    for (key, _) in &block.keys {
        if !SHIM_KEYS.contains(&key.as_str()) {
            return Err(unknown_key("shim", key));
        }
    }
    let raw_env = arr(block, "env")?.unwrap_or_default();
    if raw_env.len() > MAX_SHIM_ENV {
        return Err(refusal_len("[shim] env", raw_env.len(), MAX_SHIM_ENV));
    }
    let mut env: Vec<(String, String)> = Vec::with_capacity(raw_env.len());
    for entry in &raw_env {
        let (name, value) = admit_env_entry(entry)?;
        if env.iter().any(|(n, _)| *n == name) {
            return Err(format!("[shim] env {entry:?}: a duplicate name"));
        }
        env.push((name, value));
    }
    let args = arr(block, "args")?.unwrap_or_default();
    if args.len() > MAX_SHIM_ARGS {
        return Err(refusal_len("[shim] args", args.len(), MAX_SHIM_ARGS));
    }
    // A flag's VALUE follows it, so the walk alternates: a token starting with
    // `-` must be in the closed set, and the token after it is its value.
    let mut i = 0;
    while i < args.len() {
        let token = &args[i];
        if !token.starts_with('-') {
            return Err(format!(
                "[shim] args {token:?}: a bare value with no flag before it"
            ));
        }
        if !SHIM_FLAGS.contains(&token.as_str()) {
            return Err(format!(
                "[shim] args {token:?}: outside the closed vocabulary ({})",
                SHIM_FLAGS.join(" ")
            ));
        }
        i += 1;
        if let Some(value) = args.get(i) {
            if value.starts_with('-') {
                // The next token is another flag: this one took no value.
                continue;
            }
            admit_shim_value(value)?;
            i += 1;
        }
    }
    Ok(Shim { env, args })
}

/// A `[shim] args` VALUE: no `..`, no control byte, and any `${…}` it carries
/// is one of the two named substitutions.
fn admit_shim_value(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > MAX_ENTRY_BYTES {
        return Err(format!(
            "[shim] args {value:?}: empty or over the byte bound"
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("[shim] args {value:?}: a control byte"));
    }
    let stripped = value.replace(SUBST_ROOT, "").replace(SUBST_STATE, "");
    if stripped.contains("${") || stripped.contains('$') {
        return Err(format!(
            "[shim] args {value:?}: a substitution that is not {SUBST_ROOT} or {SUBST_STATE}"
        ));
    }
    if stripped.split('/').any(|c| c == "..") {
        return Err(format!("[shim] args {value:?}: a `..` component"));
    }
    Ok(())
}

/// `NAME=VALUE` under the mirrored shim_env shape rule (see [`NEVER_SET`]).
fn admit_env_entry(entry: &str) -> Result<(String, String), String> {
    if entry.len() > MAX_ENTRY_BYTES {
        return Err(format!(
            "[shim] env {entry:?}: longer than {MAX_ENTRY_BYTES} bytes"
        ));
    }
    let Some((name, value)) = entry.split_once('=') else {
        return Err(format!("[shim] env {entry:?}: not NAME=VALUE"));
    };
    if name.is_empty() {
        return Err(format!("[shim] env {entry:?}: an empty name"));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(format!("[shim] env {entry:?}: name is not [A-Z0-9_]+"));
    }
    if name.as_bytes()[0].is_ascii_digit() {
        return Err(format!(
            "[shim] env {entry:?}: name is digit-led (not a valid sh identifier)"
        ));
    }
    if NEVER_SET.contains(&name) || NEVER_SET_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return Err(format!(
            "[shim] env {entry:?}: names a variable the shim never sets (the shell's, the loader's)"
        ));
    }
    admit_env_value(name, value)?;
    Ok((name.to_string(), value.to_string()))
}

/// The VALUE half of the shape rule, applied to the DECLARED value at
/// admission and again to the RENDERED value in [`Shim::render`].
fn admit_env_value(name: &str, value: &str) -> Result<(), String> {
    let show = || {
        let mut s = String::from(name);
        s.push('=');
        s.push_str(truncate_bytes(value, 80));
        s
    };
    if value.is_empty() {
        return Err(format!("[shim] env {:?}: an empty value", show()));
    }
    if name.len() + 1 + value.len() > MAX_ENTRY_BYTES {
        return Err(format!(
            "[shim] env {:?}: the rendered entry is longer than {MAX_ENTRY_BYTES} bytes",
            show()
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(format!(
            "[shim] env {:?}: a control byte in the value",
            show()
        ));
    }
    if value.trim() != value {
        return Err(format!(
            "[shim] env {:?}: the value starts or ends with whitespace",
            show()
        ));
    }
    if value.contains('"') || value.contains('%') {
        return Err(format!(
            "[shim] env {:?}: a quote or percent sign in the value",
            show()
        ));
    }
    Ok(())
}

fn admit_capabilities(blocks: &[Block], declared: &[String]) -> Result<Vec<Capability>, String> {
    let mut out: Vec<Capability> = Vec::new();
    for block in blocks.iter().filter(|b| b.name == "capability") {
        for (key, _) in &block.keys {
            if !CAP_KEYS.contains(&key.as_str()) {
                return Err(unknown_key("capability", key));
            }
        }
        let id = string(block, "id")?.ok_or_else(|| missing("[[capability]] id"))?;
        if !declared.contains(&id) {
            return Err(format!(
                "harness.toml: [[capability]] {id:?} is not in the capabilities list"
            ));
        }
        if out.iter().any(|c: &Capability| c.id == id) {
            return Err(format!("harness.toml: [[capability]] {id:?} appears twice"));
        }
        let needs = arr(block, "needs")?.unwrap_or_default();
        let optional = arr(block, "optional")?.unwrap_or_default();
        if needs.len() + optional.len() > MAX_NEEDS {
            return Err(refusal_len("a capability's needs", needs.len(), MAX_NEEDS));
        }
        for probe in needs.iter().chain(optional.iter()) {
            if !is_probe_id(probe) {
                return Err(format!(
                    "harness.toml: [[capability]] {id:?} needs {probe:?}, which is not <kind>.<name>"
                ));
            }
        }
        let max_level = int(block, "max_level")?.unwrap_or(0);
        let max_level = u8::try_from(max_level)
            .ok()
            .filter(|l| *l <= MAX_ACTUATOR_LEVEL)
            .ok_or_else(|| {
                format!(
                    "harness.toml: [[capability]] {id:?} max_level must be 0..={MAX_ACTUATOR_LEVEL}"
                )
            })?;
        let budget = string(block, "budget")?.unwrap_or_default();
        if !budget.is_empty() && !is_budget(&budget) {
            return Err(format!(
                "harness.toml: [[capability]] {id:?} budget {budget:?} is not <n>/<n><s|m|h|d>"
            ));
        }
        out.push(Capability {
            id,
            needs,
            optional,
            max_level,
            budget,
        });
    }
    for id in declared {
        if !out.iter().any(|c| c.id == *id) {
            return Err(format!(
                "harness.toml: capability {id:?} is declared but has no [[capability]] block"
            ));
        }
    }
    Ok(out)
}

/// L5 (`signal`) is human-only, so an ABI-1 contract may not declare it.
const MAX_ACTUATOR_LEVEL: u8 = 4;

fn admit_tools(blocks: &[Block], caps: &[Capability]) -> Result<Vec<ToolRow>, String> {
    let mut out: Vec<ToolRow> = Vec::new();
    for block in blocks.iter().filter(|b| b.name == "tool") {
        if out.len() >= MAX_TOOLS {
            return Err(refusal_len("[[tool]] rows", out.len() + 1, MAX_TOOLS));
        }
        for (key, _) in &block.keys {
            if !TOOL_KEYS.contains(&key.as_str()) {
                return Err(unknown_key("tool", key));
            }
        }
        let bin = string(block, "bin")?.ok_or_else(|| missing("[[tool]] bin"))?;
        if bin.is_empty() || bin.len() > 32 || bin.contains('/') || !is_program_name(&bin) {
            return Err(format!(
                "harness.toml: [[tool]] bin {bin:?} must be a bare name of at most 32 bytes"
            ));
        }
        let probe = string(block, "probe")?.unwrap_or_default();
        if probe != "version" {
            return Err(format!(
                "harness.toml: [[tool]] {bin:?} probe {probe:?}; every tool row is a `version` probe"
            ));
        }
        let min = string(block, "min")?.unwrap_or_default();
        if !min.is_empty() && !is_dotted_number(&min) {
            return Err(format!(
                "harness.toml: [[tool]] {bin:?} min {min:?} is not a dotted number"
            ));
        }
        let tool_caps = arr(block, "caps")?.unwrap_or_default();
        if tool_caps.is_empty() {
            return Err(format!(
                "harness.toml: [[tool]] {bin:?} names no caps, so nothing would degrade"
            ));
        }
        for cap in &tool_caps {
            let head = cap.split('.').next().unwrap_or(cap);
            if !caps.iter().any(|c| c.id == head) {
                return Err(format!(
                    "harness.toml: [[tool]] {bin:?} gates {cap:?}, which is not a declared capability"
                ));
            }
        }
        if out.iter().any(|t| t.bin == bin) {
            return Err(format!("harness.toml: [[tool]] {bin:?} appears twice"));
        }
        out.push(ToolRow {
            bin,
            min,
            caps: tool_caps,
        });
    }
    Ok(out)
}

// --- small admission helpers ------------------------------------------------

fn unknown_key(block: &str, key: &str) -> String {
    if block.is_empty() {
        format!("harness.toml: unknown key {key:?} at the root")
    } else {
        format!("harness.toml: unknown key {key:?} in [{block}]")
    }
}

fn missing(what: &str) -> String {
    format!("harness.toml: {what} is required")
}

fn string(block: &Block, key: &str) -> Result<Option<String>, String> {
    match block.get(key) {
        None => Ok(None),
        Some(Val::Str(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("harness.toml: {key} must be a string")),
    }
}

fn int(block: &Block, key: &str) -> Result<Option<i64>, String> {
    match block.get(key) {
        None => Ok(None),
        Some(Val::Int(i)) => Ok(Some(*i)),
        Some(_) => Err(format!("harness.toml: {key} must be an integer")),
    }
}

fn arr(block: &Block, key: &str) -> Result<Option<Vec<String>>, String> {
    match block.get(key) {
        None => Ok(None),
        Some(Val::Arr(a)) => Ok(Some(a.clone())),
        Some(_) => Err(format!("harness.toml: {key} must be an array of strings")),
    }
}

fn first_duplicate(list: &[String]) -> Option<&String> {
    list.iter()
        .enumerate()
        .find(|(i, v)| list[..*i].contains(v))
        .map(|(_, v)| v)
}

fn is_program_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn is_cap_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 48
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A probe id is `<kind>.<name>`: the kind keys the adaptation contract of
/// §3.2 and the name is the thing measured.
fn is_probe_id(s: &str) -> bool {
    let Some((kind, name)) = s.split_once('.') else {
        return false;
    };
    !kind.is_empty()
        && !name.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
}

fn is_hostname(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && !s.starts_with('.')
        && !s.ends_with('.')
        && !s.contains("..")
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
}

fn is_sha256(s: &str) -> bool {
    s.strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn is_dotted_number(s: &str) -> bool {
    !s.is_empty()
        && s.split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `<n>/<n><s|m|h|d>` — design §1.2's `"60/1h"`.
fn is_budget(s: &str) -> bool {
    let Some((count, window)) = s.split_once('/') else {
        return false;
    };
    if count.is_empty() || !count.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let Some(unit) = window.chars().last() else {
        return false;
    };
    let head = &window[..window.len() - unit.len_utf8()];
    matches!(unit, 's' | 'm' | 'h' | 'd')
        && !head.is_empty()
        && head.bytes().all(|b| b.is_ascii_digit())
}

/// The first eight hex characters of a `sha256:…` digest, or empty.
#[must_use]
pub fn probe_set_short(probe_set: &str) -> String {
    probe_set
        .strip_prefix("sha256:")
        .map(|hex| hex.chars().take(8).collect())
        .unwrap_or_default()
}

/// The digest over a probe DIRECTORY, as `probe_set` must equal (design §1.2:
/// "`probe_set` must equal the sha256 the host computes over `probes/`").
///
/// The pre-image is fixed here, once: for each regular file in the directory in
/// BYTE-SORTED name order, the name, a NUL, the decimal length, a NUL, the
/// bytes, a NUL. Subdirectories and symlinks are skipped and NAMED in the
/// error, because a probe tree that carries one is not the tree this digest
/// describes.
///
/// # Errors
/// The directory cannot be read, carries more than [`MAX_PROBE_FILES`] files,
/// or holds an entry that is not a regular file.
pub fn probe_set_digest(dir: &Path) -> Result<String, String> {
    let mut files: Vec<(Vec<u8>, PathBuf)> = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let meta = entry
            .path()
            .symlink_metadata()
            .map_err(|e| format!("{}: {e}", entry.path().display()))?;
        if !meta.is_file() {
            return Err(format!(
                "{}: not a regular file; the probe digest describes files only",
                entry.path().display()
            ));
        }
        if files.len() >= MAX_PROBE_FILES {
            return Err(refusal_len("probes/", files.len() + 1, MAX_PROBE_FILES));
        }
        files.push((entry.file_name().as_encoded_bytes().to_vec(), entry.path()));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = aterm_digest::Sha256::new();
    for (name, path) in &files {
        let body = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        hasher.update(name);
        hasher.update([0u8]);
        hasher.update(body.len().to_string().as_bytes());
        hasher.update([0u8]);
        hasher.update(&body);
        hasher.update([0u8]);
    }
    let mut out = String::from("sha256:");
    for byte in hasher.finalize() {
        out.push(hex_nibble(byte >> 4));
        out.push(hex_nibble(byte & 0x0f));
    }
    Ok(out)
}

fn hex_nibble(n: u8) -> char {
    char::from_digit(u32::from(n), 16).unwrap_or('0')
}

// ---------------------------------------------------------------------------
// Probes (design §3.2)
// ---------------------------------------------------------------------------

/// What one probe measured.
///
/// The three words are not interchangeable and the difference is the whole
/// point of design §3.2's "exits 0 always": `absent` is a MEASUREMENT and
/// `error` is the absence of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeStatus {
    /// The thing is there, at the shape the probe expected.
    Ok,
    /// The probe RAN and measured that the thing is not there.
    Absent,
    /// The probe did not produce a measurement: it crashed, exited non-zero,
    /// printed nothing or garbage, claimed an id that is not its own, or ran
    /// past the deadline.
    Error,
}

impl ProbeStatus {
    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeStatus::Ok => "ok",
            ProbeStatus::Absent => "absent",
            ProbeStatus::Error => "error",
        }
    }

    /// The spelling a probe may print. An unknown word is NOT accepted — the
    /// caller turns that into [`ProbeStatus::Error`], never into a guess.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ok" => ProbeStatus::Ok,
            "absent" => ProbeStatus::Absent,
            "error" => ProbeStatus::Error,
            _ => return None,
        })
    }
}

/// One probe's measurement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    /// The probe id — `<kind>.<name>`, the key a capability's `needs` uses.
    pub id: String,
    /// What it measured.
    pub status: ProbeStatus,
    /// One bounded sentence: what it saw, or why there is no measurement.
    pub detail: String,
}

impl ProbeResult {
    /// An error result with `why` as its detail.
    #[must_use]
    pub fn error(id: &str, why: &str) -> Self {
        ProbeResult {
            id: id.to_string(),
            status: ProbeStatus::Error,
            detail: truncate_bytes(why, REASON_CAP).to_string(),
        }
    }

    /// Read one probe's stdout under design §3.2's contract, FAIL-CLOSED.
    ///
    /// `expect_id` is the id the file's NAME says this is. A JSON `id` that
    /// disagrees is an [`ProbeStatus::Error`], not a rename: the needs graph
    /// keys on the id, so a probe that can choose its own id at runtime can
    /// satisfy a capability it was never written for.
    #[must_use]
    pub fn parse(expect_id: &str, stdout: &str) -> Self {
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return ProbeResult::error(expect_id, "the probe printed nothing");
        }
        let Ok(value) = aterm_json::from_str::<Value>(trimmed) else {
            return ProbeResult::error(expect_id, "the probe's output is not one JSON object");
        };
        let Value::Object(object) = value else {
            return ProbeResult::error(expect_id, "the probe's output is not a JSON object");
        };
        if let Some(Value::String(id)) = object.get("id")
            && id != expect_id
        {
            return ProbeResult::error(
                expect_id,
                &format!("the probe claims id {:?}", truncate_bytes(id, 64)),
            );
        }
        let detail = match object.get("detail") {
            Some(Value::String(d)) => truncate_bytes(d, REASON_CAP).to_string(),
            _ => String::new(),
        };
        let Some(Value::String(status)) = object.get("status") else {
            return ProbeResult::error(expect_id, "the probe printed no status");
        };
        let Some(status) = ProbeStatus::parse(status) else {
            return ProbeResult::error(
                expect_id,
                &format!("the probe printed status {:?}", truncate_bytes(status, 32)),
            );
        };
        ProbeResult {
            id: expect_id.to_string(),
            status,
            detail,
        }
    }
}

/// What a probe is run against (design §3.3's `CANDIDATE` / `TREE`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeCtx {
    /// The installed program the probes measure.
    pub candidate: PathBuf,
    /// The harness tree the probes live in.
    pub tree: PathBuf,
    /// The harness's durable state directory.
    pub state: PathBuf,
    /// Whether the `online` kind may spend the operator's quota. Default
    /// FALSE, and the probe reads it as `PROBE_ONLINE`.
    pub online: bool,
}

/// The outcome of one probe pass.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProbeRun {
    /// One result per probe file, in byte-sorted id order.
    pub results: Vec<ProbeResult>,
    /// Why there are no results at all, when there are none.
    pub refusal: Option<String>,
}

impl ProbeRun {
    /// The result for `id`, if the pass produced one.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&ProbeResult> {
        self.results.iter().find(|r| r.id == id)
    }
}

/// Run every probe in `dir` against `ctx`, inside `budget` TOTAL.
///
/// NOT A POLL: each child's stdout is read on one thread while this one blocks
/// in `recv_timeout`, which is a deadline. When the budget is spent, the probes
/// that did not run read [`ProbeStatus::Error`] with that as the reason — never
/// `absent`, because nothing measured them.
///
/// The child inherits nothing it was not given: the environment is cleared and
/// rebuilt from `PATH`, `HOME`, `TMPDIR` and the four probe variables, so a
/// probe cannot be steered by whatever the session happened to export.
#[must_use]
pub fn run_probes(dir: &Path, ctx: &ProbeCtx, budget: Duration) -> ProbeRun {
    let mut ids: Vec<(String, PathBuf)> = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            return ProbeRun {
                results: Vec::new(),
                refusal: Some(format!("{}: {e}", dir.display())),
            };
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_probe_id(&name) {
            // Not a probe id, so nothing can NEED it: a README beside the
            // probes is not an error, it is not a probe.
            continue;
        }
        if ids.len() >= MAX_PROBE_FILES {
            break;
        }
        ids.push((name, entry.path()));
    }
    ids.sort_by(|a, b| a.0.cmp(&b.0));

    let started = std::time::Instant::now();
    let mut results = Vec::with_capacity(ids.len());
    for (id, path) in ids {
        let left = budget.saturating_sub(started.elapsed());
        if left.is_zero() {
            results.push(ProbeResult::error(
                &id,
                "the probe budget was spent before this probe ran",
            ));
            continue;
        }
        results.push(run_one(&id, &path, ctx, left.min(PROBE_ONE_BUDGET)));
    }
    ProbeRun {
        results,
        refusal: None,
    }
}

fn run_one(id: &str, path: &Path, ctx: &ProbeCtx, budget: Duration) -> ProbeResult {
    let mut cmd = Command::new(path);
    cmd.env("CANDIDATE", &ctx.candidate)
        .env("TREE", &ctx.tree)
        .env("HARNESS_ROOT", &ctx.tree)
        .env("HARNESS_STATE", &ctx.state)
        .env("PROBE_ONLINE", if ctx.online { "1" } else { "0" });
    match capture(cmd, budget) {
        Ok(bytes) => ProbeResult::parse(id, &String::from_utf8_lossy(&bytes)),
        Err(why) => ProbeResult::error(id, &why),
    }
}

/// What one bounded child did: the bytes it printed, and how it left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    /// Its stdout, capped at the caller's byte budget.
    pub stdout: Vec<u8>,
    /// Its exit code; `None` when a signal took it.
    pub code: Option<i32>,
}

impl Captured {
    /// Did it exit 0?
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Run one bounded child and hand back its stdout, or the SENTENCE that says why
/// there is none. `capture` is the probe-shaped wrapper; this is the shape
/// every OTHER subprocess in this crate's harness borrows, because a
/// deadline-free `wait_with_output` is a hang waiting for a stale mount or a
/// user command that loops.
///
/// The environment is CLEARED and rebuilt from `PATH`, `HOME`, `TMPDIR` plus
/// whatever the caller already set on `cmd`, so a child cannot be steered by
/// whatever the session happened to export. Nothing here polls: one thread
/// reads stdout while this one blocks in `recv_timeout`, which is a deadline.
/// `stdin` is written on a thread of its own for the same reason — a child
/// that never reads must not be able to block the writer once the pipe fills.
///
/// # Errors
///
/// The child will not spawn, its stdout cannot be captured, it ran past
/// `budget`, or it closed its output and did not exit.
pub fn capture_bounded(
    mut cmd: Command,
    budget: Duration,
    stdin: Option<Vec<u8>>,
    max_stdout: usize,
) -> Result<Captured, String> {
    let mut keep: Vec<(std::ffi::OsString, std::ffi::OsString)> = Vec::new();
    for (name, value) in cmd.get_envs() {
        if let Some(value) = value {
            keep.push((name.to_os_string(), value.to_os_string()));
        }
    }
    cmd.env_clear()
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for pass in ["PATH", "HOME", "TMPDIR"] {
        if let Some(value) = std::env::var_os(pass) {
            cmd.env(pass, value);
        }
    }
    for (name, value) in keep {
        cmd.env(name, value);
    }
    let mut child = cmd.spawn().map_err(|e| format!("it will not run: {e}"))?;
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // The handle MOVES onto the thread, so the drop that closes the pipe
        // happens there: a child reading to EOF still sees one.
        std::thread::spawn(move || {
            let _ = pipe.write_all(&bytes);
        });
    }
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(String::from("its stdout could not be captured"));
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let read = stdout
            .take(max_stdout as u64)
            .read_to_end(&mut buf)
            .map(|_| buf);
        let _ = tx.send(read);
    });
    let Ok(read) = rx.recv_timeout(budget) else {
        // It is still holding stdout open, so it is still running: this is
        // the deadline, and killing it here is safe.
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!(
            "it ran past its {} ms deadline",
            budget.as_millis()
        ));
    };
    let bytes = read.map_err(|e| format!("its output was unreadable: {e}"))?;
    let (wait_tx, wait_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = wait_tx.send(child.wait());
    });
    let status = wait_rx
        .recv_timeout(budget.max(REAP_FLOOR))
        .map_err(|_| String::from("it closed its output and did not exit"))?
        .map_err(|e| format!("it could not be reaped: {e}"))?;
    Ok(Captured {
        stdout: bytes,
        code: status.code(),
    })
}

/// One bounded PROBE run: [`capture_bounded`] with §3.2's rule on top —
/// exit 0 always, so any other exit is the sentence that says so — and the
/// probe's own stdout cap.
fn capture(cmd: Command, budget: Duration) -> Result<Vec<u8>, String> {
    let got = capture_bounded(cmd, budget, None, MAX_PROBE_STDOUT)
        .map_err(|e| format!("the probe: {e}"))?;
    match got.code {
        Some(0) => Ok(got.stdout),
        Some(code) => Err(format!("the probe exited {code}; §3.2 says exit 0 always")),
        None => Err(String::from("the probe was killed by a signal")),
    }
}

/// `<bin> --version`, bounded. `bin` is either a BARE NAME, which the OS
/// resolves on the session PATH — the `[[tool]]` row's rule, and why admission
/// refuses a `bin` containing `/` — or a path a caller already resolved, which
/// is how the wrapped program's own version is read.
///
/// `None` is "the name did not resolve or did not answer": the caller turns
/// that into `absent`, which is a measurement, and an answer carrying no
/// version into `error`, which is not.
#[must_use]
pub fn tool_version(bin: &str, budget: Duration) -> Option<String> {
    if bin.is_empty() {
        return None;
    }
    let mut cmd = Command::new(bin);
    cmd.arg("--version");
    capture(cmd, budget)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// Measure one `[[tool]]` row through an INJECTED runner, so the decision is a
/// pure function in tests and a subprocess in production.
///
/// `run` is handed the binary's bare name and answers its `--version` output,
/// or `None` when the name does not resolve on the session PATH. Design §1.2:
/// "absent or below ⇒ exactly the named caps degrade".
pub fn probe_tool(row: &ToolRow, run: &mut dyn FnMut(&str) -> Option<String>) -> ProbeResult {
    let id = row.probe_id();
    let Some(output) = run(&row.bin) else {
        return ProbeResult {
            id,
            status: ProbeStatus::Absent,
            detail: format!("tool {}: absent", row.bin),
        };
    };
    let Some(found) = first_dotted_number(&output) else {
        return ProbeResult::error(
            &id,
            &format!(
                "tool {}: {:?} carries no dotted version",
                row.bin,
                truncate_bytes(output.trim(), 64)
            ),
        );
    };
    if row.min.is_empty() || version_at_least(&found, &row.min) {
        return ProbeResult {
            id,
            status: ProbeStatus::Ok,
            detail: format!("tool {}: {found}", row.bin),
        };
    }
    ProbeResult {
        id,
        status: ProbeStatus::Absent,
        detail: format!("tool {}: {found} < {}", row.bin, row.min),
    }
}

/// The first dotted number in `text`, with a leading `v` tolerated — `node`
/// prints `v26.3.0` and `claude` prints `2.1.274 (Claude Code)` (both MEASURED,
/// design §3.2).
#[must_use]
pub fn first_dotted_number(text: &str) -> Option<String> {
    for token in text.split(|c: char| c.is_whitespace() || c == '(' || c == ')') {
        let token = token.trim_start_matches('v').trim_end_matches(',');
        if is_dotted_number(token) && token.contains('.') {
            return Some(token.to_string());
        }
    }
    // A bare major with no dot ("20") is still a version.
    text.split_whitespace()
        .map(|t| t.trim_start_matches('v'))
        .find(|t| is_dotted_number(t))
        .map(str::to_string)
}

/// Component-wise `found >= min` over dotted numbers. A missing component is
/// zero, so `20` satisfies `20.0.0` and `2.1` does not satisfy `2.1.1`.
#[must_use]
pub fn version_at_least(found: &str, min: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let (found, min) = (parse(found), parse(min));
    for i in 0..found.len().max(min.len()) {
        let f = found.get(i).copied().unwrap_or(0);
        let m = min.get(i).copied().unwrap_or(0);
        if f != m {
            return f > m;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// The verdict (design §3.5, §3.6, §3.8)
// ---------------------------------------------------------------------------

/// The four words of design §3.8's vocabulary, as DATA.
///
/// The per-program ROW is spelled in `atpkg::harness`; this is the wire word that
/// keys it ([`Alignment::wire`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Every capability is on.
    Aligned,
    /// Some capabilities are off, named here in declaration order.
    Degraded(Vec<String>),
    /// The harness is not usable against this program, and why.
    Unsupported(String),
    /// Nothing has been measured yet.
    Pending,
}

impl Verdict {
    /// The bare word, with no counts and no list: `aligned`, `degraded`,
    /// `unsupported`, `pending`.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Verdict::Aligned => "aligned",
            Verdict::Degraded(_) => "degraded",
            Verdict::Unsupported(_) => "unsupported",
            Verdict::Pending => "pending",
        }
    }

    /// The SIGNED-ROW spelling of design §3.4: `aligned`,
    /// `degraded:<caps>`, `unsupported:<reason>`, `pending`. No counts — the
    /// signed carrier does not have them.
    #[must_use]
    pub fn as_wire(&self) -> String {
        match self {
            Verdict::Aligned => String::from("aligned"),
            Verdict::Pending => String::from("pending"),
            Verdict::Degraded(caps) => {
                let mut s = String::from("degraded:");
                s.push_str(&caps.join(","));
                s
            }
            Verdict::Unsupported(reason) => {
                let mut s = String::from("unsupported:");
                s.push_str(reason);
                s
            }
        }
    }

    /// Read back [`Verdict::as_wire`], and also the counted form
    /// [`Alignment::wire`] writes (`aligned(7/7)`, `degraded(5/7):a,b`). An
    /// unknown word is `None`, never a guess.
    #[must_use]
    pub fn parse_wire(s: &str) -> Option<Self> {
        let (head, tail) = match s.split_once(':') {
            Some((head, tail)) => (head, Some(tail)),
            None => (s, None),
        };
        // `aligned(7/7)` → `aligned`; the counts are display, not vocabulary.
        let head = head.split_once('(').map_or(head, |(h, _)| h);
        Some(match head {
            "aligned" => Verdict::Aligned,
            "pending" => Verdict::Pending,
            "degraded" => Verdict::Degraded(
                tail.unwrap_or_default()
                    .split(',')
                    .filter(|c| !c.is_empty())
                    .map(str::to_string)
                    .collect(),
            ),
            "unsupported" => Verdict::Unsupported(tail.unwrap_or_default().to_string()),
            _ => return None,
        })
    }

    /// Design §3.5's INTERSECTION: "a capability is on only if both say ok.
    /// Local can lower, never raise."
    ///
    /// * `unsupported` on either side wins, and `self`'s reason is kept when
    ///   both are unsupported (the signed side is `self` at the call site).
    /// * `pending` on one side yields the other, unchanged — a side that
    ///   measured nothing narrows nothing.
    /// * otherwise the off-capability sets are UNIONED, which is the
    ///   intersection of the on-sets.
    #[must_use]
    pub fn narrow(&self, other: &Verdict) -> Verdict {
        match (self, other) {
            (Verdict::Unsupported(_), _) => self.clone(),
            (_, Verdict::Unsupported(_)) => other.clone(),
            (Verdict::Pending, v) | (v, Verdict::Pending) => v.clone(),
            (Verdict::Aligned, Verdict::Aligned) => Verdict::Aligned,
            (Verdict::Aligned, v) | (v, Verdict::Aligned) => v.clone(),
            (Verdict::Degraded(a), Verdict::Degraded(b)) => {
                let mut off = a.clone();
                for cap in b {
                    if !off.contains(cap) {
                        off.push(cap.clone());
                    }
                }
                Verdict::Degraded(off)
            }
        }
    }

    /// Whether this verdict lets any capability run at all.
    #[must_use]
    pub fn renders(&self) -> bool {
        matches!(self, Verdict::Aligned | Verdict::Degraded(_))
    }
}

/// WHAT CAN BE DONE WITH ONE CAPABILITY — the per-capability verdict, once.
///
/// Two callers, one judgment, and until 2026-09-22 two spellings of it: a
/// profile ([`super::profile::Profile::serves`]) answered `Serve` for whether
/// a PROGRAM can serve a capability at all, while [`CapVerdict`] answered an
/// `on: bool` beside a free-text `reason` for whether the PROBES turned it on
/// for this tuple. The words were already shared — `Serve`'s own doc said it
/// borrowed [`Verdict`]'s — so the type is shared now too, and `CapVerdict`
/// carries one rather than re-deriving it.
///
/// `R` is the reason string, and it is what lets one type serve both: a
/// profile's reasons are compiled in, so it takes the default `&'static str`
/// and stays `Copy`; a probe run builds its reasons, so it takes `String`.
///
/// The reason is not decoration: design §3.6's whole contract is that a
/// failure takes down exactly the capabilities that named it AND says which
/// thing did it. A verdict that answers `Unsupported` with no reason is a
/// capability that silently vanished, which is the outcome the owner's
/// central objection is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Serve<R = &'static str> {
    /// It can be served as designed.
    Ok,
    /// It runs, with less than the full signal. The reason names what is
    /// missing and what stands in for it.
    Degraded(R),
    /// It cannot be served at all. The reason names the thing that is not
    /// there.
    Unsupported(R),
}

impl<R: AsRef<str>> Serve<R> {
    /// The wire word — the vocabulary [`Verdict`] uses.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Serve::Ok => "ok",
            Serve::Degraded(_) => "degraded",
            Serve::Unsupported(_) => "unsupported",
        }
    }

    /// The reason, where there is one. [`Serve::Ok`] has none by
    /// construction: "it works" needs no excuse. A caller with something to
    /// say about a working capability says it from what it already knows —
    /// [`CapVerdict::reason`] renders `ok` from its own probe counts rather
    /// than storing a second copy of them as prose.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Serve::Ok => None,
            Serve::Degraded(why) | Serve::Unsupported(why) => Some(why.as_ref()),
        }
    }

    /// Whether the capability runs at all. `degraded` runs; `unsupported`
    /// does not.
    #[must_use]
    pub fn runs(&self) -> bool {
        !matches!(self, Serve::Unsupported(_))
    }

    /// The one-line row a `harness caps` listing prints:
    /// `usage-hud: unsupported — emacs is not a model client …`.
    #[must_use]
    pub fn row(&self, cap: &str) -> String {
        match self.reason() {
            Some(why) => format!("{cap}: {} — {why}", self.word()),
            None => format!("{cap}: {}", self.word()),
        }
    }
}

/// One capability's verdict, with the probe that decided it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapVerdict {
    /// The capability id.
    pub id: String,
    /// The verdict, in the ONE capability vocabulary: `ok`, or
    /// `unsupported` with the sentence a person reads. A probe run has no
    /// `degraded` per capability — a capability is on or it is off, and the
    /// DEGRADATION is the tuple-level [`Verdict::Degraded`] listing the ones
    /// that are off — but the words and the shape are the profile's, so the
    /// two surfaces cannot drift apart again.
    pub serve: Serve<String>,
    /// The probe id that turned it off, when one did.
    pub blamed: Option<String>,
    /// How many of its needs read `ok`.
    pub ok: usize,
    /// How many needs it has.
    pub total: usize,
}

impl CapVerdict {
    /// Whether it is on.
    #[must_use]
    pub fn on(&self) -> bool {
        self.serve.runs()
    }

    /// The one sentence a person reads: what failed and how, or — for a
    /// capability that is ON — what was measured.
    ///
    /// The `ok` sentence is RENDERED from [`CapVerdict::ok`] and
    /// [`CapVerdict::total`] rather than stored. It was a `String` field
    /// holding `format!("{ok}/{total} probes ok")` beside the two numbers it
    /// was formatted from; one copy of a fact is the version that cannot go
    /// stale.
    #[must_use]
    pub fn reason(&self) -> String {
        match self.serve.reason() {
            Some(why) => why.to_owned(),
            None if self.total == 0 => String::from("needs nothing"),
            None => format!("{}/{} probes ok", self.ok, self.total),
        }
    }
}

/// WHO ATTESTS a verdict (design §3.5's `(local)` note).
///
/// This was called `Source` until 2026-09-22 and was the fifth thing in the
/// harness with that name, which made it read as a fifth answer to "where
/// did this fact come from". It is not: [`super::source::Source`] names the
/// INPUT a fact was read from, and this names WHO SAYS SO — a signed row
/// published for this program build, a local probe run, both intersected, or
/// neither. Merging the two would have been a category error; the fix was
/// the name, and the printed key moved with it (`attested`, not `source`),
/// so `source` means exactly one thing across every harness surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attestation {
    /// A signed row and a local run agreed or were intersected.
    Both,
    /// A signed row only; nothing has been probed locally.
    Signed,
    /// A local run only — the lane has not published a row for this program
    /// build, or the owner pinned locally.
    Local,
    /// Neither: `probe pending`.
    None,
}

impl Attestation {
    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Attestation::Both => "both",
            Attestation::Signed => "signed",
            Attestation::Local => "local",
            Attestation::None => "none",
        }
    }
}

/// The whole alignment answer for one (harness, program, aterm) tuple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alignment {
    /// The adopted verdict.
    pub verdict: Verdict,
    /// Per capability, in the contract's declaration order.
    pub caps: Vec<CapVerdict>,
    /// Where it came from.
    pub attested: Attestation,
    /// Set when the signed and local verdicts disagreed, for the journal
    /// (design §3.5: "journaled as a disagreement").
    pub disagreement: Option<String>,
}

impl Alignment {
    /// How many capabilities are on, and how many there are.
    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        (self.caps.iter().filter(|c| c.on()).count(), self.caps.len())
    }

    /// The capability ids that are ON — exactly what `ATERM_HARNESS_CAPS`
    /// would carry (design §3.5).
    #[must_use]
    pub fn caps_on(&self) -> Vec<String> {
        self.caps
            .iter()
            .filter(|c| c.on())
            .map(|c| c.id.clone())
            .collect()
    }

    /// The WIRE word `atpkg::harness::row` renders a row from:
    /// `aligned(7/7)`, `degraded(5/7):usage-hud,model-failover`,
    /// `unsupported:<reason>`, `pending`.
    #[must_use]
    pub fn wire(&self) -> String {
        let (ok, total) = self.counts();
        match &self.verdict {
            Verdict::Aligned | Verdict::Degraded(_) => {
                let mut s = String::from(self.verdict.word());
                s.push('(');
                s.push_str(&ok.to_string());
                s.push('/');
                s.push_str(&total.to_string());
                s.push(')');
                if let Verdict::Degraded(caps) = &self.verdict {
                    s.push(':');
                    s.push_str(&caps.join(","));
                }
                s
            }
            other => other.as_wire(),
        }
    }

    /// The JSON form the `harness align --json` verb prints.
    #[must_use]
    pub fn to_json(&self, tuple: &Tuple) -> String {
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(1u64));
        o.insert("kind".to_owned(), Value::from("alignment".to_owned()));
        o.insert("abi".to_owned(), Value::from(u64::from(HARNESS_ABI)));
        o.insert("harness".to_owned(), Value::from(tuple.harness.clone()));
        o.insert(
            "harness_build".to_owned(),
            Value::from(tuple.harness_build.clone()),
        );
        o.insert("program".to_owned(), Value::from(tuple.program.clone()));
        o.insert(
            "program_build".to_owned(),
            Value::from(tuple.program_build.clone()),
        );
        o.insert("aterm".to_owned(), Value::from(tuple.aterm_build.clone()));
        o.insert("probe_set".to_owned(), Value::from(tuple.probe_set.clone()));
        o.insert("verdict".to_owned(), Value::from(self.wire()));
        o.insert(
            "verdict_word".to_owned(),
            Value::from(self.verdict.word().to_owned()),
        );
        o.insert(
            "attested".to_owned(),
            Value::from(self.attested.as_str().to_owned()),
        );
        let (ok, total) = self.counts();
        o.insert("caps_ok".to_owned(), Value::from(ok as u64));
        o.insert("caps_total".to_owned(), Value::from(total as u64));
        o.insert(
            "caps".to_owned(),
            Value::Array(
                self.caps
                    .iter()
                    .map(|c| {
                        let mut r = Map::new();
                        r.insert("id".to_owned(), Value::from(c.id.clone()));
                        r.insert("on".to_owned(), Value::from(c.on()));
                        r.insert(
                            "blamed".to_owned(),
                            c.blamed.clone().map_or(Value::Null, Value::from),
                        );
                        r.insert("reason".to_owned(), Value::from(c.reason()));
                        r.insert("probes_ok".to_owned(), Value::from(c.ok as u64));
                        r.insert("probes_total".to_owned(), Value::from(c.total as u64));
                        Value::Object(r)
                    })
                    .collect(),
            ),
        );
        o.insert(
            "disagreement".to_owned(),
            self.disagreement.clone().map_or(Value::Null, Value::from),
        );
        aterm_json::to_string(&Value::Object(o))
            .unwrap_or_else(|_| String::from("{\"schema\":1,\"kind\":\"alignment\"}"))
    }
}

/// Compute the LOCAL verdict: the per-capability degradation of design §3.6.
///
/// * The ONE ABI rule first. A miss renders NOTHING: every capability is off,
///   the verdict is `unsupported — needs aterm ABI <n>, running <m>`, and no
///   probe result can lift it.
/// * A capability is ON when every id in its `needs` has a result reading
///   `ok`. A need with NO result is off with the reason `pending`, because an
///   unmeasured need is not a measured one.
/// * `optional` needs are consulted only for the ids in `optional_on`, which
///   is the owner's `allow_*` set (design §1.2's `[[capability]] optional`).
/// * `required` — the contract's list, or the owner's override — turns a
///   degradation into `unsupported`, which is the only thing `hold-program`
///   acts on.
#[must_use]
pub fn evaluate(
    contract: &Contract,
    results: &[ProbeResult],
    required: &[String],
    optional_on: &[String],
) -> Alignment {
    if !abi_supported(contract.abi) {
        let reason = abi_reason(contract.abi);
        return Alignment {
            verdict: Verdict::Unsupported(reason.clone()),
            caps: contract
                .capabilities
                .iter()
                .map(|c| CapVerdict {
                    id: c.id.clone(),
                    serve: Serve::Unsupported(reason.clone()),
                    blamed: None,
                    ok: 0,
                    total: c.needs.len(),
                })
                .collect(),
            attested: Attestation::Local,
            disagreement: None,
        };
    }

    let by_id: BTreeMap<&str, &ProbeResult> = results.iter().map(|r| (r.id.as_str(), r)).collect();
    // A `[[tool]]` row gates the capabilities it NAMES — including a
    // `<cap>.<sub>` sub-id, which gates the sub-capability and leaves the rest
    // of the parent alone (design §3.6's `tool.node` ⇒ `usage-hud.sheet`).
    let mut tool_block: BTreeMap<&str, (String, String)> = BTreeMap::new();
    for row in &contract.tools {
        let id = row.probe_id();
        let Some(result) = by_id.get(id.as_str()) else {
            continue;
        };
        if result.status == ProbeStatus::Ok {
            continue;
        }
        for cap in &row.caps {
            // A sub-id gates only the sub-capability; the parent survives.
            if cap.contains('.') {
                continue;
            }
            tool_block
                .entry(cap.as_str())
                .or_insert_with(|| (id.clone(), result.detail.clone()));
        }
    }

    let mut caps = Vec::with_capacity(contract.capabilities.len());
    let mut off: Vec<String> = Vec::new();
    let mut measured = false;
    for cap in &contract.capabilities {
        let consulted: Vec<&String> = cap
            .needs
            .iter()
            .chain(cap.optional.iter().filter(|o| optional_on.contains(o)))
            .collect();
        let mut ok = 0usize;
        let mut blamed: Option<(String, String)> = None;
        for need in &consulted {
            match by_id.get(need.as_str()) {
                Some(result) if result.status == ProbeStatus::Ok => {
                    measured = true;
                    ok += 1;
                }
                Some(result) => {
                    measured = true;
                    if blamed.is_none() {
                        blamed = Some(((*need).clone(), probe_reason(result)));
                    }
                }
                None => {
                    if blamed.is_none() {
                        blamed = Some(((*need).clone(), String::from("pending: no probe ran")));
                    }
                }
            }
        }
        if blamed.is_none()
            && let Some((probe, detail)) = tool_block.get(cap.id.as_str())
        {
            blamed = Some((probe.clone(), detail.clone()));
        }
        let total = consulted.len();
        let on = blamed.is_none();
        if !on {
            off.push(cap.id.clone());
        }
        // `ok` carries NO stored reason: `CapVerdict::reason` renders it
        // from `ok`/`total`, which are right here in the row.
        let (blame_id, serve) = match blamed {
            Some((id, why)) => {
                let reason = format!("{id}: {why}");
                (
                    Some(id.clone()),
                    Serve::Unsupported(truncate_bytes(&reason, REASON_CAP).to_string()),
                )
            }
            None => (None, Serve::Ok),
        };
        debug_assert_eq!(on, serve.runs());
        caps.push(CapVerdict {
            id: cap.id.clone(),
            serve,
            blamed: blame_id,
            ok,
            total,
        });
    }

    let verdict = if !measured && results.is_empty() {
        Verdict::Pending
    } else if off.is_empty() {
        Verdict::Aligned
    } else {
        let held: Vec<String> = off
            .iter()
            .filter(|id| required.contains(id))
            .cloned()
            .collect();
        if held.is_empty() {
            Verdict::Degraded(off.clone())
        } else {
            let mut reason = String::from("required: ");
            reason.push_str(&held.join(", "));
            Verdict::Unsupported(reason)
        }
    };
    let caps = if matches!(verdict, Verdict::Pending) {
        caps.into_iter()
            .map(|mut c| {
                c.serve = Serve::Unsupported(String::from("probe pending"));
                c
            })
            .collect()
    } else {
        caps
    };
    Alignment {
        verdict,
        caps,
        attested: Attestation::Local,
        disagreement: None,
    }
}

/// `needs aterm ABI <n>, running <m>` — design §3.8's ABI row tail, with
/// `running` spelled as `<min>..<max>` when the two constants differ.
#[must_use]
pub fn abi_reason(abi: u32) -> String {
    let mut s = String::from("needs aterm ABI ");
    s.push_str(&abi.to_string());
    s.push_str(", running ");
    if HARNESS_ABI_MIN == HARNESS_ABI {
        s.push_str(&HARNESS_ABI.to_string());
    } else {
        s.push_str(&HARNESS_ABI_MIN.to_string());
        s.push_str("..");
        s.push_str(&HARNESS_ABI.to_string());
    }
    s
}

fn probe_reason(result: &ProbeResult) -> String {
    let mut s = String::from(result.status.as_str());
    if !result.detail.is_empty() {
        s.push_str(" — ");
        s.push_str(&result.detail);
    }
    s
}

/// Adopt the intersection of a SIGNED row and a LOCAL one (design §3.5).
///
/// `local` is the alignment this client computed; `signed` is the verdict the
/// lane published for this program build, when there is one. The result keeps
/// `local`'s per-capability detail — the signed row has none — and its verdict
/// is `signed.narrow(local)`, so local can lower and never raise. A
/// disagreement (signed `unsupported`, local fine) is RECORDED rather than
/// smoothed over.
#[must_use]
pub fn adopt(signed: Option<&Verdict>, local: Alignment) -> Alignment {
    let Some(signed) = signed else {
        return Alignment {
            attested: if matches!(local.verdict, Verdict::Pending) {
                Attestation::None
            } else {
                Attestation::Local
            },
            ..local
        };
    };
    let adopted = signed.narrow(&local.verdict);
    let disagreement = (adopted != local.verdict).then(|| {
        let mut s = String::from("signed ");
        s.push_str(&signed.as_wire());
        s.push_str(", local ");
        s.push_str(&local.verdict.as_wire());
        s
    });
    let off = match &adopted {
        Verdict::Degraded(off) => off.clone(),
        _ => Vec::new(),
    };
    let renders = adopted.renders();
    let caps = local
        .caps
        .into_iter()
        .map(|mut c| {
            if !renders {
                // The whole tuple renders nothing, so every capability is
                // off for the tuple's own reason; its per-capability
                // sentence, where it had one, is kept.
                if let Serve::Ok = c.serve {
                    c.serve = Serve::Unsupported(adopted.as_wire());
                }
            } else if off.contains(&c.id) && c.on() {
                c.serve = Serve::Unsupported(String::from("off in the signed alignment row"));
            }
            c
        })
        .collect();
    Alignment {
        verdict: adopted,
        caps,
        attested: if matches!(local.verdict, Verdict::Pending) {
            Attestation::Signed
        } else {
            Attestation::Both
        },
        disagreement,
    }
}

// ---------------------------------------------------------------------------
// The tuple, the sidecar and the refusal memo (design §1.3, §3.4, §3.5)
// ---------------------------------------------------------------------------

/// The key everything alignment is filed under: `(harness build, program
/// build, aterm build, probe_set)`, plus the two names that say what they are.
///
/// `aterm_build` is the identity token `aterm --version` prints after the
/// `aterm ` prefix (`0.86.0`), VERBATIM — design §3.1 defines it once and calls
/// it "a string compared for equality, never ordered".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tuple {
    /// The harness package name, e.g. `claude-harness`.
    pub harness: String,
    /// Its build.
    pub harness_build: String,
    /// The wrapped program.
    pub program: String,
    /// Its installed build.
    pub program_build: String,
    /// The aterm identity token.
    pub aterm_build: String,
    /// The harness's `probe_set` digest, or its short form.
    pub probe_set: String,
}

impl Tuple {
    /// The sidecar line's KEY: `claude@2026091701/0.86.0#a1b2c3d4`.
    ///
    /// The digest is carried SHORT, exactly as design §3.4's signed entry
    /// carries it, so the two sides of the intersection are keyed the same
    /// way. A `probe_set` that is not a `sha256:` digest is carried verbatim
    /// (a dev-linked tree may declare none, and `-` is then the key).
    #[must_use]
    pub fn key(&self) -> String {
        let short = probe_set_short(&self.probe_set);
        let mut s = String::new();
        s.push_str(&self.program);
        s.push('@');
        s.push_str(&self.program_build);
        s.push('/');
        s.push_str(&self.aterm_build);
        s.push('#');
        s.push_str(if short.is_empty() {
            if self.probe_set.is_empty() {
                "-"
            } else {
                self.probe_set.as_str()
            }
        } else {
            short.as_str()
        });
        s
    }

    /// A filesystem-safe rendering of [`Tuple::key`] for the refusal memo's
    /// name (design §1.3's `refused/<tuple>`). Every byte outside
    /// `[A-Za-z0-9._-]` becomes `_`, so nothing in a tuple can traverse.
    #[must_use]
    pub fn slug(&self) -> String {
        let mut s = String::with_capacity(48);
        s.push_str(&self.harness_build);
        s.push('-');
        s.push_str(&self.key());
        sanitize_component(&s)
    }
}

/// One row of the LOCAL alignment sidecar,
/// `<state>/alignment/<harness build>.toml` (design §1.3, §3.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRow {
    /// The tuple key ([`Tuple::key`]).
    pub key: String,
    /// The verdict, in [`Alignment::wire`]'s counted spelling.
    pub wire: String,
    /// When it was written, Unix seconds.
    pub at: i64,
}

impl LocalRow {
    /// The parsed verdict, or `None` for a word this client does not know.
    #[must_use]
    pub fn verdict(&self) -> Option<Verdict> {
        Verdict::parse_wire(&self.wire)
    }
}

/// `<state>/alignment/<harness build>.toml`.
#[must_use]
pub fn sidecar_path(state: &Path, harness_build: &str) -> PathBuf {
    let mut name = sanitize_component(harness_build);
    name.push_str(".toml");
    state.join("alignment").join(name)
}

/// `<state>/refused/<tuple slug>` — the DURABLE memo a pre-flip probe failure
/// leaves behind, because the refused candidate tree is reclaimed by gc and
/// cannot carry it (design §1.3, §3.1).
#[must_use]
pub fn refused_path(state: &Path, tuple: &Tuple) -> PathBuf {
    state.join("refused").join(tuple.slug())
}

/// One sidecar line: `<key> = "<wire>" # at=<unix seconds>`.
///
/// Deliberately the same grammar [`read_sidecar`] reads and nothing richer: the
/// file is appended to by a lock-free hidden verb (design §1.4), so a torn
/// final line must cost one row and never the file.
#[must_use]
pub fn sidecar_line(tuple: &Tuple, wire: &str, at: i64) -> String {
    let mut s = String::new();
    s.push_str(&tuple.key());
    s.push_str(" = \"");
    s.push_str(&sanitize_wire(wire));
    s.push_str("\" # at=");
    s.push_str(&at.to_string());
    s.push('\n');
    s
}

/// Read a sidecar, skipping every line that is not the grammar above.
///
/// FAIL-OPEN PER ROW, FAIL-CLOSED PER VERDICT: an unreadable row is dropped (a
/// torn append costs one row), and a row whose word this client does not know
/// is KEPT with its wire text, so [`LocalRow::verdict`] answers `None` and the
/// caller reads `probe pending` rather than inventing a verdict.
#[must_use]
pub fn read_sidecar(text: &str) -> Vec<LocalRow> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, rest) = match line.split_once('=') {
            Some(parts) => parts,
            None => continue,
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let rest = rest.trim();
        let Some(body) = rest.strip_prefix('"') else {
            continue;
        };
        let Some(end) = body.find('"') else {
            continue;
        };
        let wire = &body[..end];
        let at = body[end..]
            .split_once("at=")
            .and_then(|(_, a)| a.trim().parse::<i64>().ok())
            .unwrap_or(0);
        out.push(LocalRow {
            key: key.to_string(),
            wire: wire.to_string(),
            at,
        });
    }
    out
}

/// The NEWEST row for `tuple` — the file is append-only, so the last wins.
#[must_use]
pub fn lookup<'a>(rows: &'a [LocalRow], tuple: &Tuple) -> Option<&'a LocalRow> {
    let key = tuple.key();
    rows.iter().rev().find(|r| r.key == key)
}

/// One entry of design §3.4's SIGNED `alignment` key:
/// `<program>@<build>=<verdict>#<probe_set[:8]>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedRow {
    /// The program the entry is about.
    pub program: String,
    /// Its build.
    pub build: String,
    /// The verdict, with no counts (the signed carrier has none).
    pub verdict: Verdict,
    /// The first eight hex characters of the harness's `probe_set`.
    pub probe_set: String,
}

impl SignedRow {
    /// Parse one entry, or say what is wrong with it. The client is the
    /// validator here (design §3.4: "parse-validated by the client"), so a
    /// malformed entry is a refusal and never a partially-read row.
    ///
    /// # Errors
    /// The entry is not `<program>@<build>=<verdict>#<digest8>`, or carries a
    /// verdict word this ABI does not know.
    pub fn parse(entry: &str) -> Result<Self, String> {
        let bad = |why: &str| format!("alignment entry {:?}: {why}", truncate_bytes(entry, 120));
        let (head, tail) = entry.split_once('=').ok_or_else(|| bad("no `=`"))?;
        let (program, build) = head.split_once('@').ok_or_else(|| bad("no `@`"))?;
        let (verdict, digest) = tail.rsplit_once('#').ok_or_else(|| bad("no `#`"))?;
        if !is_program_name(program) {
            return Err(bad("the program name is not a program name"));
        }
        if build.is_empty()
            || !build
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.')
        {
            return Err(bad("the build is not alphanumeric"));
        }
        if digest.len() != 8 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad("the probe_set is not eight hex characters"));
        }
        let verdict = Verdict::parse_wire(verdict).ok_or_else(|| bad("an unknown verdict word"))?;
        Ok(SignedRow {
            program: program.to_string(),
            build: build.to_string(),
            verdict,
            probe_set: digest.to_string(),
        })
    }

    /// The entry, rendered back exactly as design §3.4 spells it.
    #[must_use]
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(&self.program);
        s.push('@');
        s.push_str(&self.build);
        s.push('=');
        s.push_str(&self.verdict.as_wire());
        s.push('#');
        s.push_str(&self.probe_set);
        s
    }

    /// Whether this row is about `tuple`'s program build AND was produced by
    /// the same probe set. A digest mismatch means the lane measured a
    /// DIFFERENT set of probes, so the row is not about this harness tree.
    #[must_use]
    pub fn matches(&self, tuple: &Tuple) -> bool {
        self.program == tuple.program
            && self.build == tuple.program_build
            && self.probe_set == probe_set_short(&tuple.probe_set)
    }
}

/// Pick the signed row for `tuple` out of a signed `alignment` list, refusing
/// the whole list if any entry is malformed (design §3.4's `Reject::Alignment`).
///
/// # Errors
/// Any entry fails [`SignedRow::parse`].
pub fn signed_for(entries: &[&str], tuple: &Tuple) -> Result<Option<SignedRow>, String> {
    let mut found = None;
    for entry in entries {
        let row = SignedRow::parse(entry)?;
        if row.matches(tuple) {
            found = Some(row);
        }
    }
    Ok(found)
}

/// One path component with every separator and every `..` neutralised.
///
/// A single `.` is kept (a build may be `0.86.0`); a RUN of them is not, so
/// nothing a tuple carries can climb out of the directory it is filed in.
fn sanitize_component(s: &str) -> String {
    let mut cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    while cleaned.contains("..") {
        cleaned = cleaned.replace("..", "__");
    }
    if let Some(rest) = cleaned.strip_prefix('.') {
        let mut safe = String::from("_");
        safe.push_str(rest);
        cleaned = safe;
    }
    if cleaned.is_empty() {
        String::from("_")
    } else {
        cleaned
    }
}

/// A wire word with everything that would break the sidecar's one-line
/// grammar removed: the quote that frames the value, and every character
/// [`crate::supervise::limit::breaks_a_line`] names. That predicate is shared
/// rather than spelled here because the local test — `!c.is_control()` — is
/// Cc-only, so `U+2028` LINE SEPARATOR survived it into a row whose whole
/// grammar is one line. `\n` is covered by it and is no longer named twice.
fn sanitize_wire(wire: &str) -> String {
    truncate_bytes(wire, REASON_CAP)
        .chars()
        .filter(|c| *c != '"' && !crate::supervise::limit::breaks_a_line(*c))
        .collect()
}

#[path = "align_tests.rs"]
#[cfg(test)]
mod tests;
