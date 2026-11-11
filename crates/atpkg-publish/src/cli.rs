// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Verb dispatch. Each verb parses its own typed flags; [`crate::ctx::Ctx::from_env_hosts`]
//! reads the environment and builds the selected hosts. Linux scoped checks and installs
//! parse their local host choice before context construction; other host operations use a
//! leading `--host` (`gh` by default).

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
        name: "toolchain",
        synopsis: crate::project_toolchain::SYNOPSIS,
        does: "run a project's controller under one authenticated, leased sealed compiler",
    },
    Verb {
        name: "canary",
        synopsis: crate::canary::SYNOPSIS,
        does: "install and execute the signed staged Trust pack, or stage its existing signed receipt without changing the index",
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
        synopsis: "status [--quad DIR] [--releases [PRODUCT…]]",
        does: "print the verified index, or human release versions, UTC dates, platforms and verification status",
    },
    Verb {
        name: "lane",
        synopsis: "lane stage linux --reuse-package PKG… | lane run <alab|linux> [--dry-run] [--staged-only [--queued]] [--json] [--sha256 HEX] [--log FILE] [--triple T]… | lane install <alab|linux> [--staged-only] [--allow-dirty] [--no-arm] [--keyless] [--host gh|agh-staging|agh] | lane disarm <alab|linux>",
        does: "stage retained GNU bottles unsigned, or run/install/disarm a lane (a run without --dry-run needs its installed unit's --sha256)",
    },
    Verb {
        name: "hosts",
        synopsis: "hosts check [--host agh-staging|agh] [--unit-env] [--keyless] [--lane alab|linux]",
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
    // A selected controller uses the same native explicit-target admission hop as installed
    // atpkg. No shim/cwd selection, host identity, or publisher context belongs in this hop.
    if args.first().is_some_and(|word| word == "__build_admit") {
        if let Err(error) = crate::compiler_context::admit_hop(&args[1..]) {
            eprintln!("atpkg-publish: {error}");
            return ExitCode::from(error.exit_code());
        }
        return atpkg::build_admit::run_hidden(&args[1..]);
    }
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

/// An explicit raw-seal component selection after the optional global host.
fn frontend_flag(args: &[String]) -> Result<(Option<String>, &[String])> {
    component_flag(args, "--frontend-component")
}

fn component_flag<'a>(args: &'a [String], flag: &str) -> Result<(Option<String>, &'a [String])> {
    let Some((first, rest)) = args.split_first() else {
        return Ok((None, args));
    };
    let (value, rest) = if first == flag {
        let (value, rest) = rest
            .split_first()
            .ok_or_else(|| Error::usage(format!("{flag} needs its receipt SHA256")))?;
        (value.as_str(), rest)
    } else if let Some(value) = first
        .strip_prefix(flag)
        .and_then(|suffix| suffix.strip_prefix('='))
    {
        (value, rest)
    } else {
        return Ok((None, args));
    };
    if !aterm_codec::hex::is_lower(value, 64) {
        return Err(Error::usage(format!("{flag} needs a lowercase SHA256")));
    }
    Ok((Some(value.to_owned()), rest))
}

fn component_flags(mut args: &[String]) -> Result<(Option<String>, Option<String>, &[String])> {
    let mut frontend = None;
    let mut targo = None;
    loop {
        let (selected, rest) = frontend_flag(args)?;
        if let Some(selected) = selected {
            if frontend.replace(selected).is_some() {
                return Err(Error::usage("duplicate --frontend-component"));
            }
            args = rest;
            continue;
        }
        let (selected, rest) = component_flag(args, "--targo-component")?;
        if let Some(selected) = selected {
            if targo.replace(selected).is_some() {
                return Err(Error::usage("duplicate --targo-component"));
            }
            args = rest;
            continue;
        }
        break;
    }
    if targo.is_some() && frontend.is_none() {
        return Err(Error::usage(
            "--targo-component requires --frontend-component",
        ));
    }
    Ok((frontend, targo, args))
}

/// Run one command line (without `argv[0]`).
///
/// # Errors
///
/// The verb's error; Usage for a bad global `--host`, no verb, a verb [`VERBS`] does not name
/// (before any context is built), or a global `--host` before `hosts`, `agh` or
/// `lane install` (each takes its own, or none).
pub fn run(args: &[String]) -> Result<()> {
    run_with_context(args, |choice, learn| {
        if learn {
            Ctx::from_env_hosts(choice)
        } else {
            Ctx::from_env_hosts_view(choice)
        }
    })
}

fn run_with_context(
    args: &[String],
    context: impl FnOnce(HostChoice, bool) -> Result<Ctx>,
) -> Result<()> {
    let (choice, args) = host_flag(args)?;
    let (frontend_component, targo_component, args) = component_flags(args)?;
    let Some((verb, rest)) = args.split_first() else {
        eprint!("{}", usage());
        return Err(Error::usage("no verb (atpkg-publish help)"));
    };
    if matches!(verb.as_str(), "help" | "--help" | "-h") {
        if frontend_component.is_some() || targo_component.is_some() {
            return Err(Error::usage(
                "--frontend-component selects pack, pack-bundle, or publish-group rustc",
            ));
        }
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
    if (frontend_component.is_some() || targo_component.is_some())
        && !(matches!(verb.as_str(), "pack" | "pack-bundle")
            || verb == "publish-group" && rest.first().is_some_and(|v| v == "rustc"))
    {
        return Err(Error::usage(
            "--frontend-component selects pack, pack-bundle, or publish-group rustc",
        ));
    }
    let selected = match verb.as_str() {
        "hosts" => crate::hosts_check::context_host(rest)?,
        "lane" if rest.first().is_some_and(|w| w == "install") => {
            let kind = rest
                .get(1)
                .ok_or_else(|| Error::usage("lane install needs alab or linux"))?;
            let kind = crate::lane::LaneKind::parse(kind)?;
            let flags = crate::lane::install::InstallFlags::parse(&rest[2..])?;
            if kind == crate::lane::LaneKind::Linux {
                flags.host
            } else {
                HostChoice::Gh
            }
        }
        _ => choice.unwrap_or_default(),
    };
    let mut ctx = context(selected, live_identity_lookup(verb, rest))?;
    ctx.frontend_component = frontend_component;
    ctx.targo_component = targo_component;
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
        "toolchain" => crate::project_toolchain::run(ctx, rest),
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
         usage: atpkg-publish [--host gh|agh-staging|agh] [--frontend-component SHA256] [--targo-component SHA256] <verb> [args]\n\n",
    );
    for v in VERBS {
        let _ = writeln!(s, "  {}\n      {}", v.synopsis, v.does);
    }
    s.push_str("\nExit: 0 ok, 1 refused, 2 usage, 3 could not run.\n");
    s
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
