// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PER-PROGRAM PROFILES (design `docs/DESIGN-aterm-wrapper-2026-09-17.md`
//! §6.1-§6.3): the unit of adaptation, and the proof that the harness is not
//! a Claude Code script.
//!
//! # What a profile is
//!
//! A [`Profile`] names one program family and everything that differs about
//! it: the preference-ordered [`Profile::wraps_any`] the prelude picks a
//! target from, the `[shim]` env and args it would inject, the probe ids its
//! capabilities stand on, the delivery [`Policy`], and — the part that makes
//! this module worth its bytes — [`Profile::serves`], which answers for every
//! capability whether this program can serve it, and when it cannot, WHY, in
//! one sentence that is printed rather than swallowed.
//!
//! Three profiles ship: [`CLAUDE`], [`CODEX`] and [`EMACS`]. They differ as
//! far as three programs can: full hooks, hooks-behind-a-trust-gate, and no
//! hooks at all.
//!
//! # The claim this module discharges, and how it is checked
//!
//! The spine of stage 1 ([`super::observe`]) reads aterm's own view of a
//! session — `status`, the parsed grid — and NO vendor surface. If that is
//! true, the same spine classifies a codex session and an emacs session with
//! nothing added. It does, and the tests prove it from screens and `status`
//! lines CAPTURED FROM REAL SESSIONS of each program in a headless aterm
//! (MEASURED 2026-09-21; the fixtures and how they were taken are in
//! `profile_tests.rs`). What the profile adds on top is not the spine — it is
//! the frame test that names the program when `detail=` is empty, and the
//! per-capability answer above.
//!
//! # Nothing here is Claude-shaped
//!
//! * **codex** has the same hook vocabulary (`hooks.json`, `PreToolUse`,
//!   `CLAUDE_PLUGIN_ROOT` — all MEASURED as literals in the store binary) but
//!   puts it behind a `trusted_hash` gate the user answers once, so hooks are
//!   `degraded` until then and the profile leans on the TRUST-FREE channels
//!   instead: the `notify` command, OSC 9, and the rollout files.
//! * **emacs** has no hook vocabulary at all, which is the point. Its profile
//!   is an elisp file on `EMACSLOADPATH` that reports through `aterm ctl`.
//! * **Neither emits OSC 133** — and neither does claude in the case that
//!   matters (an ADOPTED session owns no shell-integration block). So the
//!   spine's turn evidence is `status phase`/`revision` plus the shipped grid
//!   readers for all three ([`TurnEvidence::Phase`]), and a block-based wait
//!   is a capability none of them can serve. That is stated per profile in
//!   [`Channels::osc133`] rather than assumed anywhere.
//!
//! # What this module deliberately does NOT do
//!
//! It does not launch anything, open a socket, run a probe or touch the
//! filesystem except through [`materialize`], which a packer calls with an
//! explicit root. It does not build the codex fork lane (`acx`): §6.3's
//! decision rule is here as the pure function [`needs_fork`], and the lane is
//! named and left unbuilt. It does not make the emacs profile REACHABLE —
//! that needs the owner's D2 ruling (see [`EMACS_REACH`]).
//!
//! STATUS (docs/README.md honesty ratchet): unit-tested. The emacs term-file
//! chain is MEASURED by a test that runs `emacs --batch` against a
//! materialized tree where one is installed, and says so out loud where it is
//! not. The codex config layer is rendered and shape-checked here; that codex
//! HONOURS it is UNVERIFIED — no codex run has consumed one.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::align::{Contract, PatchPolicy, Policy, SUBST_ROOT, SUBST_STATE, Serve};
use crate::supervise::phase::has_composer_frame;

// ---------------------------------------------------------------------------
// Whether a program can serve a capability, and why not
// ---------------------------------------------------------------------------

// `Serve` used to be declared here, with its own `Ok`/`Degraded`/`Unsupported`
// and its own `word()` — while `align::CapVerdict` spelled the SAME
// per-capability verdict as an `on: bool` beside a free-text `reason`. Two
// spellings of one judgment, and `Serve`'s own doc already admitted it
// borrowed `align::Verdict`'s words. It now lives beside those words, in
// `align`, generic over its reason string so a profile can keep `&'static
// str` (and stay `Copy`) while a probe run keeps a `String`.

// ---------------------------------------------------------------------------
// The signals a program emits, measured rather than assumed
// ---------------------------------------------------------------------------

/// How the spine learns a turn ended on this program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEvidence {
    /// Shell-integration blocks (OSC 133 `A`/`B`/`C`/`D`) bound the turn, so
    /// `await block` is exact. **No shipped profile uses this**, and that is
    /// the measurement, not an oversight.
    Blocks,
    /// aterm's own classification — `status phase=`, `revision=` and the
    /// shipped grid readers. Lossy, universal, always available; the tier-0
    /// of RFC-agent-cli-hosting §"structured signals are enrichment".
    Phase,
}

impl TurnEvidence {
    /// The wire word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TurnEvidence::Blocks => "blocks",
            TurnEvidence::Phase => "phase",
        }
    }
}

/// What a hook channel is worth on this program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hooks {
    /// No hook vocabulary exists (emacs).
    None,
    /// The vocabulary exists and fires once installed (claude).
    Native,
    /// The vocabulary exists behind a trust gate the USER answers once; until
    /// then the channel reads `degraded` and the harness never bypasses it.
    TrustGated,
}

impl Hooks {
    /// The wire word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Hooks::None => "none",
            Hooks::Native => "native",
            Hooks::TrustGated => "trust-gated",
        }
    }
}

/// The enrichment channels this program offers, each field a MEASURED fact
/// with its measurement named in the profile's own comment. Every one of them
/// is allowed to be false: the spine does not read this struct, the
/// CAPABILITIES do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Channels {
    /// The program emits OSC 133 shell-integration marks of its own.
    pub osc133: bool,
    /// The program emits OSC 9 notifications aterm already parses.
    pub osc9: bool,
    /// The program can be told to run a command on turn completion.
    pub notify_cmd: bool,
    /// What its hook vocabulary is worth.
    pub hooks: Hooks,
    /// The program writes a machine-readable session log the harness may read
    /// (claude's transcript, codex's rollout jsonl).
    pub session_log: bool,
    /// How the spine bounds a turn here.
    pub turns: TurnEvidence,
}

// ---------------------------------------------------------------------------
// Capability rows
// ---------------------------------------------------------------------------

/// One capability as THIS program can serve it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapRow {
    /// The capability id, shared across profiles.
    pub id: &'static str,
    /// The probe ids it stands on, in design §3.6's `<kind>.<name>` spelling.
    /// Empty means it stands on aterm alone.
    pub needs: &'static [&'static str],
    /// The highest actuator level it may reach (design §4.3).
    pub max_level: u8,
    /// The declared token bucket, or empty.
    pub budget: &'static str,
    /// The answer on the VENDOR program.
    pub serve: Serve,
    /// The answer on a FORKED target, where it differs (design §6.3: on
    /// `acx`, `exact-blocks` and `hooks-trusted` become `ok`). `None` means
    /// the fork changes nothing, which is the honest default.
    pub on_fork: Option<Serve>,
}

// ---------------------------------------------------------------------------
// Naming the program from aterm's own view
// ---------------------------------------------------------------------------

/// How a session is recognised as running this program.
#[derive(Debug, Clone, Copy)]
pub struct Identity {
    /// The `status detail=` words this profile answers to. `detail=` is the
    /// spine's own field, so this is the rank-1 test.
    pub detail_any: &'static [&'static str],
    /// A frame test over the tail of the grid, for the ADOPTED case where
    /// `detail=` is `-` because the session owns no shell-integration block.
    /// `None` means the grid cannot name this program and only `detail=` can
    /// — which is the honest answer for codex today.
    pub frame: Option<fn(&[String]) -> bool>,
}

/// What named the program, and how it was named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named {
    /// `status detail=` said so. Rank 1: aterm's own field.
    Detail,
    /// A frame test over the grid said so. Heuristic by construction.
    Frame,
}

impl Named {
    /// The wire word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Named::Detail => "detail",
            Named::Frame => "frame",
        }
    }
}

/// Whether the tail of the grid shows an Emacs mode line.
///
/// MEASURED 2026-09-21 against a real `emacs -nw -Q` in a headless aterm
/// (110×32, `TERM=xterm-256color`, which
/// `crates/aterm-gui/src/lib.rs:30700` forces for every session shell): the
/// last row reads
/// `-UUU:**-  F1  *scratch*   All   L5   (Lisp Interaction ElDoc) -----…`.
///
/// The rule is deliberately narrow — an ASCII `-`-led row whose first eight
/// bytes carry the mode-line's `:` separator, ending in a run of ASCII `-`
/// filler at least [`MODELINE_FILL`] long. It cannot collide with Claude
/// Code's composer rules, which are U+2500 `─`, nor with a Markdown `---`
/// rule, which has no `:`. Ties break toward "not emacs".
#[must_use]
pub fn has_emacs_modeline(rows: &[String]) -> bool {
    rows.iter().rev().take(4).any(|row| {
        let t = row.trim_end();
        if !t.starts_with('-') || !t.is_ascii() || t.len() < 40 {
            return false;
        }
        let head = &t[..8.min(t.len())];
        if !head.contains(':') {
            return false;
        }
        let fill = t.bytes().rev().take_while(|b| *b == b'-').count();
        fill >= MODELINE_FILL
    })
}

/// How many trailing `-` a row must carry to read as an Emacs mode line. Four
/// is the shortest run the mode line draws on a narrow frame, and it is long
/// enough that a sentence ending in a dash cannot reach it.
pub const MODELINE_FILL: usize = 4;

// ---------------------------------------------------------------------------
// The profile
// ---------------------------------------------------------------------------

/// Everything that differs about one wrapped program family.
#[derive(Debug, Clone, Copy)]
pub struct Profile {
    /// The profile's own id — the word `harness status` prints as
    /// `profile=<id>`.
    pub id: &'static str,
    /// The preference-ordered program family (design §1.2's `wraps_any`).
    /// The FIRST name that is installed and usable wins, which is how the
    /// fork lane is selected without a second mechanism.
    pub wraps_any: &'static [&'static str],
    /// The names in [`Profile::wraps_any`] that are compile-our-own rows.
    pub forks: &'static [&'static str],
    /// The delivery policy (design §1.2, §7).
    pub policy: Policy,
    /// The patch-lane column.
    pub patch: PatchPolicy,
    /// `[shim] env`, UNSUBSTITUTED, in contract order.
    pub env: &'static [&'static str],
    /// `[shim] args`, UNSUBSTITUTED, in contract order.
    pub args: &'static [&'static str],
    /// The actuator vocabulary this profile may reach, ⊆
    /// [`super::align::ACTUATORS`].
    pub actuators: &'static [&'static str],
    /// Declared outbound hostnames.
    pub egress: &'static [&'static str],
    /// The `[[tool]]` rows, as `(bin, min, caps)`.
    pub tools: &'static [(&'static str, &'static str, &'static [&'static str])],
    /// Every capability, servable or not. An `Unsupported` row is NOT
    /// omitted: it is the row that carries the reason.
    pub caps: &'static [CapRow],
    /// The capabilities whose failure means `unsupported`, not `degraded`.
    pub required: &'static [&'static str],
    /// The MEASURED signal channels.
    pub channels: Channels,
    /// How a session is recognised as running this program.
    pub identity: Identity,
    /// The files this profile contributes to the harness tree.
    pub files: &'static [TreeFile],
    /// What is out of reach on this box without an owner ruling, or empty.
    pub reach_note: &'static str,
}

impl Profile {
    /// Whether this profile wraps `program`.
    #[must_use]
    pub fn wraps(&self, program: &str) -> bool {
        self.wraps_any.contains(&program)
    }

    /// Whether `program` is one of this profile's compile-our-own rows.
    #[must_use]
    pub fn is_fork(&self, program: &str) -> bool {
        self.forks.contains(&program)
    }

    /// The capability row with this id.
    #[must_use]
    pub fn cap(&self, id: &str) -> Option<&'static CapRow> {
        self.caps.iter().find(|c| c.id == id)
    }

    /// **The question this module exists to answer**: can `program`, wrapped
    /// by this profile, serve capability `id` — and if not, why not?
    ///
    /// A capability this profile has never heard of reads `unsupported` with
    /// the profile named, rather than `ok` by omission: the tie breaks toward
    /// the safe answer.
    #[must_use]
    pub fn serves(&self, id: &str, program: &str) -> Serve {
        match self.cap(id) {
            Some(row) => match (self.is_fork(program), row.on_fork) {
                (true, Some(forked)) => forked,
                _ => row.serve,
            },
            None => Serve::Unsupported(UNDECLARED),
        }
    }

    /// Every capability this profile can actually run on `program`, in
    /// contract order — what a rendered `harness.toml` declares.
    #[must_use]
    pub fn runnable(&self, program: &str) -> Vec<&'static str> {
        self.caps
            .iter()
            .filter(|c| self.serves(c.id, program).runs())
            .map(|c| c.id)
            .collect()
    }

    /// The rows a `harness caps` listing prints for `program`, one per
    /// capability including the ones it cannot serve.
    #[must_use]
    pub fn cap_rows(&self, program: &str) -> Vec<String> {
        self.caps
            .iter()
            .map(|c| self.serves(c.id, program).row(c.id))
            .collect()
    }

    /// Render the `harness.toml` this profile needs against `program`, with
    /// the `probe_set` digest INJECTED (it is a fact about the packed
    /// `probes/` directory, not about the profile).
    ///
    /// The output is admitted by [`Contract::admit`] — the tests check every
    /// profile against it, so the grammar and the profiles cannot drift.
    /// Capabilities the program cannot serve are left out of the contract
    /// entirely; [`Profile::serves`] is where their reason lives.
    #[must_use]
    pub fn contract_toml(&self, program: &str, probe_set: &str) -> String {
        let runnable = self.runnable(program);
        let mut s = String::with_capacity(1024);
        let _ = writeln!(s, "# generated from the {} profile (design §6)", self.id);
        let _ = writeln!(s, "abi = {}", super::align::HARNESS_ABI);
        let _ = writeln!(s, "wraps_any = {}", toml_array(self.wraps_any));
        let _ = writeln!(s, "policy = \"{}\"", self.policy.as_str());
        let _ = writeln!(s, "patch = \"{}\"", self.patch.as_str());
        let _ = writeln!(s, "capabilities = {}", toml_array(&runnable));
        let _ = writeln!(s, "required = {}", toml_array(self.required));
        let _ = writeln!(s, "actuators = {}", toml_array(self.actuators));
        let _ = writeln!(s, "egress = {}", toml_array(self.egress));
        if !probe_set.is_empty() {
            let _ = writeln!(s, "probe_set = \"{probe_set}\"");
        }
        s.push_str("\n[shim]\n");
        let _ = writeln!(s, "env = {}", toml_array(self.env));
        let _ = writeln!(s, "args = {}", toml_array(self.args));
        for (bin, min, caps) in self.tools {
            let _ = write!(
                s,
                "\n[[tool]]\nbin = \"{bin}\"\nprobe = \"version\"\nmin = \"{min}\"\ncaps = {}\n",
                toml_array(caps)
            );
        }
        for row in self.caps.iter().filter(|c| runnable.contains(&c.id)) {
            let _ = write!(
                s,
                "\n[[capability]]\nid = \"{}\"\nneeds = {}\nmax_level = {}\n",
                row.id,
                toml_array(row.needs),
                row.max_level
            );
            if !row.budget.is_empty() {
                let _ = writeln!(s, "budget = \"{}\"", row.budget);
            }
        }
        s
    }

    /// The rendered contract, ADMITTED. Sugar over
    /// [`Profile::contract_toml`] for a caller that wants the parsed shape.
    ///
    /// # Errors
    /// Whatever [`Contract::admit`] refuses. A profile that cannot render an
    /// admissible contract is a bug in the profile, and the tests treat it as
    /// one.
    pub fn contract(&self, program: &str, probe_set: &str) -> Result<Contract, String> {
        Contract::admit(&self.contract_toml(program, probe_set))
    }
}

/// The reason an unknown capability id gets. Stated once so the tests can
/// pin it.
pub const UNDECLARED: &str = "this profile declares no such capability";

fn toml_array<S: AsRef<str>>(items: &[S]) -> String {
    let mut s = String::from("[");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push('"');
        s.push_str(item.as_ref());
        s.push('"');
    }
    s.push(']');
    s
}

// ---------------------------------------------------------------------------
// Selection: which target the prelude points at
// ---------------------------------------------------------------------------

/// Which program of a family the prelude will wrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// The chosen program name.
    pub program: &'static str,
    /// Whether it is a compile-our-own row (design §6.3's `acx`).
    pub forked: bool,
    /// Its position in `wraps_any`, so a caller can say "the preferred one"
    /// without re-deriving it.
    pub rank: usize,
}

/// Walk `wraps_any` in PREFERENCE ORDER and take the first name `usable`
/// accepts.
///
/// `usable` is the caller's whole world: design §6.3 says the fork is chosen
/// "when `acx` is installed **and has an `aligned` row**", which is two facts
/// the profile does not hold. Passing it in keeps this function pure and lets
/// the test drive both halves.
///
/// `None` means nothing in the family is usable — the honest answer, and the
/// one that leaves the program running plain.
#[must_use]
pub fn select(profile: &Profile, usable: &mut dyn FnMut(&str) -> bool) -> Option<Selection> {
    profile
        .wraps_any
        .iter()
        .enumerate()
        .find(|(_, name)| usable(name))
        .map(|(rank, name)| Selection {
            program: name,
            forked: profile.is_fork(name),
            rank,
        })
}

/// The profile whose id or `wraps_any` names `program`, searched in table
/// order.
#[must_use]
pub fn profile_for(program: &str) -> Option<&'static Profile> {
    PROFILES
        .iter()
        .find(|p| p.id == program || p.wraps(program))
        .copied()
}

/// Name the program a session is running, from aterm's own view ALONE:
/// `status detail=` first, then each profile's frame test over the tail of
/// the grid.
///
/// This is design §0.2 rank 1 and nothing else — no hook, no transcript, no
/// vendor file is consulted, which is why it answers for all three programs.
/// `None` means nothing named it, which is a real answer and not an error.
#[must_use]
pub fn identify(detail: Option<&str>, rows: &[String]) -> Option<(&'static Profile, Named)> {
    if let Some(detail) = detail {
        let name = program_word(detail);
        if let Some(p) = PROFILES
            .iter()
            .find(|p| p.identity.detail_any.contains(&name))
        {
            return Some((*p, Named::Detail));
        }
    }
    PROFILES
        .iter()
        .find(|p| p.identity.frame.is_some_and(|f| f(rows)))
        .map(|p| (*p, Named::Frame))
}

/// The program word inside a `status detail=` value.
///
/// MEASURED 2026-09-21: a real session gives `detail=emacs` and
/// `detail=codex` — the sanitized command HEAD, not a path and not the whole
/// line. This takes the basename of the first whitespace-separated token so
/// an absolute path still names its program, and nothing else: a `detail=`
/// that carries arguments is not re-parsed here, because guessing at a
/// command line is how a classifier earns a wrong answer.
fn program_word(detail: &str) -> &str {
    let head = detail.split_whitespace().next().unwrap_or(detail);
    head.rsplit('/').next().unwrap_or(head)
}

// ---------------------------------------------------------------------------
// The fork rule (design §6.3) — NAMED, not built
// ---------------------------------------------------------------------------

/// What a capability asks of the program it runs against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Asks {
    /// It needs a signal the vendor build does not emit at all (design
    /// §6.3's example: nonced OSC 133 — `133;A` MEASURED 0 in codex).
    pub unemitted_signal: bool,
    /// It needs a hook the user would otherwise have to trust by hand
    /// (codex's `trusted_hash`).
    pub hand_trusted_hook: bool,
    /// It changes the program's own decision channel.
    pub decision_channel: bool,
}

/// **The one rule that decides when the fork lane is needed** (design §6.3,
/// stated once): a capability moves into the compile-our-own build when it
/// needs a signal the vendor cannot emit, a hook the user would have to trust
/// by hand, or a change to the vendor's own decision channel. Everything
/// reachable through `-c` / `--profile` / `rules/` stays in the harness.
///
/// `None` is "stays in the harness" — and it is the answer for every
/// capability the shipped profiles declare, which is why the lane is named
/// and not built.
#[must_use]
pub fn needs_fork(asks: &Asks) -> Option<&'static str> {
    if asks.unemitted_signal {
        return Some("needs a signal the vendor build does not emit");
    }
    if asks.hand_trusted_hook {
        return Some("needs a hook the user would otherwise trust by hand");
    }
    if asks.decision_channel {
        return Some("changes the program's own decision channel");
    }
    None
}

/// The fork lane's name, so nothing spells it twice. It is its OWN program
/// row packed from source — never the `codex` row, whose vendor bytes and
/// Developer ID are pinned (design §6.3). **Not built.**
pub const FORK_ROW: &str = "acx";

/// What the emacs profile cannot reach on this box without an owner ruling
/// (design §6.2, §9.2 D2), stated so a caller can PRINT it instead of
/// pretending.
pub const EMACS_REACH: &str = "emacs is a pending system= row with no twin and \
     AGENT_PROGRAMS is fixed at claude, codex: reaching a Homebrew emacs needs \
     the owner's D2 ruling (a harnessed name set or a data-driven reroute row); \
     without it this profile reaches only a managed emacs";

// ---------------------------------------------------------------------------
// Tree files
// ---------------------------------------------------------------------------

/// One file a profile contributes to the harness tree (design §1.3's store
/// layout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeFile {
    /// The path relative to the tree root. Always relative, always
    /// forward-slashed, never `..`.
    pub path: &'static str,
    /// The bytes.
    pub body: &'static str,
    /// Whether it is laid 0755. `install.rs` sanitizes modes to 0755/0644, so
    /// this mirrors that two-valued choice and nothing wider.
    pub exec: bool,
}

/// Write `profile`'s files under `root`, creating directories as needed, and
/// return what was written in tree order.
///
/// This is the packer's half of §1.3 and the only function in this module
/// that touches the filesystem. Every path is re-checked here rather than
/// trusted from the table: a `TreeFile` is data, and data is checked at the
/// boundary even when this crate wrote it.
///
/// # Errors
/// A path that is absolute, carries a `..` component or a backslash, or any
/// I/O failure, naming the path.
pub fn materialize(profile: &Profile, root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut written = Vec::with_capacity(profile.files.len());
    for file in profile.files {
        let rel = safe_rel(file.path)?;
        let full = root.join(&rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("{}: create {}: {e}", file.path, parent.display()))?;
        }
        std::fs::write(&full, file.body).map_err(|e| format!("{}: write: {e}", file.path))?;
        set_mode(&full, file.exec).map_err(|e| format!("{}: mode: {e}", file.path))?;
        written.push(full);
    }
    Ok(written)
}

fn safe_rel(path: &str) -> Result<PathBuf, String> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.split('/').any(|c| c == ".." || c.is_empty())
    {
        return Err(format!("tree path {path:?} is not a safe relative path"));
    }
    Ok(PathBuf::from(path))
}

#[cfg(unix)]
fn set_mode(path: &Path, exec: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        path,
        std::fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 }),
    )
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _exec: bool) -> std::io::Result<()> {
    // The harness is Unix-only until a `.cmd` twin dialect exists (design
    // §1.4), so there is no mode to set here.
    Ok(())
}

// ---------------------------------------------------------------------------
// The emacs tree (design §6.2)
// ---------------------------------------------------------------------------

/// The term file aterm's emacs loads.
///
/// MEASURED 2026-09-21 on GNU Emacs 30.2 (Homebrew,
/// `/opt/homebrew/Cellar/emacs/30.2_2`), reading `faces.el`'s own
/// `tty-run-terminal-initialization` (:2372-2411) and then running it:
///
/// 1. `EMACSLOADPATH=<dir>:` puts `<dir>` FIRST and keeps the default path
///    (load-path length 28, ending in `.../lisp/obsolete`).
/// 2. `tty-run-terminal-initialization` LOADS the first `term/<type>` file it
///    finds, walking `xterm-256color` → `xterm`. Because our file is found
///    first, stock `term/xterm.el` is **not** loaded by that step — so this
///    file must `require` it, and does.
/// 3. Only AFTER that load does it look for `terminal-init-<type>`, walking
///    the same list. This file deliberately does **not** define
///    `terminal-init-xterm-256color`, so `terminal-init-xterm` is what runs
///    (measured: `terminal-initted` = `terminal-init-xterm`) — the stock
///    behaviour, with our capability list already in place.
/// 4. `xterm-extra-capabilities` defaults to `check`, which negotiates
///    through DA2; aterm answers version 100, below xterm.el's ≥203/216/242
///    gates, so `check` wins nothing. Setting the list explicitly is what
///    makes the capabilities arrive — and `getSelection` is deliberately NOT
///    in it.
/// 5. `startup.el:1583` runs `tty-run-terminal-initialization` before
///    `command-line-1` at :1597, so this file runs before any `-l` file and
///    for every tty frame.
///
/// A term file is `load`ed, not `require`d, so it need not `provide` — but it
/// provides anyway, so a second `require` is a no-op rather than a re-run.
pub const EMACS_TERM_FILE: &str = r#";;; xterm-256color.el --- aterm's terminal init  -*- lexical-binding: t; -*-
;; SPDX-License-Identifier: Apache-2.0
;; Copyright 2026 Andrew Yates

;;; Commentary:

;; Loaded by `tty-run-terminal-initialization' because this directory is
;; first on EMACSLOADPATH.  Finding this file STOPS the search, so stock
;; term/xterm is required here explicitly.  No `terminal-init-xterm-256color'
;; is defined on purpose: emacs then falls through to `terminal-init-xterm',
;; which is the stock behaviour, with the capability list already set.
;;
;; aterm answers DA2 with version 100, below xterm.el's 203/216/242 feature
;; gates, so the default `check' negotiates nothing.  Naming the capabilities
;; is what turns them on.  `getSelection' is deliberately absent: it would let
;; a buffer read the host clipboard.

;;; Code:

(require 'term/xterm)

(setq xterm-extra-capabilities '(modifyOtherKeys setSelection reportBackground))
(setq xterm-set-window-title t)

(require 'aterm-harness)

(provide 'term/xterm-256color)

;;; xterm-256color.el ends here
"#;

/// The elisp reporter: emacs' whole harness, because emacs has no hooks.
///
/// It reports through `aterm ctl @self meta set <field>=<value>`, whose field
/// set is MEASURED 2026-09-21 from the server's own refusal:
/// `ERR unknown meta field (title|description|icon|role|attention)`.
///
/// Three rules it never breaks: it never signals (every call is wrapped, and
/// a missing `aterm` is silence, not an error in the minibuffer); it never
/// blocks (the process is started and forgotten, output discarded); and it
/// reports only what emacs itself already knows. The SPINE does not need this
/// file at all — `status detail=emacs` and the mode-line frame test answer
/// with zero elisp loaded, which is the central law restated for a program
/// with no vendor surface whatsoever.
pub const EMACS_REPORTER: &str = r#";;; aterm-harness.el --- report emacs state to aterm  -*- lexical-binding: t; -*-
;; SPDX-License-Identifier: Apache-2.0
;; Copyright 2026 Andrew Yates

;;; Commentary:

;; Emacs has no hook vocabulary a vendor gives us, which is the point: this
;; file is ENRICHMENT over aterm's own view, never the source of it.  With
;; this file absent, aterm still classifies the session from `status' and the
;; mode line.  With it present, a long compilation says so by name.

;;; Code:

(defgroup aterm-harness nil
  "Report emacs state to the aterm session that owns this tty."
  :group 'tools)

(defcustom aterm-harness-program "aterm"
  "The aterm executable.  Absent means this file reports nothing."
  :type 'string
  :group 'aterm-harness)

(defcustom aterm-harness-enabled t
  "Non-nil to report.  The off switch, local to emacs."
  :type 'boolean
  :group 'aterm-harness)

(defun aterm-harness--call (&rest args)
  "Run aterm ctl with ARGS, discarding output.  Never signals, never blocks."
  (when (and aterm-harness-enabled
             (executable-find aterm-harness-program))
    (condition-case nil
        (let ((process-connection-type nil))
          (apply #'start-process "aterm-harness" nil
                 aterm-harness-program "ctl" "@self" args)
          nil)
      (error nil))))

(defun aterm-harness--meta (field value)
  "Set session meta FIELD to VALUE."
  (aterm-harness--call "meta" "set" (format "%s=%s" field value)))

(defun aterm-harness-note (text)
  "Describe this session to aterm as TEXT."
  (aterm-harness--meta "description" text))

(defun aterm-harness-attention (text)
  "Raise TEXT as this session's attention row."
  (aterm-harness--meta "attention" text))

(defun aterm-harness--compilation-started (proc)
  "Note that compilation PROC began."
  (aterm-harness-note (format "compiling: %s" (process-name proc))))

(defun aterm-harness--compilation-finished (buffer status)
  "Note that compilation in BUFFER ended with STATUS."
  (ignore buffer)
  (let ((word (string-trim (format "%s" status))))
    (aterm-harness-note (format "compile %s" word))
    (unless (string-prefix-p "finished" word)
      (aterm-harness-attention (format "compile %s" word)))))

(add-hook 'compilation-start-hook #'aterm-harness--compilation-started)
(add-hook 'compilation-finish-functions #'aterm-harness--compilation-finished)

(provide 'aterm-harness)

;;; aterm-harness.el ends here
"#;

// ---------------------------------------------------------------------------
// The codex tree (design §6.3)
// ---------------------------------------------------------------------------

/// The codex config profile the prelude selects with `-p aterm`.
///
/// A TEMPLATE: the host renders it through [`render_tree_text`] into
/// `$CODEX_HOME` at launch, behind a marker, never merged into a file the
/// user owns. It carries `notify`, which design §6.3 put in the prelude as a
/// third `-c` — see the comment on [`CODEX`]'s `args` for why it cannot live
/// there.
///
/// `notify` is the first of the three TRUST-FREE channels: a command codex
/// runs on turn completion, with no hook and no `trusted_hash` in the path.
///
/// UNVERIFIED: that codex reads these keys from a profile table, and that a
/// profile table may carry `notify` at all. The literals
/// `check_for_update_on_startup` (16), `notification_method` (10) and
/// `notify` (51) are MEASURED present in the codex-cli 0.155.1 store binary;
/// their SEMANTICS under `-p` are not.
pub const CODEX_CONFIG: &str = r#"# aterm harness — codex profile (design §6.3)
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrew Yates
#
# Selected with `-p aterm`. aterm owns updates, so the program never checks.
# Notifications ride OSC 9, which aterm already parses; turn completion rides
# `notify`, a plain command with no hook trust in the path.

[profiles.aterm]
check_for_update_on_startup = false
notify = ["${HARNESS_ROOT}/bin/claude-harness", "codex-notify"]

[profiles.aterm.tui]
notification_method = "osc9"
"#;

/// Substitute `${HARNESS_ROOT}` and `${HARNESS_STATE}` in a tree TEMPLATE the
/// host lays at launch — the codex config above, and anything like it.
///
/// This is NOT [`super::align::Shim::render`]'s job: that one re-admits an
/// env or arg against the shim's shape rule, which a config file is not. The
/// two substitutions are the same two, deliberately, so a tree author learns
/// one vocabulary.
///
/// The signed tree keeps the unsubstituted bytes, so `probe_set` and
/// `tree_root` still cover what was published; only the launch-time copy
/// carries a path.
#[must_use]
pub fn render_tree_text(text: &str, root: &Path, state: &Path) -> String {
    text.replace(SUBST_ROOT, &root.display().to_string())
        .replace(SUBST_STATE, &state.display().to_string())
}

/// The trust-free rm decision: a prefix rule codex reads with no hook and no
/// hash to trust.
///
/// This is the whole reason codex's rm capability is `degraded` and not
/// `unsupported`: the decision happens, it simply leaves no per-command row
/// the way a `PostToolUse` hook would. The grammar is UNVERIFIED (the literal
/// `prefix_rule` is MEASURED 20 lines in codex-cli 0.155.1; that this file
/// parses is not).
pub const CODEX_RULES: &str = r#"# aterm harness — codex rules (design §6.3)
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrew Yates
#
# Trust-free: no hook, no trusted_hash, no decision channel of ours.
# It approves nothing the owner's rm policy would refuse, because it approves
# only the bare prefix and codex still shows its own prompt for the rest.
prefix_rule(pattern=["rm"], decision="allow")
"#;

/// Codex's hook manifest — shipped, and honestly `degraded` until the user
/// trusts it once.
///
/// MEASURED in codex-cli 0.155.1 (`grep -a -c -F` over the 222 MB store
/// binary, 2026-09-21): `hooks.json` 7, `trusted_hash` 21, `PreToolUse` 36,
/// `CLAUDE_PLUGIN_ROOT` 1 — the same vocabulary, including the compatibility
/// variable that lets ONE `bridge.sh` serve both programs. The harness never
/// passes `--dangerously-bypass-hook-trust` (literal MEASURED 4 lines); a
/// user who does not trust it gets the trust-free channels above and a
/// `degraded` row saying so.
///
/// Every command carries THREE arguments — the bridge, the event, and the
/// capability id — so a capability that is off makes the hook inert without
/// editing this file (design §4.5 rule (b)).
pub const CODEX_HOOKS: &str = r#"{
  "hooks": {
    "PreToolUse": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "${CLAUDE_PLUGIN_ROOT}/hooks/bridge.sh PreToolUse rm-approve"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "${CLAUDE_PLUGIN_ROOT}/hooks/bridge.sh Stop liveness"
          }
        ]
      }
    ]
  }
}
"#;

// ---------------------------------------------------------------------------
// The three profiles
// ---------------------------------------------------------------------------

/// Claude Code (design §6.1) — the primary, and the only one with a native
/// hook channel.
pub static CLAUDE: Profile = Profile {
    id: "claude",
    wraps_any: &["claude"],
    forks: &[],
    policy: Policy::HooksOnly,
    patch: PatchPolicy::Deny,
    env: &["DISABLE_UPDATES=1"],
    args: &[
        "--plugin-dir",
        "${HARNESS_ROOT}/plugin",
        "--settings",
        "${HARNESS_STATE}/settings.json",
    ],
    actuators: &["meta", "notice", "decide", "turn", "key:escape", "restart"],
    egress: &["oauth2.googleapis.com", "sheets.googleapis.com"],
    tools: &[
        ("node", "20", &["usage-hud.sheet"]),
        ("sh", "", &["rm-approve", "usage-hud", "liveness"]),
        ("grep", "", &["introspect"]),
    ],
    caps: CLAUDE_CAPS,
    required: &[],
    channels: Channels {
        // MEASURED 2026-09-21 on the live session that wrote this file:
        // `aterm ctl ls` reads `detail=-`, because an ADOPTED session owns no
        // shell-integration block. The program emits no marks of its own.
        osc133: false,
        osc9: false,
        notify_cmd: false,
        hooks: Hooks::Native,
        session_log: true,
        turns: TurnEvidence::Phase,
    },
    identity: Identity {
        detail_any: &["claude"],
        frame: Some(has_composer_frame),
    },
    // The claude tree's plugin/, probes/ and policy/ are stage 2's and stage
    // 6's; this profile contributes no file of its own, which is why the list
    // is empty rather than duplicating them.
    files: &[],
    reach_note: "",
};

static CLAUDE_CAPS: &[CapRow] = &[
    CapRow {
        id: "rm-approve",
        needs: &[
            "hook.PermissionRequest",
            "hook.PreToolUse",
            "hook.PostToolUse",
            "wire.permissionDecision",
        ],
        max_level: 2,
        budget: "60/1h",
        serve: Serve::Ok,
        on_fork: None,
    },
    CapRow {
        id: "usage-hud",
        needs: &[
            "help.settings",
            "literal.statusLine",
            "literal.rate_limits",
            "file.cachedUsage",
            "settings.statusline_precedence",
        ],
        max_level: 1,
        budget: "",
        serve: Serve::Ok,
        on_fork: None,
    },
    CapRow {
        id: "model-failover",
        needs: &[
            "literal.StopFailure",
            "literal.PostModelSwitch",
            "help.fallback-model",
        ],
        max_level: 3,
        budget: "4/6h",
        serve: Serve::Ok,
        on_fork: None,
    },
    CapRow {
        id: "liveness",
        needs: &[
            "literal.Stop",
            "literal.StopFailure",
            "literal.Notification",
        ],
        max_level: 3,
        budget: "12/1h",
        serve: Serve::Ok,
        on_fork: None,
    },
    CapRow {
        id: "disk",
        needs: &[],
        max_level: 0,
        budget: "",
        serve: Serve::Ok,
        on_fork: None,
    },
    CapRow {
        id: "accounts",
        needs: &["help.resume", "literal.CLAUDE_CONFIG_DIR"],
        max_level: 4,
        budget: "4/6h",
        serve: Serve::Ok,
        on_fork: None,
    },
    CapRow {
        id: "limits",
        needs: &[
            "hook.StopFailure",
            "hook.Notification",
            "literal.quota_auto_resume",
            "literal.five_hour",
            "help.settings",
            "file.cachedUsage",
            "validate.hooks",
        ],
        max_level: 4,
        budget: "4/6h",
        serve: Serve::Ok,
        on_fork: None,
    },
    CapRow {
        id: "introspect",
        needs: &[],
        max_level: 0,
        budget: "",
        serve: Serve::Ok,
        on_fork: None,
    },
    CapRow {
        id: "exact-blocks",
        needs: &["literal.osc133"],
        max_level: 0,
        budget: "",
        // MEASURED: an adopted claude session reads `blocks OK 0`. The
        // capability is named so the answer is PRINTED rather than missing.
        serve: Serve::Unsupported(
            "claude emits no OSC 133 of its own and an adopted session owns no \
             shell-integration block, so a turn is bounded by status phase and \
             revision instead",
        ),
        on_fork: None,
    },
];

/// Codex (design §6.3) — the same hook vocabulary behind a trust gate, plus
/// three trust-free channels that need no hook trust at all.
pub static CODEX: Profile = Profile {
    id: "codex",
    // Preference order, and the ONLY fork-selection mechanism: when `acx` is
    // installed and aligned, the prelude targets it (design §6.3).
    wraps_any: &["acx", "codex"],
    forks: &[FORK_ROW],
    policy: Policy::Config,
    patch: PatchPolicy::Source,
    // CODEX_HOME is set DYNAMICALLY by the prelude, after aterm's PTY
    // sanitizer strips `CODEX_` (env_sanitize.rs:21), and so is never a
    // `[shim] env` entry.
    env: &[],
    // Design §6.3 puts `notify` here as a third `-c`. It CANNOT go here: a
    // `notify` value is a TOML array of strings, and the §1.2 contract
    // grammar refuses a string carrying a quote (align.rs's reader), so the
    // whole harness.toml would be refused. It lives in [`CODEX_CONFIG`]
    // instead, which the host RENDERS into `$CODEX_HOME` at launch and can
    // therefore substitute the tree path into — the same treatment claude's
    // `${HARNESS_STATE}/settings.json` already gets. Reported as a deviation.
    args: &[
        "-c",
        "check_for_update_on_startup=false",
        "-c",
        "tui.notification_method=osc9",
        "-p",
        "aterm",
    ],
    actuators: &["meta", "notice", "decide", "turn", "restart"],
    egress: &[],
    tools: &[("sh", "", &["liveness", "rm-approve"])],
    caps: CODEX_CAPS,
    required: &[],
    channels: Channels {
        // MEASURED 2026-09-21, codex-cli 0.155.1, `grep -a -c -F` over the
        // store binary: `133;A` → 0. Block-based waits cannot serve codex.
        osc133: false,
        // `notification_method` 10 lines; the prelude selects `osc9`, which
        // aterm's own `handler_osc_notify` parses.
        osc9: true,
        // `notify` 51 lines — a command run on turn completion, no hook trust
        // involved.
        notify_cmd: true,
        // `trusted_hash` 21 lines: the vocabulary exists, the trust does not
        // until a human grants it once.
        hooks: Hooks::TrustGated,
        // The rollout jsonl, which carries `rate_limits` (21 lines). Shape
        // UNVERIFIED.
        session_log: true,
        turns: TurnEvidence::Phase,
    },
    identity: Identity {
        detail_any: &["codex", FORK_ROW],
        // No frame test. MEASURED 2026-09-21: a real codex session's first
        // screen is its own directory-trust gate, not a composer, and nothing
        // on it is distinctive enough to name codex without guessing. The
        // spine names it from `detail=codex`, which it MEASURED correctly.
        frame: None,
    },
    files: CODEX_FILES,
    reach_note: "",
};

static CODEX_FILES: &[TreeFile] = &[
    TreeFile {
        path: "codex/aterm.config.toml",
        body: CODEX_CONFIG,
        exec: false,
    },
    TreeFile {
        path: "codex/rules/aterm.rules",
        body: CODEX_RULES,
        exec: false,
    },
    TreeFile {
        path: "codex/hooks.json",
        body: CODEX_HOOKS,
        exec: false,
    },
];

static CODEX_CAPS: &[CapRow] = &[
    CapRow {
        id: "rm-approve",
        needs: &["literal.prefix_rule"],
        max_level: 1,
        budget: "60/1h",
        serve: Serve::Degraded(
            "codex answers rm from the trust-free rules file, so the decision \
             happens with no hook trusted — but it leaves no per-command row \
             until the user trusts hooks once",
        ),
        on_fork: Some(Serve::Ok),
    },
    CapRow {
        id: "usage-hud",
        needs: &["literal.rate_limits"],
        max_level: 1,
        budget: "",
        serve: Serve::Degraded(
            "codex has no statusLine slot, so the HUD is readable through \
             harness usage and appstatus but never in the vendor's own footer; \
             the numbers come from the rollout log, whose shape is UNVERIFIED",
        ),
        on_fork: None,
    },
    CapRow {
        id: "model-failover",
        needs: &[],
        max_level: 0,
        budget: "",
        serve: Serve::Unsupported(
            "no --fallback-model and no model-switch event measured in \
             codex-cli 0.155.1: a model change needs a relaunch, which is a \
             human's call, not this harness's",
        ),
        on_fork: None,
    },
    CapRow {
        id: "liveness",
        needs: &["literal.notify"],
        max_level: 2,
        budget: "12/1h",
        serve: Serve::Degraded(
            "codex emits zero OSC 133, so a turn boundary is not exact: the \
             spine bounds it with status phase and revision, corroborated by \
             the notify command and OSC 9 — both trust-free",
        ),
        on_fork: Some(Serve::Ok),
    },
    CapRow {
        id: "disk",
        needs: &[],
        max_level: 0,
        budget: "",
        // Disk is aterm-side. It is program-independent by construction, and
        // this row is the proof that "program-independent" was checked and
        // not assumed.
        serve: Serve::Ok,
        on_fork: None,
    },
    CapRow {
        id: "accounts",
        needs: &["help.resume"],
        max_level: 4,
        budget: "4/6h",
        serve: Serve::Degraded(
            "CODEX_HOME per account is set by the prelude after aterm's \
             sanitizer strips CODEX_, but that a second home carries its own \
             credentials is UNVERIFIED",
        ),
        on_fork: None,
    },
    CapRow {
        id: "limits",
        needs: &["literal.rate_limits"],
        max_level: 1,
        budget: "4/6h",
        serve: Serve::Degraded(
            "the class comes from the grid and the rollout log, which is rank \
             1 and classifies with no hook at all; the cap is the TURN, not \
             the evidence — every L3 act is a turn and codex emits zero OSC \
             133, so its boundary is heuristic until acx",
        ),
        on_fork: None,
    },
    CapRow {
        id: "introspect",
        needs: &[],
        max_level: 0,
        budget: "",
        serve: Serve::Ok,
        on_fork: None,
    },
    CapRow {
        id: "exact-blocks",
        needs: &["literal.osc133"],
        max_level: 0,
        budget: "",
        serve: Serve::Unsupported(
            "codex-cli 0.155.1 emits zero OSC 133 (`133;A` measured 0), so \
             block-based waits do not serve it; the spine uses status phase \
             and revision plus the notify command and OSC 9",
        ),
        // The fork exists to emit nonced marks — design §6.3's first
        // decision-rule clause, and the only capability that meets it.
        on_fork: Some(Serve::Ok),
    },
    CapRow {
        id: "hooks-trusted",
        needs: &["literal.trusted_hash"],
        max_level: 0,
        budget: "",
        serve: Serve::Degraded(
            "codex gates hooks behind a trusted_hash the user answers once, \
             and the harness never passes --dangerously-bypass-hook-trust",
        ),
        on_fork: Some(Serve::Ok),
    },
];

/// Emacs (design §6.2) — a config-only program with no twin, no hooks, and no
/// vendor surface of any kind. The generality proof.
pub static EMACS: Profile = Profile {
    id: "emacs",
    wraps_any: &["emacs"],
    forks: &[],
    policy: Policy::Elisp,
    // GPL-3: nothing of emacs is rewritten, and elisp covers everything
    // measured, so the source lane is the policy and the patch lane never
    // runs.
    patch: PatchPolicy::Source,
    // A runtime-loader variable of the class shim_env's rule refuses for
    // PYTHONPATH/PERL5OPT/RUBYOPT; it passes today only by omission from a
    // finite list, so the profile carries it explicitly and the owner records
    // the allow (design §6.2).
    env: &["EMACSLOADPATH=${HARNESS_ROOT}/emacs/lisp:"],
    args: &[],
    actuators: &["meta", "notice"],
    egress: &[],
    tools: &[],
    caps: EMACS_CAPS,
    required: &[],
    channels: Channels {
        osc133: false,
        osc9: false,
        notify_cmd: false,
        hooks: Hooks::None,
        session_log: false,
        turns: TurnEvidence::Phase,
    },
    identity: Identity {
        detail_any: &["emacs", "emacsclient"],
        frame: Some(has_emacs_modeline),
    },
    files: EMACS_FILES,
    reach_note: EMACS_REACH,
};

static EMACS_FILES: &[TreeFile] = &[
    TreeFile {
        path: "emacs/lisp/term/xterm-256color.el",
        body: EMACS_TERM_FILE,
        exec: false,
    },
    TreeFile {
        path: "emacs/lisp/aterm-harness.el",
        body: EMACS_REPORTER,
        exec: false,
    },
];

static EMACS_CAPS: &[CapRow] = &[
    CapRow {
        id: "rm-approve",
        needs: &[],
        max_level: 0,
        budget: "",
        serve: Serve::Unsupported(
            "emacs has no tool-approval channel: there is no permission prompt \
             to answer and no decision to record",
        ),
        on_fork: None,
    },
    CapRow {
        id: "usage-hud",
        needs: &[],
        max_level: 0,
        budget: "",
        serve: Serve::Unsupported(
            "emacs is not a model client: it has no usage window, no account \
             and no rate limit to show",
        ),
        on_fork: None,
    },
    CapRow {
        id: "model-failover",
        needs: &[],
        max_level: 0,
        budget: "",
        serve: Serve::Unsupported("emacs runs no model, so there is nothing to fail over"),
        on_fork: None,
    },
    CapRow {
        id: "liveness",
        needs: &["elisp.terminal_init_xterm"],
        max_level: 1,
        budget: "12/1h",
        serve: Serve::Degraded(
            "aterm's own view answers whether emacs is working, and the elisp \
             reporter names a long compilation — but there is no prod rung: \
             Escape is not a remedy for a wedged emacs",
        ),
        on_fork: None,
    },
    CapRow {
        id: "disk",
        needs: &[],
        max_level: 0,
        budget: "",
        serve: Serve::Ok,
        on_fork: None,
    },
    CapRow {
        id: "accounts",
        needs: &[],
        max_level: 0,
        budget: "",
        serve: Serve::Unsupported("emacs has no account to rotate"),
        on_fork: None,
    },
    CapRow {
        id: "limits",
        needs: &[],
        max_level: 0,
        budget: "",
        serve: Serve::Unsupported("emacs has no usage limit to classify"),
        on_fork: None,
    },
    CapRow {
        id: "introspect",
        needs: &[],
        max_level: 0,
        budget: "",
        serve: Serve::Ok,
        on_fork: None,
    },
    CapRow {
        id: "exact-blocks",
        needs: &[],
        max_level: 0,
        budget: "",
        serve: Serve::Unsupported(
            "emacs emits no OSC 133; the spine bounds its work with status \
             phase and revision",
        ),
        on_fork: None,
    },
];

/// Every shipped profile, in preference order for [`identify`].
pub static PROFILES: &[&Profile] = &[&CLAUDE, &CODEX, &EMACS];

#[cfg(test)]
#[path = "profile_tests.rs"]
mod profile_tests;
