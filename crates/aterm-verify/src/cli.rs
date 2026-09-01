// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The flag surface, which is FROZEN.
//!
//! `--fast` / `--full` / `--scope <crate>` / `--selftest` are spelled into
//! docs/PROCESS.md, into the pre-push hook's advice, and into every standing
//! agent instruction in this repo. They keep working unedited, including
//! `--scope=<crate>` and `-h`/`--help`.
//!
//! Two additions, both for the bash shim that execs this driver: `--root <dir>`
//! (the shim already resolved the repo root from its own path, and a compiled
//! binary cannot) and the `ATERM_VERIFY_ROOT` environment variable behind it.
//! Neither changes any stage's decision.

use std::path::PathBuf;

/// `--fast` (the per-commit merge contract) or `--full` (+ differential oracle
/// and the trust-mc / Kani floor).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    #[default]
    Fast,
    Full,
}

impl Mode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Fast => "fast",
            Mode::Full => "full",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Args {
    pub mode: Mode,
    /// `None` = the whole workspace.
    pub scope: Option<String>,
    /// `--changed`: narrow to the diff's crates and their dependents.
    pub changed: bool,
    /// `--base <ref>` AS GIVEN. `None` means "not given", which is not the same
    /// as "given the default": only the first may pass the `--changed` check
    /// below, so `--base main` without `--changed` is still the usage error it
    /// always was in intent.
    pub base: Option<String>,
    pub selftest: bool,
    pub root: Option<PathBuf>,
    pub help: bool,
}

impl Args {
    /// The ref the diff is taken against: `--base`, then `ATERM_VERIFY_BASE`,
    /// then `main`. Kept a pure function of the two inputs so the default is
    /// testable without touching the process environment.
    #[must_use]
    pub fn base_ref(&self, from_env: Option<&str>) -> String {
        self.base
            .clone()
            .or_else(|| from_env.map(str::to_string))
            .unwrap_or_else(|| "main".to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    ScopeNeedsCrateName,
    BaseNeedsGitRef,
    RootNeedsPath,
    /// `--scope` and `--changed` are two different narrowings.
    ScopeAndChanged,
    /// `--base` without `--changed` narrows nothing and means nothing.
    BaseWithoutChanged,
    Unknown(String),
}

impl ParseError {
    /// The message the script wrote to stderr, unchanged where it existed.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            ParseError::ScopeNeedsCrateName => "verify: --scope needs a crate name".to_string(),
            ParseError::BaseNeedsGitRef => "verify: --base needs a git ref".to_string(),
            ParseError::RootNeedsPath => "verify: --root needs a directory".to_string(),
            ParseError::ScopeAndChanged => {
                "verify: --scope and --changed both narrow the build; pick one".to_string()
            }
            ParseError::BaseWithoutChanged => {
                "verify: --base <ref> only means something with --changed".to_string()
            }
            ParseError::Unknown(a) => format!("verify: unknown argument: {a}"),
        }
    }
}

/// Parse the command line.
///
/// # Errors
/// Returns the usage error to print before exiting `2`.
pub fn parse<I, S>(args: I) -> Result<Args, ParseError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut out = Args::default();
    let mut it = args.into_iter().map(Into::into);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--fast" => out.mode = Mode::Fast,
            "--full" => out.mode = Mode::Full,
            "--selftest" => out.selftest = true,
            "--changed" => out.changed = true,
            "-h" | "--help" => out.help = true,
            "--scope" => {
                let v = it.next().unwrap_or_default();
                if v.is_empty() {
                    return Err(ParseError::ScopeNeedsCrateName);
                }
                out.scope = Some(v);
            }
            "--base" => {
                let v = it.next().unwrap_or_default();
                if v.is_empty() {
                    return Err(ParseError::BaseNeedsGitRef);
                }
                out.base = Some(v);
            }
            "--root" => {
                let v = it.next().unwrap_or_default();
                if v.is_empty() {
                    return Err(ParseError::RootNeedsPath);
                }
                out.root = Some(PathBuf::from(v));
            }
            _ => {
                if let Some(v) = a.strip_prefix("--scope=") {
                    // FAIL-CLOSED DIVERGENCE, deliberate. Bash let `--scope=`
                    // set an EMPTY scope, which meant "the whole workspace" —
                    // so a typo silently WIDENED the claim all the way to the
                    // merge-contract sentence. That is precisely the class of
                    // false green the verdict discipline exists to stop.
                    if v.is_empty() {
                        return Err(ParseError::ScopeNeedsCrateName);
                    }
                    out.scope = Some(v.to_string());
                } else if let Some(v) = a.strip_prefix("--base=") {
                    // Same fail-closed rule as `--scope=`: bash let `--base=`
                    // set an EMPTY ref, which then failed to find a merge-base
                    // and WIDENED the run without the caller ever learning the
                    // flag was malformed.
                    if v.is_empty() {
                        return Err(ParseError::BaseNeedsGitRef);
                    }
                    out.base = Some(v.to_string());
                } else if let Some(v) = a.strip_prefix("--root=") {
                    if v.is_empty() {
                        return Err(ParseError::RootNeedsPath);
                    }
                    out.root = Some(PathBuf::from(v));
                } else {
                    return Err(ParseError::Unknown(a));
                }
            }
        }
    }
    // Two different narrowings: silently letting one win would make the printed
    // scope a lie about what was built.
    if out.scope.is_some() && out.changed {
        return Err(ParseError::ScopeAndChanged);
    }
    if out.base.is_some() && !out.changed {
        return Err(ParseError::BaseWithoutChanged);
    }
    Ok(out)
}

/// The `--help` text: the script's own header, kept as the contract it documents.
pub const USAGE: &str = "\
verify — the single local gate entrypoint for aterm.

aterm has NO CI by owner decision (docs/AUDIT.md, docs/PROCESS.md): the merge
contract is *this gate passing locally* before a slice enters the ff-only
main merge-queue. There is exactly one way to verify, so there is exactly one
way for a reviewer (human or AI) to be wrong about it: run this.

  tools/verify.sh --fast            # the per-commit gate (the merge contract)
  tools/verify.sh --full            # --fast + differential oracle + trust-mc
  tools/verify.sh --changed         # change-scoped tier (NOT the merge contract)
  tools/verify.sh --scope <crate>   # narrow build/test to one crate (+ guards)
  tools/verify.sh --fast --scope aterm-grid

--fast    : targo build + targo test --workspace + the zero-tolerance grep
            guards + bootstrap update-channel arbitration/identity checks + a
            headless control-socket smoke (the AI-first spine must never
            regress, so every gate run proves the socket still answers).
--full    : everything in --fast, PLUS the aterm-vs-alacritty differential
            oracle and the trust-mc / Kani BMC harnesses *when those tools are
            installed* (skipped-not-failed when absent — see docs/PROCESS.md).
--scope   : restrict the targo build/test to `-p <crate>`; the guards and the
            socket smoke always run whole-tree (they are cheap and global).
--changed : THE MISSING MIDDLE — a change-scoped tier between a bare `targo
            check` and a whole-tree run. (Until 2026-08-31 this line named the
            ~2 s pre-push L0 hook as the lower end. There is no such hook:
            `.githooks/pre-push` was demoted to ADVISORY on 2026-08-24 and its
            whole body is one printf and `exit 0`, so the tier below this one is
            whatever you run by hand.) Restricts build/test/doctest/lint to
            the crates this branch touches PLUS every workspace crate that
            depends on one of them (the reverse-dependency cone, read from the
            SAME dependency graph the build uses). `--base <ref>` (default
            `main`, or $ATERM_VERIFY_BASE) picks the merge-base the diff is
            taken against. Every whole-tree stage still runs. A narrowed run can
            NEVER claim the merge contract — the verdict names exactly what it
            left out — and if the scope cannot be computed honestly the run
            WIDENS to the whole workspace, because a broken narrower must do
            MORE work, never less.

A skip is an honest \"tool absent\", never a silent pass: skips are counted and
NAMED, and any run that skipped a stage or narrowed its scope is refused the
merge-contract verdict.

A stage child that never exits is a FAILURE, not a hang: every child runs under
a 45-minute wall-clock ceiling and is killed and reported past it, because a
gate that hangs has decided nothing and says nothing. $ATERM_VERIFY_STAGE_TIMEOUT
moves that ceiling (seconds); =off removes it and restores the unbounded wait.

exit 0  everything that ran was green (the verdict says which green)
exit 1  a gate FAILED — a real finding about the tree
exit 2  usage error
exit 3  COULD NOT RUN — the environment is broken; nothing was decided
";

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(args: &[&str]) -> Args {
        parse(args.iter().copied()).expect("parses")
    }

    #[test]
    fn the_frozen_spellings_all_still_work() {
        assert_eq!(ok(&[]).mode, Mode::Fast, "default is the per-commit gate");
        assert_eq!(ok(&["--fast"]).mode, Mode::Fast);
        assert_eq!(ok(&["--full"]).mode, Mode::Full);
        assert!(ok(&["--selftest"]).selftest);
        assert_eq!(
            ok(&["--scope", "aterm-grid"]).scope.as_deref(),
            Some("aterm-grid")
        );
        assert_eq!(
            ok(&["--scope=aterm-grid"]).scope.as_deref(),
            Some("aterm-grid")
        );
        assert!(ok(&["-h"]).help);
        assert!(ok(&["--help"]).help);
    }

    #[test]
    fn combinations_compose_and_the_last_mode_wins() {
        let a = ok(&["--fast", "--scope", "aterm-grid"]);
        assert_eq!(a.mode, Mode::Fast);
        assert_eq!(a.scope.as_deref(), Some("aterm-grid"));
        assert_eq!(ok(&["--full", "--fast"]).mode, Mode::Fast);
        assert_eq!(ok(&["--fast", "--full"]).mode, Mode::Full);
    }

    #[test]
    fn a_scope_without_a_crate_name_is_a_usage_error() {
        assert_eq!(parse(["--scope"]), Err(ParseError::ScopeNeedsCrateName));
        assert_eq!(parse(["--scope", ""]), Err(ParseError::ScopeNeedsCrateName));
        // The bash form that silently widened the claim to the whole workspace.
        assert_eq!(parse(["--scope="]), Err(ParseError::ScopeNeedsCrateName));
        assert_eq!(
            ParseError::ScopeNeedsCrateName.message(),
            "verify: --scope needs a crate name"
        );
    }

    #[test]
    fn an_unknown_argument_is_a_usage_error_naming_itself() {
        assert_eq!(parse(["--fest"]), Err(ParseError::Unknown("--fest".into())));
        assert_eq!(
            parse(["aterm-grid"]),
            Err(ParseError::Unknown("aterm-grid".into()))
        );
        assert_eq!(
            ParseError::Unknown("--fest".into()).message(),
            "verify: unknown argument: --fest"
        );
    }

    #[test]
    fn the_change_scoped_tier_has_its_own_flag_and_its_own_base() {
        assert!(ok(&["--changed"]).changed);
        assert_eq!(ok(&["--changed"]).base, None, "not given is not defaulted");
        assert_eq!(
            ok(&["--changed", "--base", "origin/main"]).base.as_deref(),
            Some("origin/main")
        );
        assert_eq!(
            ok(&["--changed", "--base=HEAD~5"]).base.as_deref(),
            Some("HEAD~5")
        );
        // …and it composes with the mode, which is the whole use for it.
        let a = ok(&["--full", "--changed"]);
        assert_eq!((a.mode, a.changed), (Mode::Full, true));
    }

    #[test]
    fn the_base_defaults_to_main_and_the_environment_can_move_it() {
        let given = ok(&["--changed", "--base", "HEAD~1"]);
        assert_eq!(given.base_ref(Some("release")), "HEAD~1", "the flag wins");
        let not_given = ok(&["--changed"]);
        assert_eq!(not_given.base_ref(Some("release")), "release");
        assert_eq!(not_given.base_ref(None), "main");
    }

    #[test]
    fn two_narrowings_at_once_is_a_usage_error_not_a_silent_winner() {
        // Letting one win would make the printed scope a lie about what was built.
        assert_eq!(
            parse(["--scope", "aterm-grid", "--changed"]),
            Err(ParseError::ScopeAndChanged)
        );
        assert_eq!(
            parse(["--changed", "--scope=aterm-grid"]),
            Err(ParseError::ScopeAndChanged)
        );
        assert_eq!(
            ParseError::ScopeAndChanged.message(),
            "verify: --scope and --changed both narrow the build; pick one"
        );
    }

    #[test]
    fn a_base_without_changed_narrows_nothing_and_says_so() {
        assert_eq!(
            parse(["--base", "origin/main"]),
            Err(ParseError::BaseWithoutChanged)
        );
        // Including the value that happens to be the default: the flag was
        // given, and a flag that means nothing here must not look accepted.
        assert_eq!(
            parse(["--base", "main"]),
            Err(ParseError::BaseWithoutChanged)
        );
        assert_eq!(
            ParseError::BaseWithoutChanged.message(),
            "verify: --base <ref> only means something with --changed"
        );
    }

    #[test]
    fn a_base_without_a_ref_is_a_usage_error() {
        assert_eq!(
            parse(["--changed", "--base"]),
            Err(ParseError::BaseNeedsGitRef)
        );
        assert_eq!(
            parse(["--changed", "--base", ""]),
            Err(ParseError::BaseNeedsGitRef)
        );
        // The bash form that silently widened the run instead of complaining.
        assert_eq!(
            parse(["--changed", "--base="]),
            Err(ParseError::BaseNeedsGitRef)
        );
        assert_eq!(
            ParseError::BaseNeedsGitRef.message(),
            "verify: --base needs a git ref"
        );
    }

    #[test]
    fn root_is_shim_plumbing_only() {
        assert_eq!(
            ok(&["--root", "/tmp/x"]).root,
            Some(PathBuf::from("/tmp/x"))
        );
        assert_eq!(ok(&["--root=/tmp/x"]).root, Some(PathBuf::from("/tmp/x")));
        assert_eq!(parse(["--root"]), Err(ParseError::RootNeedsPath));
        // and it changes nothing else about the run
        let a = ok(&["--root", "/tmp/x"]);
        assert_eq!(
            (a.mode, a.scope, a.selftest, a.changed),
            (Mode::Fast, None, false, false)
        );
    }

    #[test]
    fn usage_documents_every_frozen_flag_and_every_exit_code() {
        for flag in ["--fast", "--full", "--scope <crate>", "--changed", "--base"] {
            assert!(USAGE.contains(flag), "usage must document {flag}");
        }
        for code in ["exit 0", "exit 1", "exit 2", "exit 3"] {
            assert!(USAGE.contains(code), "usage must document {code}");
        }
    }
}
