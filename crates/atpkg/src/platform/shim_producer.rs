// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Whole-body producer recognition, before any consumer-specific policy projection.

use super::{MAX_SHIM_BYTES, parse_sh_shim_env, sh_exec_target, sh_shim_body, sh_shim_quote};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Direct,
    Factory,
}

pub(super) struct Producer {
    pub(super) kind: Kind,
    pub(super) target: PathBuf,
    pub(super) env: crate::shim_env::ShimEnv,
    pub(super) route: Option<PathBuf>,
    pub(super) launch: Option<PathBuf>,
    pub(super) program: String,
    pub(super) prefix: PathBuf,
    pub(super) prelude: String,
    pub(super) atpkg: Option<PathBuf>,
    pub(super) token_route: bool,
}

pub(super) fn recognize(wrapper: &Path, content: &str) -> Option<Producer> {
    if content.len() > MAX_SHIM_BYTES || !wrapper.is_absolute() {
        return None;
    }
    let target = sh_exec_target(content.lines().rev().find(|line| !line.trim().is_empty())?)?;
    if !target.is_absolute() {
        return None;
    }
    let env = parse_sh_shim_env(content);
    let launch = match content
        .lines()
        .find_map(|line| line.strip_prefix("export ATPKG_MANAGED_LAUNCH_PATH="))
    {
        Some(path) => Some(sh_exec_target(&format!("exec {path} \"$@\""))?),
        None => None,
    };
    let route = content
        .lines()
        .filter(|line| line.starts_with("[ ! -h "))
        .find_map(|line| sh_exec_target(&format!("exec {}", line.rsplit_once(" && exec ")?.1)));
    let legacy = format!("#!/bin/sh\nexec {} \"$@\"\n", sh_shim_quote(&target));
    let atpkg = crate::stub::sh_named_atpkg(content, "__atpkg");
    // Aliases can differ from the program name; every candidate still requires a whole-body match.
    for (program, prefix) in contexts(wrapper, &target) {
        let verbs = crate::selfupdate::verbs_of(&program);
        if ["--help", "--version"]
            .iter()
            .any(|word| verbs.contains(word))
        {
            continue;
        }
        let mut prelude = super::twin_prelude_with_launcher(
            &program,
            &prefix,
            atpkg.as_deref(),
            verbs,
            &crate::yolo::policy_of(&program),
            crate::native_defaults::Launcher::recorded(content).as_ref(),
        );
        prelude.push_str(&crate::launch_tap::sh_prelude(&program, atpkg.as_deref()));
        let rendered = sh_shim_body(&target, &env, route.as_deref(), launch.as_deref(), &prelude);
        let token_route = content != rendered
            && content
                == crate::launch_tap::with_token_block(
                    &program,
                    rendered.clone(),
                    atpkg.as_deref(),
                );
        if content == rendered || token_route {
            return Some(Producer {
                kind: Kind::Factory,
                target,
                env,
                route,
                launch,
                program,
                prefix,
                prelude,
                atpkg,
                token_route,
            });
        }
    }
    if content == sh_shim_body(&target, &env, route.as_deref(), launch.as_deref(), "")
        || (env.is_empty() && launch.is_none() && route.is_none() && content == legacy)
    {
        return Some(Producer {
            kind: Kind::Direct,
            target,
            env,
            route,
            launch,
            program: String::new(),
            prefix: PathBuf::new(),
            prelude: String::new(),
            atpkg,
            token_route: false,
        });
    }
    None
}

fn contexts(wrapper: &Path, target: &Path) -> Vec<(String, PathBuf)> {
    let mut contexts = Vec::new();
    for wrapper in [
        Some(wrapper.to_path_buf()),
        std::fs::canonicalize(wrapper).ok(),
    ]
    .into_iter()
    .flatten()
    {
        if let Some((program, prefix)) = wrapper
            .file_name()
            .and_then(|name| name.to_str())
            .zip(wrapper.parent().and_then(Path::parent))
        {
            contexts.push((program.to_string(), prefix.to_path_buf()));
        }
    }
    for store in target
        .ancestors()
        .filter(|path| path.file_name().is_some_and(|name| name == "store"))
    {
        if let Some((program, prefix)) = target
            .strip_prefix(store)
            .ok()
            .and_then(|rest| rest.components().next())
            .and_then(|part| part.as_os_str().to_str())
            .zip(store.parent())
        {
            contexts.push((program.to_string(), prefix.to_path_buf()));
        }
    }
    contexts
}
