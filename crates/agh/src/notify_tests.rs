// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::{Notice, Posted, Slug};

/// Under a tape, `settled` emits only a line `sendable` answers `Ok`, so a body over the
/// broker's 4 KiB limit is emitted nowhere, taped or not.
#[test]
fn an_oversized_body_is_never_sendable() {
    let repo = Slug::parse("fixture-dev/app").expect("a slug");
    let big = Notice::converged(&repo, &"t".repeat(4097), "uploaded", Some(1), None);
    assert_eq!(
        big.sendable("self"),
        Err(Posted::Failed("oversized".to_string()))
    );
    let small = Notice::converged(&repo, "v1", "uploaded", Some(1), None);
    assert_eq!(small.sendable("self"), Ok(small.line("self")));
}

/// A tab's env whose `TMUX` or `ATERM_MUX` is not UTF-8. `MapEnv::with` takes only
/// `&str`, so it cannot carry one.
#[cfg(unix)]
struct RawEnv {
    /// The UTF-8 variables.
    map: crate::identity::MapEnv,
    /// The one variable whose value is not UTF-8.
    raw: &'static str,
}

#[cfg(unix)]
impl crate::identity::EnvSource for RawEnv {
    fn var_os(&self, name: &str) -> Option<std::ffi::OsString> {
        use std::os::unix::ffi::OsStringExt;
        if name == self.raw {
            Some(std::ffi::OsString::from_vec(b"/tmp/\xff".to_vec()))
        } else {
            self.map.var_os(name)
        }
    }
}

/// A value that is not UTF-8 is unset, as aterm-ctl reads it, so a tab whose `TMUX` or
/// `ATERM_MUX` is not UTF-8 posts where `aterm ctl @self post` works.
#[cfg(unix)]
#[test]
fn a_value_that_is_not_utf8_is_unset() {
    use super::{SessionNotifier, mux_nested};
    let tab = || {
        crate::identity::MapEnv::new()
            .with("ATERM_PARENT_SESSION_ID", "s-01")
            .with("ATERM_MUX_BASE", "|")
    };
    for raw in ["TMUX", "ATERM_MUX"] {
        let env = RawEnv { map: tab(), raw };
        assert!(!mux_nested(&env), "{raw}");
        assert_eq!(
            SessionNotifier::from_env(&env).sid,
            Ok("s-01".to_string()),
            "{raw}"
        );
    }
    // The control: the same tab with a UTF-8 `TMUX` is a pane nested in it.
    let pane = RawEnv {
        map: tab().with("TMUX", "/tmp/tmux-1/default,1,0"),
        raw: "UNSET",
    };
    assert!(mux_nested(&pane));
}

/// `ATERM_MUX_BASE` is `$TMUX|$STY` joined, so a tab begun under a tmux whose socket name
/// holds a `|` (`tmux -L 'a|b'`) is the same multiplexer, not one nested in it; a pane
/// opened later under another `TMUX` is nested.
#[test]
fn a_bar_inside_tmux_is_the_same_session() {
    use super::mux_nested;
    let tmux = "/tmp/tmux-501/a|b,123,0";
    let tab = |now: &str| {
        crate::identity::MapEnv::new()
            .with("ATERM_PARENT_SESSION_ID", "s-01")
            .with("ATERM_MUX_BASE", &format!("{tmux}|"))
            .with("TMUX", now)
    };
    assert!(!mux_nested(&tab(tmux)));
    assert!(mux_nested(&tab("/tmp/tmux-501/default,9,0")));
}
