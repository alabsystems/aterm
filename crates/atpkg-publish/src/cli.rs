// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Verb dispatch. Each verb parses its own typed flags; [`crate::ctx::Ctx::from_env`]
//! reads the environment.

use std::process::ExitCode;

use crate::ctx::Ctx;
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
        name: "pack",
        synopsis: "pack <program> --checkout <dir> [--triple T] [--build N] [--merge-from PKG] [--toolchain BIN] [--out DIR] [--trust-repo DIR] [--unsigned] [--upload]",
        does: "build, gate, stage and sign one program for one target",
    },
    Verb {
        name: "pack-bundle",
        synopsis: "pack-bundle <program> --sysroot <dir> [--src-repo DIR] [--build N] [--version V] [--triple T] [--hello-sysroot DIR | --no-hello] [--donor-root DIR]… [--keep-target T]… [--merge-from PKG] [--out DIR] [--trust-repo DIR] [--allow-dirty] [--no-smoke] [--no-strip] [--unsigned] [--upload]",
        does: "stage, prove, relocate, gate and sign a sysroot bundle (trust, trust-mc)",
    },
    Verb {
        name: "pack-package",
        synopsis: artifact::package::SYNOPSIS,
        does: "pack, gate and sign a prebuilt package tree (codex-alab)",
    },
    Verb {
        name: "build-local",
        synopsis: "build-local <program> <checkout> [--plan] [--policy repository|unverified] [--profile release|dev] [--jobs N] [--toolchain BIN] [--out DIR] [--link]",
        does: "development build of a program: unsigned, never packed or uploaded",
    },
    Verb {
        name: "sign",
        synopsis: "sign <pkg.toml>… [--trust-repo DIR]",
        does: "sign staged manifests with this machine's key and verify them",
    },
    Verb {
        name: "upload",
        synopsis: "upload <staging|public> <repo> <tag> <file>…",
        does: "replace or add assets on a standing release with that owner's token (the signing ceremony's re-upload)",
    },
    Verb {
        name: "index",
        synopsis: "index [--baseline QUAD-DIR | --first CHANNEL --index-build N] [--pin P=B]… [--target-pin T:P=B[,…]]… [--yank …] [--unyank …] [--min-build N [--lower-min-build]] [--retire P]… [--retire-target T[:P]]… [--allow-triple-drop P=T --reason …] [--valid-until TS] [--out DIR] [--pack-dir DIR | --upload]",
        does: "sign the next index as a delta over the verified head; --upload publishes it",
    },
    Verb {
        name: "mirror",
        synopsis: "mirror <quad-dir | --quad DIR> [--dry-run]",
        does: "copy the staged packs a verified index pins to the public owner",
    },
    Verb {
        name: "publish-group",
        synopsis: "publish-group rustc --seal <dir> [--trust-repo DIR] [--checkout NAME=DIR]… [--out DIR] [--dry-run | --upload] [--force-same-build] [--allow-build-tree-seal]",
        does: "pack, sign and index the rustc coherence group from one seal; --upload publishes it",
    },
    Verb {
        name: "status",
        synopsis: "status [--quad DIR]",
        does: "print the verified channel head or a local quad",
    },
    Verb {
        name: "lane",
        synopsis: "lane run <alab|linux> [--dry-run] [--sha256 HEX] [--log FILE] [--triple T]… | lane install <alab|linux> [--allow-dirty] [--no-arm] [--keyless] | lane disarm <alab|linux>",
        does: "run, install or disarm an unattended lane",
    },
];

/// Binary entry point: exits `0` ok, `1` refused, `2` usage, `3` could not run.
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

/// Run one command line (without `argv[0]`).
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

/// Route `verb` against `ctx`.
pub fn dispatch(ctx: &Ctx, verb: &str, rest: &[String]) -> Result<()> {
    match verb {
        "pack" => artifact::pack::run(ctx, rest),
        "pack-bundle" => artifact::bundle::run(ctx, rest),
        "pack-package" => artifact::package::run(ctx, rest),
        "build-local" => artifact::build_local::run(ctx, rest),
        "sign" => artifact::sign::run(ctx, rest),
        "upload" => artifact::upload::run(ctx, rest),
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

/// Top-level help text.
#[must_use]
pub fn usage() -> String {
    let mut s = String::from(
        "atpkg-publish — owner-side atpkg producer (never shipped)\n\n\
         usage: atpkg-publish <verb> [args]\n\n",
    );
    for v in VERBS {
        s.push_str(&format!("  {}\n      {}\n", v.synopsis, v.does));
    }
    s.push_str("\nExit: 0 ok, 1 refused, 2 usage, 3 could not run.\n");
    s
}
