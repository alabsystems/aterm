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
mod tests {
    use super::*;

    #[test]
    fn acquiring_a_desired_target_does_not_acknowledge_its_presentation() {
        let overview = Target::Overview.selection();
        for (presented, pending) in [
            ("unknown", 0),
            ("closed", 0),
            (Target::Peer.selection(), 0),
            (overview, 1),
        ] {
            let state = parse(&format!(
                "stats-lens window=7 desired={overview} presented={presented} pending={pending}\n"
            ))
            .unwrap();
            assert!(state.matches_desired(Target::Overview, None));
            assert!(state.matches_desired(Target::Overview, Some(7)));
            assert!(!state.matches(Target::Overview, Some(7)));
            assert!(!state.matches_desired(Target::Peer, Some(7)));
            assert!(!state.matches_desired(Target::Overview, Some(8)));
        }
        let ready = parse(&format!(
            "stats-lens window=7 desired={overview} presented={overview} pending=0\n"
        ))
        .unwrap();
        assert!(ready.matches_desired(Target::Overview, Some(7)));
        assert!(ready.matches(Target::Overview, Some(7)));
        let wrong_desired = parse(&format!(
            "stats-lens window=7 desired=NetworkState:Global:Local presented={overview} pending=0\n"
        ))
        .unwrap();
        assert!(!wrong_desired.matches_desired(Target::Overview, Some(7)));
        assert!(!wrong_desired.matches(Target::Overview, Some(7)));
    }

    #[test]
    fn only_the_same_windows_completed_selection_proves_the_peer_view() {
        let chrome = |desired: &str, presented: &str, pending| {
            format!(
                "unrelated chrome\nstats-lens window=7 desired={desired} presented={presented} pending={pending}\n"
            )
        };
        let peer = Target::Peer.selection();
        let overview = Target::Overview.selection();
        let ready = parse(&chrome(peer, peer, 0)).unwrap();
        assert!(ready.matches(Target::Peer, Some(7)));
        assert!(!ready.matches(Target::Peer, Some(8)));
        assert!(!ready.matches(Target::Overview, Some(7)));
        for (desired, presented, pending) in [
            (peer, overview, 0),
            (overview, peer, 0),
            (peer, peer, 1),
            (
                "NetworkState:Global:Peer(2)",
                "NetworkState:Global:Peer(2)",
                0,
            ),
            ("Cpu:Global:Overview", "Cpu:Global:Overview", 0),
        ] {
            assert!(
                !parse(&chrome(desired, presented, pending))
                    .unwrap()
                    .matches(Target::Peer, Some(7))
            );
        }
        let one = chrome(peer, peer, 0);
        for malformed in [
            "unrelated chrome".to_owned(),
            format!("{one}{one}"),
            one.replace("pending=0", "pending=2"),
            one.replace("window=7", "window=invalid"),
            one.replace("presented=", "desired="),
        ] {
            assert!(parse(&malformed).is_err());
        }
    }

    #[test]
    fn native_navigation_requires_two_posted_keys_and_an_active_key_window() {
        assert!(two_keys_posted("OK posted=2 key=1\n"));
        for reply in [
            "OK posted=2 key=0\n",
            "OK posted=2 key=2\n",
            "OK posted=1 key=1\n",
            "ERR posted=2 key=1\n",
            "OK posted=2 key=1 key=0\n",
            "",
        ] {
            assert!(!two_keys_posted(reply));
        }
    }
}
