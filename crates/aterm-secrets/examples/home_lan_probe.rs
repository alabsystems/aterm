// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read the automatic LAN admission evidence without changing membership or exposing keys.

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use aterm_secrets::{client::Paths, home_lan};

    let paths = Paths::live()?;
    let facts = home_lan::Facts::live();
    let answers = home_lan::read(&paths);
    let now = aterm_time::unix_millis().ok_or("clock precedes Unix epoch")?;
    let tags = home_lan::here(&facts, &answers, now);
    println!(
        "home={} evidence_rows={} primary_ipv4={} gateway_known={} expensive_or_unknown={}",
        !tags.is_empty(),
        answers.len(),
        facts.primary.is_some(),
        facts.gateway.is_some(),
        facts.expensive,
    );
    for tag in tags {
        println!("home_tag={tag}");
    }
    if let Some(primary) = facts.primary
        && let Some(answer) = answers.iter().find(|answer| answer.addr != primary)
    {
        // A recorded remote address exercises the same admission verdict as a request.
        // No address, identity or gateway hardware value is printed.
        match home_lan::observed(&facts, &answers, primary.into(), answer.addr.into(), now) {
            Ok(_) => println!("recorded_peer=home"),
            Err(why) => println!("recorded_peer=away reason={why}"),
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("home_lan_probe requires Unix vault support");
    std::process::ExitCode::from(2)
}
