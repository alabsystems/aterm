// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only accounts UI against the real package and production native caller proof.
//! The only account labels live in the private native roster, never a model response.

use super::*;

fn roster(home: &Path, labels: &[String]) -> Result<String, Fail> {
    use aterm_agent::harness::accounts::{Cli, Roster};
    let text = labels
        .iter()
        .map(|label| {
            let dir = home.join(".aterm/logins/codex").join(label);
            let dir = dir
                .to_str()
                .ok_or_else(|| Fail::die("scratch login path is not UTF-8"))?;
            Ok(format!(
                "[[account]]\nlabel = {}\nclass = \"dedicated\"\ndir = {}\n\n",
                aterm_toml::edit::Value::from(label.as_str()),
                aterm_toml::edit::Value::from(dir)
            ))
        })
        .collect::<Result<String, Fail>>()?;
    Roster::parse_for(Cli::Codex, &text).map_err(Fail::die)?;
    Ok(text)
}

fn model_requests(proof: &Proof) -> Result<usize, Fail> {
    let log = proof
        .api_log
        .lock()
        .map_err(|_| Fail::die("model journal poisoned"))?;
    let (requests, _) = log.snapshot().map_err(|e| Fail::die(e.to_string()))?;
    Ok(requests.len())
}

fn scene(proof: &mut Proof) -> Result<(), Fail> {
    let mut random = [0u8; 8];
    aterm_uds::rand::fill(&mut random).map_err(|e| Fail::die(e.to_string()))?;
    let nonce = u64::from_ne_bytes(random);
    let labels = [
        format!("pool-a-{nonce:016x}"),
        format!("pool-b-{nonce:016x}"),
    ];
    let initial = roster(&proof.h, &labels)?;
    crate::util::atomic_write_mode(
        &proof._world.cfg.join("aterm/accounts-codex.toml"),
        initial.as_bytes(),
        0o600,
    )
    .map_err(|e| Fail::die(e.to_string()))?;
    proof.link(&proof.s.join("pkg-a"))?;
    proof.inst.await_socket(Duration::from_secs(60))?;
    let ready = proof.inst.ctl(&["ready"]);
    if ready.code != 0 {
        return Err(Fail::die("private host did not accept ready"));
    }
    let leg = proof.start_leg("accounts", None, "", Folder::Plain, Some("off"))?;
    let ready = Proof::wait_for(Duration::from_secs(60), Duration::from_millis(250), || {
        composer_row("codex", &proof.on(&leg.sid, &["text"]).stdout).is_some()
    })?;
    if !proof.check(
        "official Codex package reached its composer with Autopilot off",
        ready,
    ) {
        return Err(Fail::die("accounts composer did not appear"));
    }
    let before = model_requests(proof)?;
    let typed = proof.on(&leg.sid, &["send", "/accounts"]);
    let entered = proof.on(&leg.sid, &["key", "enter"]);
    if typed.code != 0 || entered.code != 0 {
        return Err(Fail::die("could not open the accounts view"));
    }
    let mut seen = [false; 2];
    let observed = Proof::wait_for(Duration::from_secs(60), Duration::from_millis(250), || {
        let text = proof.on(&leg.sid, &["text"]).stdout;
        for (found, label) in seen.iter_mut().zip(&labels) {
            *found |= text.contains(label);
        }
        if !seen[1] && seen[0] {
            let _ = proof.on(&leg.sid, &["key", "down"]);
        }
        seen.iter().all(|found| *found)
    })?;
    proof.check(
        "strict native accounts view rendered both private random roster rows",
        observed,
    );
    let refresh_label = format!("pool-c-{nonce:016x}");
    let updated = roster(
        &proof.h,
        &[labels[0].clone(), labels[1].clone(), refresh_label.clone()],
    )?;
    crate::util::atomic_write_mode(
        &proof._world.cfg.join("aterm/accounts-codex.toml"),
        updated.as_bytes(),
        0o600,
    )
    .map_err(|e| Fail::die(e.to_string()))?;
    let refreshed = proof.on(&leg.sid, &["key", "enter"]);
    let refresh_observed =
        Proof::wait_for(Duration::from_secs(60), Duration::from_millis(250), || {
            let text = proof.on(&leg.sid, &["text"]).stdout;
            if text.contains(&refresh_label) {
                true
            } else {
                let _ = proof.on(&leg.sid, &["key", "down"]);
                false
            }
        })?;
    proof.check(
        "accounts refresh rendered the changed private native roster",
        refreshed.code == 0 && refresh_observed,
    );
    let closed = proof.on(&leg.sid, &["key", "escape"]);
    let back = Proof::wait_for(Duration::from_secs(60), Duration::from_millis(100), || {
        composer_row("codex", &proof.on(&leg.sid, &["text"]).stdout).is_some()
    })?;
    proof.check(
        "accounts view closes back to the composer",
        closed.code == 0 && back,
    );
    proof.check(
        "accounts open/refresh/close made no model Responses requests",
        model_requests(proof)? == before,
    );
    Ok(())
}

/// One accounts scene, using the existing sandboxed package rig; no reductions or login.
pub fn cmd_codex_accounts_proof(args: &[String]) -> Res {
    stop::install();
    let mut args = args.to_vec();
    let aterm = instance::take_value(&mut args, "--aterm")?
        .ok_or_else(|| Fail::usage("codex-accounts-proof requires --aterm PATH"))?;
    let given = instance::take_value(&mut args, "--package")?
        .ok_or_else(|| Fail::usage("codex-accounts-proof requires --package DIR"))?;
    let keep = instance::take_flag(&mut args, "--keep");
    if !args.is_empty() {
        return Err(Fail::usage("codex-accounts-proof: unexpected arguments"));
    }
    let aterm = instance::resolve_aterm(Some(aterm))?;
    let home = crate::paths::paths().need_store_home()?;
    let pkg = package(Some(&given), home)?;
    let grid = ["--columns", "120", "--lines", "40"]
        .map(str::to_string)
        .to_vec();
    let mut proof = start(aterm, (&pkg, None), keep, (grid, ""))?;
    let result = scene(&mut proof);
    // The existing rig owns every PTY, child, connection and private instance cleanup.
    proof.end_all();
    result?;
    Ok(i32::from(!proof.fails.is_empty()))
}
