// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The verb dispatch. Each verb parses its own typed flags in its own module; the only
//! environment knobs are what [`crate::ctx::Ctx::from_env`] reads.

use std::process::ExitCode;

use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::{artifact, channel, lane};

/// One verb: its name, its synopsis, and what it does.
#[derive(Clone, Copy, Debug)]
pub struct Verb {
    /// The verb.
    pub name: &'static str,
    /// Its synopsis.
    pub synopsis: &'static str,
    /// What it does, in one line.
    pub does: &'static str,
}

/// Every verb, in the order `help` lists them.
pub const VERBS: &[Verb] = &[
    Verb {
        name: "pack",
        synopsis: "pack <program> --checkout <dir> [--triple T] [--build N] [--merge-from PKG] [--toolchain BIN] [--out DIR] [--trust-repo DIR] [--unsigned] [--upload]",
        does: "build, gate, stage and sign one program build for one target",
    },
    Verb {
        name: "pack-bundle",
        synopsis: "pack-bundle <program> --sysroot <dir> [--src-repo DIR] [--build N] [--version V] [--triple T] [--hello-sysroot DIR | --no-hello] [--donor-root DIR]… [--keep-target T]… [--merge-from PKG] [--out DIR] [--trust-repo DIR] [--allow-dirty] [--no-smoke] [--no-strip] [--unsigned] [--upload]",
        does: "stage, prove, relocate, gate and sign a sysroot bundle (trust, trust-mc)",
    },
    Verb {
        name: "build-local",
        synopsis: "build-local <program> <checkout> [--plan] [--policy repository|unverified] [--profile release|dev] [--jobs N] [--toolchain BIN] [--out DIR] [--link]",
        does: "the development build of a program, with no pack or signature",
    },
    Verb {
        name: "sign",
        synopsis: "sign <pkg.toml>… [--trust-repo DIR]",
        does: "sign staged manifests with this machine's key, then verify them",
    },
    Verb {
        name: "index",
        synopsis: "index [--baseline QUAD-DIR | --first CHANNEL --index-build N] [--pin P=B]… [--target-pin T:P=B[,…]]… [--yank …] [--unyank …] [--min-build N [--lower-min-build]] [--retire P]… [--retire-target T[:P]]… [--allow-triple-drop P=T --reason …] [--valid-until TS] [--out DIR] [--upload]",
        does: "publish the next signed index: a typed delta over the verified head",
    },
    Verb {
        name: "mirror",
        synopsis: "mirror <quad-dir> | --quad DIR [--dry-run]",
        does: "make exactly the signed packs an index pins public",
    },
    Verb {
        name: "publish-group",
        synopsis: "publish-group rustc --seal <dir> [--trust-repo DIR] [--checkout NAME=DIR]… [--out DIR] [--dry-run | --upload] [--force-same-build] [--allow-build-tree-seal]",
        does: "publish the rustc coherence group from one seal",
    },
    Verb {
        name: "status",
        synopsis: "status [--quad DIR]",
        does: "the verified channel head, or a quad: pins, overlays, yanks, floors, horizon",
    },
    Verb {
        name: "lane",
        synopsis: "lane run <alab|linux> [--dry-run] [--sha256 HEX] [--log FILE] [--triple T]… | lane install <alab|linux> [--allow-dirty] [--no-arm] [--keyless] | lane disarm <alab|linux>",
        does: "the unattended lanes: run, install a snapshot under a unit, disarm",
    },
];

/// The binary's entry point.
pub fn main_entry(args: impl IntoIterator<Item = String>) -> ExitCode {
    let args: Vec<String> = args.into_iter().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("atpkg-publish: {e}");
            ExitCode::from(e.exit_code())
        }
    }
}

/// Run one command line (without the program name).
///
/// # Errors
///
/// Whatever the verb answers; a usage error for an unknown verb.
pub fn run(args: &[String]) -> Result<()> {
    let Some((verb, rest)) = args.split_first() else {
        eprint!("{}", usage());
        return Err(Error::usage("no verb (atpkg-publish help)"));
    };
    if matches!(verb.as_str(), "help" | "--help" | "-h") {
        print!("{}", usage());
        return Ok(());
    }
    let ctx = Ctx::from_env()?;
    dispatch(&ctx, verb, rest)
}

/// Route one verb against `ctx`.
///
/// # Errors
///
/// Whatever the verb answers; a usage error for an unknown verb.
pub fn dispatch(ctx: &Ctx, verb: &str, rest: &[String]) -> Result<()> {
    match verb {
        "pack" => artifact::pack::run(ctx, rest),
        "pack-bundle" => artifact::bundle::run(ctx, rest),
        "build-local" => artifact::build_local::run(ctx, rest),
        "sign" => artifact::sign::run(ctx, rest),
        "index" => channel::index::run(ctx, rest),
        "mirror" => channel::mirror::run(ctx, rest),
        "publish-group" => channel::group::run(ctx, rest),
        "status" => channel::status::run(ctx, rest),
        "lane" => lane::run(ctx, rest),
        other => Err(Error::usage(format!(
            "unknown verb `{other}` — `atpkg-publish help` lists them"
        ))),
    }
}

/// The help text.
#[must_use]
pub fn usage() -> String {
    let mut s = String::from(
        "atpkg-publish — the atpkg producer (owner-side; never shipped)\n\n\
         usage: atpkg-publish <verb> [args]\n\n",
    );
    for v in VERBS {
        s.push_str(&format!("  {}\n      {}\n", v.synopsis, v.does));
    }
    s
}
