// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `abrew-admission`: measures native Homebrew formula and cask admission across machines.

use atpkg::abrew::{
    api, census,
    machine::{self, Machine},
};

use crate::{Fail, Res};

const USAGE: &str = "abrew-admission [--json] [--indexes DIR] [--this-machine | --arch A --macos V --xcode V|none --no-clt]";

/// The verb's switches.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    json: bool,
    indexes: Option<String>,
    this_machine: bool,
    arch: Option<String>,
    macos: Option<String>,
    /// `Some(None)`: `--xcode none`.
    xcode: Option<Option<String>>,
    no_clt: bool,
}

fn parse(args: &[String]) -> Result<Args, Fail> {
    let usage = || Fail::usage(format!("usage: {USAGE}"));
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(usage);
        match arg.as_str() {
            "--json" => out.json = true,
            "--indexes" => out.indexes = Some(crate::paths::from_caller(&value()?)),
            "--this-machine" => out.this_machine = true,
            "--arch" => out.arch = Some(value()?),
            "--macos" => out.macos = Some(value()?),
            "--xcode" => out.xcode = Some(Some(value()?).filter(|v| v != "none")),
            "--no-clt" => out.no_clt = true,
            _ => return Err(usage()),
        }
    }
    let modelled = out.arch.is_some() || out.macos.is_some() || out.xcode.is_some() || out.no_clt;
    if out.this_machine && modelled {
        return Err(usage());
    }
    Ok(out)
}

/// The machine `args` model: this one (`None`), the newest Mac, or the
/// newest Mac with the named facts replaced.
fn machine(args: &Args) -> Result<Option<Machine>, Fail> {
    if args.this_machine {
        return Ok(None);
    }
    let newest = Machine::newest_mac();
    let macos = args
        .macos
        .clone()
        .or_else(|| newest.macos_version().map(str::to_owned))
        .unwrap_or_default();
    let xcode = args
        .xcode
        .clone()
        .unwrap_or_else(|| machine::release_xcode(&macos));
    Machine::modelled(
        args.arch.as_deref().unwrap_or(newest.arch()),
        &macos,
        xcode.as_deref(),
        !args.no_clt,
    )
    .map(Some)
    .map_err(|e| Fail::usage(format!("abrew-admission: {e}\nusage: {USAGE}")))
}

fn index(args: &Args, file: &str, casks: bool) -> Result<Vec<u8>, Fail> {
    match &args.indexes {
        Some(dir) => {
            let path = std::path::Path::new(dir).join(file);
            std::fs::read(&path)
                .map_err(|e| Fail::die(format!("abrew-admission: {}: {e}", path.display())))
        }
        None => api::fetch_index(casks).map_err(|e| Fail::die(format!("abrew-admission: {e}"))),
    }
}

pub fn cmd_abrew_admission(args: &[String]) -> Res {
    let args = parse(args)?;
    let modelled = machine(&args)?;
    let formulae = index(&args, "formula.json", false)?;
    let casks = index(&args, "cask.json", true)?;
    let judged: &Machine = match &modelled {
        Some(modelled) => modelled,
        None => Machine::this(),
    };
    let census = census::measure(&formulae, &casks, judged)
        .map_err(|e| Fail::die(format!("abrew-admission: {e}")))?;
    if args.json {
        println!(
            "{}",
            aterm_json::to_string(&census.json())
                .map_err(|e| Fail::die(format!("abrew-admission: {e}")))?
        );
    } else {
        print!("{}", census.text());
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Result<Args, Fail> {
        parse(&words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>())
    }

    fn judged(words: &[&str]) -> Result<Option<Machine>, Fail> {
        machine(&args(words)?)
    }

    #[test]
    fn the_default_machine_is_the_newest_mac_and_flags_replace_its_facts() {
        let newest = judged(&[]).unwrap().unwrap();
        assert_eq!(newest.describe(), Machine::newest_mac().describe());
        let intel = judged(&["--arch", "x86_64", "--macos", "15.5", "--no-clt"])
            .unwrap()
            .unwrap();
        assert_eq!(
            intel.describe(),
            "x86_64, macOS 15.5, Xcode 16.0, no Command Line Tools"
        );
        let no_xcode = judged(&["--xcode", "none"]).unwrap().unwrap();
        assert_eq!(no_xcode.xcode_version(), None);
        assert!(judged(&["--this-machine"]).unwrap().is_none());
    }

    /// The lab works from `/`, so a relative `--indexes` is the caller's.
    #[test]
    fn a_relative_index_directory_is_the_caller_s() {
        let caller = crate::paths::caller_dir();
        assert_eq!(
            args(&["--indexes", "idx"]).unwrap().indexes,
            Some(format!("{}/idx", caller.trim_end_matches('/')))
        );
        assert_eq!(
            args(&["--indexes", "/srv/idx"]).unwrap().indexes.as_deref(),
            Some("/srv/idx")
        );
    }

    #[test]
    fn usage_errors_exit_two() {
        for bad in [
            &["--bogus"][..],
            &["--indexes"],
            &["--this-machine", "--arch", "arm64"],
            &["--macos", "garbage"],
            &["--xcode", "26.x"],
            &["--arch", "aarch64"],
            &["--arch", "mips"],
        ] {
            assert_eq!(judged(bad).unwrap_err().code, 2, "{bad:?}");
        }
    }
}
