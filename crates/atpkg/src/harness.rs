// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE ONE HOME of everything atpkg knows about a HARNESS
//! (`docs/DESIGN-aterm-wrapper-2026-09-17.md`): the row WORDS of §3.8 and the
//! PATHS of §1.3/§4.4, which were two homes until this module existed — the
//! words in `state.rs` beside every other member-state spelling, the paths as
//! six `Layout` methods and two free functions in `store.rs`, a thousand lines
//! apart in a file about something else. A reader asking "where does harness
//! state live" had to know both, and a change to the layout had no reason to
//! meet the row that renders it.
//!
//! The split that remains is the only one that carries meaning, and it is
//! deliberate: the VERDICT vocabulary (`aligned`, `degraded(5/7):…`) is
//! computed in `aterm-agent`, which does not depend on this crate, so [`row`]
//! takes the verdict as a `&str` wire word rather than a type. That string is
//! the contract, and both sides test it.
//!
//! ## UNREACHED, and what would reach it (audit item, 2026-09-22)
//!
//! Nothing in the shipped binary calls anything in this module yet. That is
//! stated here, once, rather than implied by silence:
//!
//! * the ROW family is written by the update pass that learns a harness build's
//!   alignment verdict, and rendered by `doctor`, `which` and the Packages
//!   rows. That pass does not exist: no atpkg index publishes a harness
//!   package, so no member row has a harness beside it to describe. The words
//!   are here FIRST because §3.8's whole point is that a harness row must never
//!   read as a fault to `doctor`'s allow-by-prefix list, and the test below
//!   pins that against every fault prefix — a property that is cheap now and
//!   archaeology after the first row ships.
//! * the PATH family is written by the same publishing ceremony (the staged
//!   tree's `harness.toml`, its `probes/`, and the `<build>.harness` sidecar
//!   the shim prelude is rendered into). [`clear_sidecar`] is the exception
//!   and IS reached: `store::discard_build` calls it, so the sidecar's discard
//!   path shipped before its writer — the ordering the `.shim-env` leak cost
//!   us once.
//!
//! What would reach the rest: an index row of kind `harness` plus the stage
//! lane that renders its prelude. Until then this module is a library with
//! tests, and the honest status of the seam is UNVERIFIED-in-production, not
//! "wired".

use std::path::{Path, PathBuf};

use crate::store::Layout;

// ---------------------------------------------------------------------------
// The row words (design §3.7, §3.8)
// ---------------------------------------------------------------------------

/// The head of every row in the HARNESS family: `harness for <program> <build>: …`
/// (design §3.8).
///
/// The family is a SECOND row about a harness package, never a replacement for its
/// member state: a harness is an ordinary `binary` row that is `managed …` or `extra —
/// not installed (…)` like any other, and this says how well it fits the program it
/// wraps. Nothing here starts with a fault prefix (`error:`, `unavailable:`, `blocked:`,
/// `aborted:`, `tombstoned:`), so doctor's allow-by-prefix fault list reads every one of
/// them as benign — which they are: a degraded harness costs a capability, and the
/// wrapped program is unaffected either way (§3.8).
pub const PREFIX: &str = "harness for ";

/// The whole row of a harness the durable master switch has turned off
/// (`harness.enabled = false` in `aterm.toml`, design §4.6.2).
pub const DISABLED: &str = "harness — disabled";

/// The whole row of a harness one session has bypassed (design §4.6.2's env escape).
pub const BYPASSED: &str = "harness — bypassed (ATERM_NO_HARNESS)";

/// The head of the `held:`-class row `hold-program` writes, rendered INSIDE the harness
/// family rather than with [`crate::state::HELD_PREFIX`]: that prefix's one constructor
/// ([`crate::state::held_unpublished`]) has the wrong grammar, and the update pass
/// prefix-lifts any `held:` row to `managed …` on its next UpToDate arm, which would
/// silently drop the hold (design §3.7).
pub const HOLDS_PREFIX: &str = "harness holds ";

/// The tail of the row a harness with nothing measured yet carries.
pub const PENDING_TAIL: &str = "probe pending";

/// The one separator between a harness row's subject and its verdict, named so the
/// constructor and both parsers can never drift on it.
const SEP: &str = ": ";

/// The HARNESS family's row for one wrapped program (design §3.8):
///
/// ```text
/// harness for claude 2026091701: aligned (7/7)
/// harness for claude 2026091701: degraded (5/7) — usage-hud, model-failover
/// harness for claude 2026091801: unsupported — needs aterm ABI 2, running 1
/// harness for claude 2026091801: probe pending
/// ```
///
/// `wire` is the WORD the harness host computed
/// (`aterm_agent::harness::align::Alignment::wire`): `aligned(7/7)`,
/// `degraded(5/7):usage-hud,model-failover`, `unsupported:<reason>`, `pending`. THE
/// SEAM IS DELIBERATE and it is the reason this function takes a string rather than a
/// verdict type: the vocabulary of verdicts is computed in `aterm-agent`, which does not
/// depend on this crate, while this module is the single home of every harness SPELLING.
/// One home each, and the string between them is the contract both sides test.
///
/// The tie breaks toward the safe answer: a `wire` this client cannot read — a future
/// verdict word, a truncated sidecar row, an empty string — renders `probe pending`,
/// which claims nothing and renders no capability.
///
/// `local` adds the ` (local)` note of §3.5: no signed row covered this program build,
/// so the client's own probes are the whole of the evidence.
#[must_use]
pub fn row(program: &str, build: u64, wire: &str, local: bool) -> String {
    let mut s = String::from(PREFIX);
    s.push_str(program);
    s.push(' ');
    s.push_str(&crate::dec_u64(build));
    s.push_str(SEP);
    let (word, counts, tail) = split_wire(wire);
    match word {
        "aligned" | "degraded" => {
            s.push_str(word);
            if let Some(counts) = counts {
                s.push_str(" (");
                s.push_str(counts);
                s.push(')');
            }
            if word == "degraded" && !tail.is_empty() {
                s.push_str(" — ");
                push_comma_list(&mut s, tail);
            }
        }
        "unsupported" if !tail.is_empty() => {
            s.push_str("unsupported — ");
            s.push_str(tail);
        }
        _ => s.push_str(PENDING_TAIL),
    }
    if local {
        s.push_str(" (local)");
    }
    s
}

/// `harness for claude 2026091801: candidate 2026091802 refused — required: rm-approve`
/// — a harness BUILD whose `required` capabilities failed against the installed program,
/// so the flip did not happen and the previous build keeps running (design §3.1, §3.8).
///
/// Derived from the durable refusal memo under [`Layout::harness_refused_dir`], never
/// from the candidate tree: gc reclaims a staged-but-never-activated build with its
/// sidecars, so by the time this row is written the tree it names is gone.
#[must_use]
pub fn candidate_refused(program: &str, build: u64, candidate: u64, caps: &str) -> String {
    let mut s = String::from(PREFIX);
    s.push_str(program);
    s.push(' ');
    s.push_str(&crate::dec_u64(build));
    s.push_str(": candidate ");
    s.push_str(&crate::dec_u64(candidate));
    s.push_str(" refused — required: ");
    push_comma_list(&mut s, caps);
    s
}

/// `harness holds claude at 2026091701 (needs ≤ 2026091701; index pins 2026091801)` —
/// the `hold-program` row of design §3.7, rendered inside the harness family.
///
/// NOT [`crate::state::HELD_PREFIX`]: that prefix's one constructor is
/// [`crate::state::held_unpublished`], whose grammar is about an unpublished artifact,
/// and the update pass lifts any `held:` row to `managed …` in its UpToDate arm — which
/// would quietly cancel a hold the owner asked for. Doctor's "once the index publishes
/// for this target" sentence is likewise never printed for this row, because nothing
/// about it is waiting on a publish.
#[must_use]
pub fn holds(program: &str, at: u64, index_pin: u64) -> String {
    let mut s = String::from(HOLDS_PREFIX);
    s.push_str(program);
    s.push_str(" at ");
    s.push_str(&crate::dec_u64(at));
    s.push_str(" (needs ≤ ");
    s.push_str(&crate::dec_u64(at));
    s.push_str("; index pins ");
    s.push_str(&crate::dec_u64(index_pin));
    s.push(')');
    s
}

/// Whether `state` is any row of the harness family — the four verdict rows, the refused
/// candidate, the hold, and the two whole-row switches.
#[must_use]
pub fn is_row(state: &str) -> bool {
    state.starts_with(PREFIX)
        || state.starts_with(HOLDS_PREFIX)
        || state == DISABLED
        || state == BYPASSED
}

/// `Some((program, build))` for a `harness for <program> <build>: …` row; `None` for
/// every other state. The inverse of [`row`] and [`candidate_refused`] (a program name
/// never contains a space).
#[must_use]
pub fn program(state: &str) -> Option<(&str, u64)> {
    let rest = state.strip_prefix(PREFIX)?;
    let head = rest.split_once(SEP)?.0;
    let (program, build) = head.rsplit_once(' ')?;
    if program.is_empty() || program.contains(' ') {
        return None;
    }
    Some((program, build.parse().ok()?))
}

/// The verdict WORD a harness row carries — `aligned`, `degraded`, `unsupported`,
/// `probe pending` or `candidate refused` — read back off the rendered row, so a surface
/// that only needs the word never re-derives it from the wire.
#[must_use]
pub fn verdict(state: &str) -> Option<&str> {
    let rest = state.strip_prefix(PREFIX)?;
    let tail = rest.split_once(SEP)?.1;
    Some(match tail {
        t if t.starts_with("aligned") => "aligned",
        t if t.starts_with("degraded") => "degraded",
        t if t.starts_with("unsupported") => "unsupported",
        t if t.starts_with(PENDING_TAIL) => PENDING_TAIL,
        t if t.starts_with("candidate ") => "candidate refused",
        _ => return None,
    })
}

/// `aligned(7/7)` → `("aligned", Some("7/7"), "")`;
/// `degraded(5/7):a,b` → `("degraded", Some("5/7"), "a,b")`;
/// `unsupported:why` → `("unsupported", None, "why")`; anything else → `("", None, "")`.
fn split_wire(wire: &str) -> (&str, Option<&str>, &str) {
    let (head, tail) = match wire.split_once(':') {
        Some((head, tail)) => (head, tail),
        None => (wire, ""),
    };
    let (word, counts) = match head.split_once('(') {
        Some((word, counts)) => (word, counts.strip_suffix(')')),
        None => (head, None),
    };
    match word {
        "aligned" | "degraded" | "unsupported" => (word, counts, tail),
        _ => ("", None, ""),
    }
}

/// `a,b` → `a, b`, in place, with empty elements dropped.
fn push_comma_list(s: &mut String, list: &str) {
    let mut first = true;
    for item in list.split(',').map(str::trim).filter(|i| !i.is_empty()) {
        if !first {
            s.push_str(", ");
        }
        s.push_str(item);
        first = false;
    }
}

// ---------------------------------------------------------------------------
// The paths (design §1.3, §4.4)
// ---------------------------------------------------------------------------

/// The harness half of the prefix layout. An inherent impl in THIS file rather than in
/// `store.rs`: the spelling a caller types is unchanged (`layout.harness_dir(name)`),
/// and the six paths now sit beside the rows that describe what lives in them.
impl Layout {
    /// `harness/` — the HARNESS STATE root (design §1.3, §4.4): durable, mutable,
    /// per-harness state that must never live inside `store/`.
    ///
    /// Two invariants make it a separate directory rather than a corner of the store:
    /// the store tree is immutable as a documented invariant and is re-verified against a
    /// signed `tree_root`, so a byte written into it reads as `Drift`; and the
    /// `__harness` hidden verb (§1.4) runs BEFORE the store lock, so the one thing it may
    /// write has to be outside the single-writer contract's reach. Everything under here
    /// is that: alignment verdicts, refusal memos, ledgers, per-session state.
    #[must_use]
    pub fn harness_root(&self) -> PathBuf {
        self.prefix.join("harness")
    }

    /// `harness/<name>/` — one harness's durable state directory (design §4.4). `name` is
    /// gated by [`crate::store::shim_allowed`]'s name shape at every caller that takes it
    /// from outside; nothing here joins a path the caller supplied.
    #[must_use]
    pub fn harness_dir(&self, name: &str) -> PathBuf {
        self.harness_root().join(name)
    }

    /// `harness/<name>/alignment/<build>.toml` — the LOCAL probe verdicts, keyed inside
    /// the file by the tuple design §3.5 names (harness build, program build, the
    /// `aterm --version` identity token, `probe_set`).
    ///
    /// Outside `store/` on purpose: this is the one file the lock-free `__harness` verb
    /// appends to (§1.4), with the bump file's discipline — append-only, capped,
    /// symlink-refused.
    #[must_use]
    pub fn harness_alignment_file(&self, name: &str, build: u64) -> PathBuf {
        let mut file = crate::dec_u64(build);
        file.push_str(".toml");
        self.harness_dir(name).join("alignment").join(file)
    }

    /// `harness/<name>/refused/` — where the durable memo of a pre-flip probe refusal
    /// lives (design §1.3, §3.1).
    ///
    /// It has to be here rather than beside the candidate tree because the refused
    /// candidate is NOT retained: it sits above live, `gc::reclaimable` classifies it as
    /// staged-but-never-activated and the pass-closing gc deletes it with its sidecars.
    /// Without this memo the 6-hourly loop would re-download, re-stage and re-probe the
    /// same tuple forever, since `gate::decide` has no "pinned but locally refused" arm.
    #[must_use]
    pub fn harness_refused_dir(&self, name: &str) -> PathBuf {
        self.harness_dir(name).join("refused")
    }

    /// `store/<name>/<build>/harness.toml` — the in-tree, `tree_root`-signed contract of
    /// design §1.2, read and admitted by the host
    /// (`aterm_agent::harness::align::Contract::admit`).
    #[must_use]
    pub fn harness_contract_file(&self, name: &str, build: u64) -> PathBuf {
        self.build_dir(name, build).join("harness.toml")
    }

    /// `store/<name>/<build>/probes/` — the probe executables of design §3.2, whose
    /// sha256 the contract's `probe_set` must equal.
    #[must_use]
    pub fn harness_probes_dir(&self, name: &str, build: u64) -> PathBuf {
        self.build_dir(name, build).join("probes")
    }
}

/// The suffix of the RENDERED-SHIM sidecar beside a harness build dir
/// (`store/<name>/<build>.harness`, design §1.3).
///
/// A SIBLING, like `<build>.ready` and `<build>.shim-env`, so it never perturbs the
/// build's `tree_root` and can outlive a tree the stage lane is still assembling. It
/// holds the `[shim]` env and args as RENDERED for this prefix — the verbs that hold no
/// contract (rollback, `unlink`'s restore) re-lay a prelude from it rather than
/// re-reading and re-substituting a tree that may no longer be live.
///
/// NOT WRITTEN YET: the stage-lane seam that produces it is part of the owner's
/// publishing ceremony (the atpkg index row and the shim prelude), which this stage
/// deliberately does not build. The suffix and its removal exist FIRST so the sidecar
/// can never leak the way `2026091301.shim-env` did — a sidecar whose discard path is
/// written after the writer is a sidecar that outlives its tree.
pub const SIDECAR_SUFFIX: &str = ".harness";

/// `store/<program>/<build>.harness` for `build_dir`, or `None` for a path with no file
/// name.
#[must_use]
pub fn sidecar_path(build_dir: &Path) -> Option<PathBuf> {
    let name = build_dir.file_name()?.to_str()?;
    let mut sidecar = String::from(name);
    sidecar.push_str(SIDECAR_SUFFIX);
    Some(build_dir.with_file_name(sidecar))
}

/// Remove the rendered-shim sidecar beside `build_dir`, if any.
pub fn clear_sidecar(build_dir: &Path) {
    if let Some(sidecar) = sidecar_path(build_dir) {
        let _ = std::fs::remove_file(sidecar);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{BLOCKED_PREFIX, HELD_PREFIX, MANAGED_PREFIX, managed};
    use crate::store::discard_build;

    #[test]
    fn the_harness_family_is_spelled_exactly_and_reads_back() {
        assert_eq!(
            row("claude", 2_026_091_701, "aligned(7/7)", false),
            "harness for claude 2026091701: aligned (7/7)"
        );
        assert_eq!(
            row(
                "claude",
                2_026_091_701,
                "degraded(5/7):usage-hud,model-failover",
                false
            ),
            "harness for claude 2026091701: degraded (5/7) — usage-hud, model-failover"
        );
        assert_eq!(
            row(
                "claude",
                2_026_091_801,
                "unsupported:needs aterm ABI 2, running 1",
                false
            ),
            "harness for claude 2026091801: unsupported — needs aterm ABI 2, running 1"
        );
        assert_eq!(
            row("claude", 2_026_091_801, "pending", false),
            "harness for claude 2026091801: probe pending"
        );
        assert_eq!(
            row("claude", 2_026_091_701, "aligned(7/7)", true),
            "harness for claude 2026091701: aligned (7/7) (local)",
            "§3.5's note: no signed row covered this build"
        );
        assert_eq!(
            candidate_refused("claude", 2_026_091_801, 2_026_091_802, "rm-approve"),
            "harness for claude 2026091801: candidate 2026091802 refused — required: rm-approve"
        );
        assert_eq!(
            holds("claude", 2_026_091_701, 2_026_091_801),
            "harness holds claude at 2026091701 (needs ≤ 2026091701; index pins 2026091801)"
        );

        // NEGATIVE CASES: a wire word this client cannot read is `probe pending`, which
        // claims nothing — never a half-rendered row and never a guess.
        for wire in [
            "",
            "pending",
            "fine",
            "ALIGNED",
            "quarantined:why",
            "unsupported",
        ] {
            let rendered = row("claude", 2_026_091_801, wire, false);
            assert_eq!(
                rendered, "harness for claude 2026091801: probe pending",
                "wire {wire:?} rendered {rendered:?}"
            );
        }

        // The parsers read back what the constructors wrote.
        for (rendered, word) in [
            (
                row("claude", 2_026_091_701, "aligned(7/7)", false),
                "aligned",
            ),
            (
                row("claude", 2_026_091_701, "degraded(5/7):a", false),
                "degraded",
            ),
            (
                row("claude", 2_026_091_701, "unsupported:why", false),
                "unsupported",
            ),
            (
                row("claude", 2_026_091_701, "pending", false),
                "probe pending",
            ),
            (
                candidate_refused("claude", 2_026_091_701, 2_026_091_702, "rm-approve"),
                "candidate refused",
            ),
        ] {
            assert_eq!(
                program(&rendered),
                Some(("claude", 2_026_091_701)),
                "{rendered}"
            );
            assert_eq!(verdict(&rendered), Some(word), "{rendered}");
            assert!(is_row(&rendered), "{rendered}");
        }
        assert!(is_row(DISABLED));
        assert!(is_row(BYPASSED));
        assert!(is_row(&holds("claude", 1, 2)));
        assert!(!is_row(&managed(6808, 41)));
        assert_eq!(program(&managed(6808, 41)), None);
        assert_eq!(verdict(&managed(6808, 41)), None);
    }

    #[test]
    fn no_harness_row_reads_as_a_doctor_fault() {
        // doctor's fault list is allow-by-prefix (`error:`, `unavailable:`, `blocked:`,
        // `aborted:`, `tombstoned:`). A degraded harness costs a capability and leaves
        // the wrapped program alone, so none of these may look like a fault (§3.8).
        let rows = [
            row("claude", 1, "aligned(7/7)", false),
            row("claude", 1, "degraded(5/7):usage-hud", false),
            row("claude", 1, "unsupported:why", false),
            row("claude", 1, "pending", true),
            candidate_refused("claude", 1, 2, "rm-approve"),
            holds("claude", 1, 2),
            String::from(DISABLED),
            String::from(BYPASSED),
        ];
        for rendered in &rows {
            for fault in [
                "error:",
                "unavailable:",
                "blocked:",
                "aborted:",
                "tombstoned:",
            ] {
                assert!(
                    !rendered.starts_with(fault),
                    "{rendered:?} reads as a {fault} fault, which it is not"
                );
            }
            assert!(
                !rendered.starts_with(MANAGED_PREFIX)
                    && !rendered.starts_with(BLOCKED_PREFIX)
                    && !rendered.starts_with(HELD_PREFIX),
                "{rendered:?} collides with an existing family's prefix"
            );
        }
    }

    /// The harness layout of the wrapper design §1.3: every mutable byte lives under
    /// `<prefix>/harness/`, never inside `store/`, and every contract-side path is a
    /// child of the build tree the signed `tree_root` covers.
    #[test]
    fn the_harness_layout_keeps_mutable_state_out_of_the_store() {
        let l = Layout {
            prefix: PathBuf::from("/opt/aterm/pkg"),
        };
        assert_eq!(l.harness_root(), PathBuf::from("/opt/aterm/pkg/harness"));
        assert_eq!(
            l.harness_dir("claude-harness"),
            PathBuf::from("/opt/aterm/pkg/harness/claude-harness")
        );
        assert_eq!(
            l.harness_alignment_file("claude-harness", 2_026_092_101),
            PathBuf::from("/opt/aterm/pkg/harness/claude-harness/alignment/2026092101.toml")
        );
        assert_eq!(
            l.harness_refused_dir("claude-harness"),
            PathBuf::from("/opt/aterm/pkg/harness/claude-harness/refused")
        );
        // The two contract-side paths ARE inside the store, because they are signed
        // bytes the tree_root covers — that is the whole distinction.
        let build = l.build_dir("claude-harness", 2_026_092_101);
        assert_eq!(
            l.harness_contract_file("claude-harness", 2_026_092_101),
            build.join("harness.toml")
        );
        assert_eq!(
            l.harness_probes_dir("claude-harness", 2_026_092_101),
            build.join("probes")
        );
        // NEGATIVE: nothing mutable may be under store/, or the apply-time re-verify
        // reports it as Drift.
        let store = l.prefix.join("store");
        for mutable in [
            l.harness_root(),
            l.harness_dir("claude-harness"),
            l.harness_alignment_file("claude-harness", 1),
            l.harness_refused_dir("claude-harness"),
        ] {
            assert!(
                !mutable.starts_with(&store),
                "{} is inside store/",
                mutable.display()
            );
        }
    }

    /// The `<build>.harness` sidecar is a SIBLING and is discarded with the tree. The
    /// `.shim-env` sidecar leaked exactly once by being written before its discard path
    /// existed; this one has the discard path first.
    #[test]
    fn the_rendered_shim_sidecar_is_a_sibling_and_never_outlives_its_tree() {
        let root = std::env::temp_dir().join(format!(
            "atpkg-harness-sidecar-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let build_dir = root.join("store").join("claude-harness").join("2026092101");
        std::fs::create_dir_all(&build_dir).expect("the build dir is created");
        let sidecar = sidecar_path(&build_dir).expect("a build dir has a file name");
        assert_eq!(sidecar, build_dir.with_file_name("2026092101.harness"));
        assert_eq!(
            sidecar.parent(),
            build_dir.parent(),
            "a SIBLING, not a child"
        );
        std::fs::write(&sidecar, "args=--plugin-dir\n").expect("the sidecar is written");
        assert!(sidecar.exists());
        discard_build(&build_dir);
        assert!(!build_dir.exists(), "the tree is gone");
        assert!(!sidecar.exists(), "and the sidecar with it");
        // Removing a sidecar that is not there is not an error.
        clear_sidecar(&build_dir);
        assert_eq!(sidecar_path(Path::new("/")), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
