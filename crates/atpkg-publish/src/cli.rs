// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Verb dispatch. Each verb parses its own typed flags; [`crate::ctx::Ctx::from_env_hosts`]
//! reads the environment and builds the hosts a leading `--host` chooses (`gh` by default).

use std::ffi::{OsStr, OsString};
use std::process::ExitCode;

use crate::ctx::{Ctx, HostChoice};
use crate::error::{Error, Result};
use crate::{artifact, channel, lane};

/// One CLI verb: name, synopsis, and one-line summary.
#[derive(Clone, Copy, Debug)]
pub struct Verb {
    /// Verb name.
    pub name: &'static str,
    /// Usage synopsis.
    pub synopsis: &'static str,
    /// One-line summary.
    pub does: &'static str,
}

/// Every verb, in `help` order.
pub const VERBS: &[Verb] = &[
    Verb {
        name: "canary",
        synopsis: crate::canary::SYNOPSIS,
        does: "install and execute the signed staged Trust pack; sign integrity evidence and consumer reports without publishing",
    },
    Verb {
        name: "pack",
        synopsis: "pack <program> --checkout <dir> [--triple T] [--build N] [--merge-from PKG] [--toolchain BIN] [--out DIR] [--trust-repo DIR] [--unsigned | --upload]",
        does: "build, gate, stage and sign one program for one target",
    },
    Verb {
        name: "pack-bundle",
        synopsis: "pack-bundle <program> --sysroot <dir> [--src-repo DIR] [--build N] [--version V] [--triple T] [--hello-sysroot DIR | --no-hello] [--donor-root DIR]… [--keep-target T]… [--merge-from PKG] [--out DIR] [--trust-repo DIR] [--allow-dirty] [--no-smoke] [--no-strip] [--unsigned | --upload]",
        does: "stage, prove, relocate, gate and sign a sysroot bundle (trust, trust-mc)",
    },
    Verb {
        name: "pack-package",
        synopsis: artifact::package::SYNOPSIS,
        does: "pack, gate and sign a prebuilt package tree (codex, built in codex-alab)",
    },
    Verb {
        name: "pack-bottle",
        synopsis: artifact::bottle::SYNOPSIS,
        does: "stage, relocate, gate and sign a declared upstream bottle or binary archive",
    },
    Verb {
        name: "pack-upstream",
        synopsis: artifact::bottle::upstream::SYNOPSIS,
        does: "acquire and qualify pinned upstream tools or GNU Git; unsigned, without upload",
    },
    Verb {
        name: "build-local",
        synopsis: "build-local <program> <checkout> [--plan] [--policy repository|unverified] [--profile release|dev] [--jobs N] [--toolchain BIN] [--out DIR] [--link]",
        does: "development build of a program: unsigned, never packed or uploaded",
    },
    #[cfg(unix)]
    Verb {
        name: "harness-local",
        synopsis: crate::harness_local::SYNOPSIS,
        does: "run or inspect guarded local harness updates; selection changes only local current",
    },
    Verb {
        name: "sign",
        synopsis: "sign <pkg.toml>… [--trust-repo DIR]",
        does: "sign staged manifests with this machine's key and verify them",
    },
    Verb {
        name: "upload",
        synopsis: "upload <staging|public> <repo> <tag> <file>…",
        does: "upload release assets; managed pack staging reserves its version, and raw public managed-pack replacement is refused",
    },
    Verb {
        name: "index",
        synopsis: "index [--baseline QUAD-DIR | --first CHANNEL --index-build N] [--pin P=B]… [--target-pin T:P=B[,…]]… [--yank P@B|P@V]… [--unyank P@B|P@V]… [--min-build N [--lower-min-build]] [--retire P]… [--retire-target T[:P]]… [--allow-triple-drop P=T --reason …] [--valid-until TS] [--out DIR] [--pack-dir DIR | --upload]",
        does: "sign the next index as a delta over the verified head; --upload publishes it",
    },
    Verb {
        name: "mirror",
        synopsis: "mirror <quad-dir | --quad DIR> [--dry-run]",
        does: "copy the staged packs a verified index pins to the public owner",
    },
    Verb {
        name: "publish-group",
        synopsis: "publish-group rustc --seal <dir> [--trust-repo DIR] [--checkout NAME=DIR]… [--out DIR] [--dry-run | --stage-upload | --publish-staged] [--force-same-build] [--allow-build-tree-seal]",
        does: "pack, sign and index the rustc coherence group from one seal; --stage-upload stages it; --publish-staged publishes canaried bytes",
    },
    Verb {
        name: "status",
        synopsis: "status [--quad DIR]",
        does: "print the verified channel head or a local quad",
    },
    Verb {
        name: "lane",
        synopsis: "lane run <alab|linux> [--dry-run] [--sha256 HEX] [--log FILE] [--triple T]… | lane install <alab|linux> [--allow-dirty] [--no-arm] [--keyless] [--host gh|agh-staging|agh] | lane disarm <alab|linux>",
        does: "run, install or disarm an unattended lane (--triple and --keyless: linux only; a run without --dry-run needs its installed unit's --sha256)",
    },
    Verb {
        name: "hosts",
        synopsis: "hosts check [--host agh-staging|agh] [--unit-env] [--keyless]",
        does: "whether this builder's agh.toml is ready for --host agh-staging or agh (reads files, sends nothing)",
    },
    Verb {
        name: "adopt",
        synopsis: crate::adopt::SYNOPSIS,
        does: "write a checkout's sealed trust.lock for a store Trust build on adopt/trust-<N>, judged by TOOLCHAIN_ACCEPT_CMD_DEFAULT when the compiler changes; commits, never pushes",
    },
    Verb {
        name: "agh",
        synopsis: "agh <agh arguments>",
        does: "agh's CLI as this binary: the lanes' git credential helper",
    },
];

/// Binary entry point: exits `0` ok, `1` refused, `2` usage, `3` could not run. A leading
/// `agh` hands the rest to agh's CLI before any context is built, so the lanes' git
/// credential helper (`<snapshot>/atpkg-publish agh auth git-credential get`) runs as agh, its
/// stdout and exit codes unchanged, and reads no atpkg-publish environment.
pub fn main_entry(args: impl IntoIterator<Item = String>) -> ExitCode {
    main_entry_os(args.into_iter().map(OsString::from))
}

/// [`main_entry`] over the arguments as the OS gave them (`std::env::args_os()`), so a
/// non-UTF-8 argument never panics: after a leading `agh` the rest pass to agh's CLI whole,
/// which answers such an argument itself, and atpkg-publish's own verbs refuse one as Usage
/// (exit 2).
pub fn main_entry_os(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let args: Vec<OsString> = args.into_iter().skip(1).collect();
    if let Some(rest) = forwarded(&args) {
        return agh::cli::main_entry(rest);
    }
    match utf8_words(args).and_then(|words| run(&words)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("atpkg-publish: {e}");
            ExitCode::from(e.exit_code())
        }
    }
}

/// The arguments after a first argument `agh`: what the forwarding verb hands agh's CLI.
fn forwarded<A: AsRef<OsStr>>(args: &[A]) -> Option<Vec<OsString>> {
    let (first, rest) = args.split_first()?;
    (first.as_ref() == "agh").then(|| rest.iter().map(|a| a.as_ref().to_os_string()).collect())
}

/// Each argument as UTF-8, or Usage `argument {n} is not UTF-8` (counted from 1, after
/// `argv[0]`).
fn utf8_words(args: Vec<OsString>) -> Result<Vec<String>> {
    args.into_iter()
        .enumerate()
        .map(|(i, arg)| {
            arg.into_string()
                .map_err(|_| Error::usage(format!("argument {} is not UTF-8", i + 1)))
        })
        .collect()
}

/// A leading global `--host VALUE` or `--host=VALUE`, parsed, and the arguments after it. Only
/// a leading one is global, so `lane install … --host` and `hosts check --host` stay the
/// verbs' own.
fn host_flag(args: &[String]) -> Result<(Option<HostChoice>, &[String])> {
    let Some((first, rest)) = args.split_first() else {
        return Ok((None, args));
    };
    let mut rest = rest.iter();
    let Some(value) = HostChoice::flag_value(first, &mut rest) else {
        return Ok((None, args));
    };
    let choice = HostChoice::parse(value)
        .ok_or_else(|| Error::usage(format!("--host takes gh, agh-staging or agh: {value}")))?;
    Ok((Some(choice), rest.as_slice()))
}

/// Run one command line (without `argv[0]`).
///
/// # Errors
///
/// The verb's error; Usage for a bad global `--host`, no verb, a verb [`VERBS`] does not name
/// (before any context is built), or a global `--host` before `hosts`, `agh` or
/// `lane install` (each takes its own, or none).
pub fn run(args: &[String]) -> Result<()> {
    let (choice, args) = host_flag(args)?;
    let Some((verb, rest)) = args.split_first() else {
        eprint!("{}", usage());
        return Err(Error::usage("no verb (atpkg-publish help)"));
    };
    if matches!(verb.as_str(), "help" | "--help" | "-h") {
        print!("{}", usage());
        return Ok(());
    }
    if choice.is_some() {
        match verb.as_str() {
            "hosts" => {
                return Err(Error::usage(
                    "hosts check takes its own --host: atpkg-publish hosts check --host \
                     agh-staging|agh",
                ));
            }
            "agh" => {
                return Err(Error::usage(
                    "agh must come first: atpkg-publish agh <agh arguments>",
                ));
            }
            // The unit records `lane install`'s own --host; a global one would build this
            // command's context alone and arm a `gh` unit.
            "lane" if rest.first().is_some_and(|w| w == "install") => {
                return Err(Error::usage(
                    "lane install takes its own --host: atpkg-publish lane install <alab|linux> \
                     --host gh|agh-staging|agh",
                ));
            }
            _ => {}
        }
    }
    // A mistyped verb is Usage before any context: under `--host agh*` building one reads
    // agh.toml and raises or clears its needs, which a typo must never do.
    if !VERBS.iter().any(|v| v.name == verb.as_str()) {
        return Err(unknown_verb(verb));
    }
    let ctx = if live_identity_lookup(verb, rest) {
        Ctx::from_env_hosts(choice.unwrap_or_default())?
    } else {
        Ctx::from_env_hosts_view(choice.unwrap_or_default())?
    };
    dispatch(&ctx, verb, rest)
}

/// Learning identity is reserved for an explicitly requested publishing run.
/// A plan or read-only preflight uses cached identity even if a later flag is invalid.
fn live_identity_lookup(verb: &str, rest: &[String]) -> bool {
    if rest
        .iter()
        .any(|arg| matches!(arg.as_str(), "--dry-run" | "--plan"))
    {
        return false;
    }
    match verb {
        "upload" | "mirror" => true,
        "publish-group" => rest
            .iter()
            .any(|arg| matches!(arg.as_str(), "--stage-upload" | "--publish-staged")),
        "pack" | "pack-bundle" | "pack-package" | "pack-bottle" | "index" => {
            rest.iter().any(|arg| arg == "--upload")
        }
        "lane" => rest.first().is_some_and(|arg| arg == "run"),
        _ => false,
    }
}

/// Route `verb` against `ctx`.
///
/// # Errors
///
/// The verb's error; Usage for an unknown verb, or for `agh` anywhere but first.
pub fn dispatch(ctx: &Ctx, verb: &str, rest: &[String]) -> Result<()> {
    match verb {
        "canary" => crate::canary::run(ctx, rest),
        "pack" => artifact::pack::run(ctx, rest),
        "pack-bundle" => artifact::bundle::run(ctx, rest),
        "pack-package" => artifact::package::run(ctx, rest),
        "pack-bottle" => artifact::bottle::run(ctx, rest),
        "pack-upstream" => artifact::bottle::upstream::run(ctx, rest),
        "build-local" => artifact::build_local::run(ctx, rest),
        #[cfg(unix)]
        "harness-local" => crate::harness_local::run(ctx, rest),
        "sign" => artifact::sign::run(ctx, rest),
        "upload" => artifact::upload::run(ctx, rest),
        "index" => channel::index::run(ctx, rest),
        "mirror" => channel::mirror::run(ctx, rest),
        "publish-group" => channel::group::run(ctx, rest),
        "status" => channel::status::run(ctx, rest),
        "lane" => lane::run(ctx, rest),
        "hosts" => crate::hosts_check::run(ctx, rest),
        "adopt" => crate::adopt::run(ctx, rest, &|k| std::env::var_os(k)),
        // `main_entry` forwards a leading `agh` before any context; one routed here came
        // after something else.
        "agh" => Err(Error::usage(
            "agh must come first: atpkg-publish agh <agh arguments>",
        )),
        other => Err(unknown_verb(other)),
    }
}

/// Usage `unknown verb `{verb}` — `atpkg-publish help` lists them`.
fn unknown_verb(verb: &str) -> Error {
    Error::usage(format!(
        "unknown verb `{verb}` — `atpkg-publish help` lists them"
    ))
}

/// Top-level help text.
#[must_use]
pub fn usage() -> String {
    use std::fmt::Write as _;
    let mut s = String::from(
        "atpkg-publish — owner-side atpkg producer (never shipped)\n\n\
         usage: atpkg-publish [--host gh|agh-staging|agh] <verb> [args]\n\n",
    );
    for v in VERBS {
        let _ = writeln!(s, "  {}\n      {}", v.synopsis, v.does);
    }
    s.push_str("\nExit: 0 ok, 1 refused, 2 usage, 3 could not run.\n");
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn identity_learning_is_only_for_an_explicit_publishing_run() {
        let words = |args: &[&str]| args.iter().map(ToString::to_string).collect::<Vec<_>>();
        for (verb, args) in [
            ("hosts", vec!["check"]),
            ("mirror", vec!["quad", "--dry-run"]),
            ("publish-group", vec!["rustc", "--dry-run"]),
            ("pack", vec!["ay", "--unsigned"]),
            ("index", vec![]),
            ("status", vec![]),
        ] {
            assert!(!super::live_identity_lookup(verb, &words(&args)));
        }
        assert!(super::live_identity_lookup(
            "upload",
            &words(&["staging", "repo"])
        ));
        assert!(super::live_identity_lookup("index", &words(&["--upload"])));
    }

    use std::ffi::OsString;

    use super::{forwarded, host_flag, run, utf8_words};
    use crate::ctx::HostChoice;
    use crate::error::Error;

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_string()).collect()
    }

    #[test]
    fn host_flag_strips_only_a_leading_choice() {
        let args = words(&["--host", "agh", "lane", "run", "linux"]);
        let (choice, rest) = host_flag(&args).expect("a leading choice");
        assert_eq!(choice, Some(HostChoice::Agh));
        assert_eq!(rest, words(&["lane", "run", "linux"]).as_slice());

        let args = words(&["--host=agh-staging", "status"]);
        let (choice, rest) = host_flag(&args).expect("a leading choice");
        assert_eq!(choice, Some(HostChoice::AghStaging));
        assert_eq!(rest, words(&["status"]).as_slice());

        let args = words(&["status", "--host", "agh"]);
        let (choice, rest) = host_flag(&args).expect("no leading choice");
        assert_eq!(choice, None);
        assert_eq!(rest, args.as_slice());

        assert!(matches!(
            host_flag(&words(&["--host"])),
            Err(Error::Usage(_))
        ));
        match host_flag(&words(&["--host", "github"])) {
            Err(Error::Usage(why)) => assert!(why.contains("github"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn forwarded_takes_a_leading_agh_alone() {
        let os = |args: &[&str]| args.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            forwarded(&words(&["agh", "auth", "git-credential", "get"])),
            Some(os(&["auth", "git-credential", "get"]))
        );
        assert_eq!(forwarded(&words(&["agh"])), Some(Vec::new()));
        assert_eq!(forwarded(&words(&["lane", "run"])), None);
        assert_eq!(forwarded(&words(&["--host", "agh", "agh"])), None);
    }

    #[test]
    fn run_refuses_a_global_host_before_hosts_and_agh() {
        for args in [
            &["--host", "agh", "hosts", "check"][..],
            &["--host", "agh", "agh", "api"][..],
            &["--host", "agh-staging", "lane", "install", "alab"][..],
        ] {
            let e = run(&words(args)).expect_err("a global --host before hosts, agh or install");
            assert!(matches!(e, Error::Usage(_)), "{args:?}: {e}");
            assert_eq!(e.exit_code(), 2, "{args:?}: {e}");
        }
    }

    /// A non-UTF-8 argument is Usage before any verb runs, and one after a leading `agh` is
    /// handed to agh's CLI as the OS gave it.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_argument_is_usage_and_agh_gets_it_whole() {
        use std::os::unix::ffi::OsStringExt as _;

        let bad = OsString::from_vec(b"/repos/\xff".to_vec());
        match utf8_words(vec![OsString::from("status"), bad.clone()]) {
            Err(Error::Usage(why)) => assert_eq!(why, "argument 2 is not UTF-8"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            utf8_words(vec![OsString::from("status")]).expect("UTF-8"),
            words(&["status"])
        );
        assert_eq!(
            forwarded(&[OsString::from("agh"), OsString::from("api"), bad.clone()]),
            Some(vec![OsString::from("api"), bad])
        );
    }
}
