// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Project one signed default-selected unit onto the ordinary group transaction.
//! The independent whole-toolset pass still owns its stamps, stubs and promises.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

fn same_source(
    captured: &crate::config::PackagesConfig,
    now: &crate::config::PackagesConfig,
    program: &str,
) -> bool {
    captured.account == now.account
        && captured.prefix == now.prefix
        && captured.links.get(program) == now.links.get(program)
}

pub(super) fn allowed(
    layout: &crate::store::Layout,
    cfg: &crate::config::PackagesConfig,
    program: &str,
) -> Result<(), String> {
    if cfg.unreadable {
        return Err("package choices cannot be read".into());
    }
    if !cfg.enabled() || !cfg.installs_unattended() {
        return Err("automatic package installation is off".into());
    }
    if cfg.exclude().iter().any(|name| name == program) {
        return Err(format!("{program} is excluded in package settings"));
    }
    if cfg.links.get(program).is_some_and(|value| {
        matches!(
            crate::classify_link(value, aterm_types::dirs::home_dir().as_deref()),
            crate::LinkTarget::Checkout(_)
        )
    }) {
        return Err(format!(
            "{program} is managed from its development checkout"
        ));
    }
    if layout
        .removed_programs()
        .map_err(|why| why.to_string())?
        .contains(program)
    {
        return Err(format!("{program} was removed on this machine"));
    }
    if let Some(why) = crate::adoption::unattended_install_off(layout, cfg) {
        return Err(why.into());
    }
    Ok(())
}

pub(super) fn run(
    layout: &crate::store::Layout,
    initial: &crate::config::PackagesConfig,
    fetcher: &dyn crate::flow::Fetcher,
    installed: &BTreeMap<String, u64>,
    program: &str,
    seams: &PassSeams<'_>,
) -> u8 {
    let read = || match seams.config.as_deref() {
        Some(path) => crate::config::load_live(path)
            .ok_or_else(|| "package choices cannot be read".to_owned()),
        None => Ok(initial.clone()),
    };
    if let Err(why) = read().and_then(|cfg| {
        allowed(layout, &cfg, program)?;
        if !same_source(initial, &cfg, program) {
            return Err("the selected package source changed".into());
        }
        Ok(())
    }) {
        println!("atpkg: {program} left unchanged: {why}");
        return 0;
    }
    let index = match crate::resolve_verified_index(
        fetcher,
        layout,
        &(seams.anchor)(layout),
        build_floor(layout),
        (seams.now)(),
    ) {
        Ok(index) => index,
        Err(why) => return update_pass_failed(layout, &why),
    };
    let Some(channel) = index.channel_for(crate::config::CHANNEL, current_triple()) else {
        eprintln!("atpkg: {program}: the selected signed channel is unavailable");
        return 1;
    };
    let Some(group) = crate::apply::plan_groups_here(layout, &index, &channel, current_triple())
        .into_iter()
        .find(|group| group.members.iter().any(|member| member == program))
    else {
        eprintln!("atpkg: {program}: no signed default-selected unit serves this machine");
        return 1;
    };
    let wanted_now = || -> Result<BTreeSet<String>, String> {
        let now = read()?;
        for member in &group.members {
            allowed(layout, &now, member)?;
            if crate::vendor_direct::is_vendor(member)
                && !crate::alab::source_in(layout, &index, &channel, member, current_triple())
                    .index_lane()
            {
                return Err("the selected package source changed".into());
            }
        }
        if group
            .members
            .iter()
            .any(|member| !same_source(initial, &now, member))
        {
            return Err("the selected package source changed".into());
        }
        let mut wanted = wanted_programs(&index, &now, current_triple());
        for removed in layout.removed_programs().map_err(|why| why.to_string())? {
            wanted.remove(&removed);
        }
        if !wanted.contains(program) {
            return Err("the program is not selected for automatic installation".into());
        }
        if group
            .members
            .iter()
            .any(|member| crate::linkmode::is_linked(layout, member))
        {
            return Err("the selected unit has a development link; it stays unchanged".into());
        }
        Ok(wanted)
    };
    if let Err(why) = wanted_now() {
        println!("atpkg: {program} left unchanged: {why}");
        return 0;
    }
    let mut installed = installed.clone();
    for member in &group.members {
        let wanted = match wanted_now() {
            Ok(wanted) => wanted,
            Err(why) => {
                println!("atpkg: {program} left unchanged: {why}");
                return 0;
            }
        };
        if !wanted.contains(member) {
            continue;
        }
        let Some(path) = system_satisfying(layout, &index, member, seams.path.as_deref()) else {
            continue;
        };
        if let Err(why) = wanted_now() {
            println!("atpkg: {program} left unchanged: {why}");
            return 0;
        }
        satisfy_by_system(layout, member, &path, &mut installed);
        if member == program {
            return 0;
        }
    }
    let current = || {
        let mut wanted = wanted_now()?;
        for member in &group.members {
            if system_satisfying(layout, &index, member, seams.path.as_deref()).is_some() {
                wanted.remove(member);
            }
        }
        if !wanted.contains(program)
            || group
                .members
                .iter()
                .any(|member| !wanted.contains(member) && !installed.contains_key(member))
        {
            return Err("the selected unit includes a package left out on this machine".into());
        }
        Ok(())
    };
    if let Err(why) = current() {
        println!("atpkg: {program} left unchanged: {why}");
        return 0;
    }
    let flip = crate::quiet::FlipGate {
        policy: (seams.flip_policy)(),
        now: (seams.now)(),
        running: seams.running,
    };
    let mut assets = BTreeMap::new();
    let (outcome, applied) = match crate::flow::bootstrap_group_guarded(
        fetcher,
        layout,
        &index,
        crate::config::CHANNEL,
        current_triple(),
        &group,
        &installed,
        &mut assets,
        &flip,
        &current,
    ) {
        Ok(result) => result,
        Err(why) => return update_pass_failed(layout, &why),
    };
    advance_floors(layout, index.index_build, index.roster_seq());
    let report = crate::flow::ChannelApplyReport {
        index_build: index.index_build,
        roster_seq: index.roster_seq(),
        groups: vec![(group, outcome)],
        applied,
        skipped_linked: Vec::new(),
        resolved_assets: assets,
    };
    let failures = report_channel_apply(layout, &installed, &report, &[], false);
    let warmed: BTreeSet<String> = report.applied.keys().cloned().collect();
    (seams.warm_agents)(layout, &warmed);
    u8::from(failures != 0)
}
