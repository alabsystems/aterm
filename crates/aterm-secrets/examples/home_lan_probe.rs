// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read the home ALab LAN evidence a member's desk admits a proving Mac by, without changing
//! membership or exposing keys: the network place's proof under this process's state root
//! (`aterm_netplace::proof::read`), as the desk's `home_lan::home_net` reads it.

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use aterm_secrets::home_lan;

    let state = aterm_types::dirs::state_dir().ok_or("no state root")?;
    let now = aterm_netplace::proof::read(&state);
    let home = home_lan::home_net_at(&now);
    let route = now.facts.route.as_ref();
    println!(
        "home={} evidence_rows={} primary_ipv4={} gateway_known={} expensive_or_unknown={}",
        home.is_ok(),
        now.answers.len(),
        route.is_some(),
        route.is_some_and(|route| route.gateway_hw.is_some()),
        now.facts.path.as_ref().is_none_or(|path| path.expensive),
    );
    // No address, identity or gateway hardware value is printed.
    if let Err(why) = home {
        println!("reason={why}");
    }
    Ok(())
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("home_lan_probe requires Unix vault support");
    std::process::ExitCode::from(2)
}
