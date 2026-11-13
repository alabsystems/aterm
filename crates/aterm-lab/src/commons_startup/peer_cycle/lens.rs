// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Existing native-lens own-presentation readback, separate from network evidence.

#[derive(Clone, Copy)]
pub(super) enum Target {
    Overview,
    Peer,
}

impl Target {
    fn selection(self) -> &'static str {
        match self {
            Self::Overview => "NetworkState:Global:Overview",
            Self::Peer => "NetworkState:Global:Peer(1)",
        }
    }
}

pub(super) struct State {
    pub window: u64,
    desired: String,
    presented: String,
    pending: bool,
}

impl State {
    pub fn matches_desired(&self, target: Target, window: Option<u64>) -> bool {
        window.is_none_or(|window| self.window == window) && self.desired == target.selection()
    }

    pub fn matches(&self, target: Target, window: Option<u64>) -> bool {
        self.matches_desired(target, window)
            && self.presented == target.selection()
            && !self.pending
    }
}

pub(super) fn parse(chrome: &str) -> Result<State, String> {
    let mut rows = chrome
        .lines()
        .filter(|line| line.starts_with("stats-lens "));
    let line = rows.next().ok_or("missing native stats-lens readback")?;
    if rows.next().is_some() {
        return Err("peer-cycle expects exactly one native window lens".into());
    }
    let fields: Vec<_> = line.split_whitespace().collect();
    let ["stats-lens", window, desired, presented, pending] = fields.as_slice() else {
        return Err("invalid native stats-lens readback".into());
    };
    let value = |field: &str, prefix: &str| {
        field
            .strip_prefix(prefix)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("invalid native stats-lens {prefix}"))
    };
    let window = value(window, "window=")?
        .parse()
        .map_err(|_| "invalid native lens window")?;
    let pending = match value(pending, "pending=")?.as_str() {
        "0" => false,
        "1" => true,
        _ => return Err("invalid native lens pending state".into()),
    };
    Ok(State {
        window,
        desired: value(desired, "desired=")?,
        presented: value(presented, "presented=")?,
        pending,
    })
}

pub(super) fn two_keys_posted(reply: &str) -> bool {
    let Some(line) = reply.lines().next() else {
        return false;
    };
    let fields: Vec<_> = line.split_whitespace().collect();
    matches!(fields.as_slice(), ["OK", "posted=2", "key=1"])
}

#[cfg(test)]
#[path = "lens_tests.rs"]
mod tests;
