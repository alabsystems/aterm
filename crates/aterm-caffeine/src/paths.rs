// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! File paths for `aterm caffeine`, derived from the three root directories in [`Paths`].

use std::path::PathBuf;

/// The mug's files, under three roots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// aterm's state directory (`~/Library/Application Support/aterm`).
    pub state_dir: PathBuf,
    /// The user's home directory (for `~/Library/LaunchAgents`).
    pub home: PathBuf,
    /// aterm's logs directory (`~/Library/Logs/aterm`).
    pub logs_dir: PathBuf,
}

impl Paths {
    /// The roots aterm uses for this user (`aterm_types::dirs`); `None` without a home.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        Some(Self {
            state_dir: aterm_types::dirs::state_dir()?,
            home: aterm_types::dirs::home_dir()?,
            logs_dir: aterm_types::dirs::logs_dir()?,
        })
    }

    /// The mode on disk ([`crate::state`]).
    #[must_use]
    pub fn state_file(&self) -> PathBuf {
        self.state_dir.join("caffeine.state")
    }

    /// The lock a running mug holds for its whole life, with its pid inside
    /// ([`crate::instance`]).
    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.state_dir.join("caffeine.lock")
    }

    /// PID file for `aterm caffeine bridge` across an update restart.
    #[must_use]
    pub fn bridge_file(&self) -> PathBuf {
        self.state_dir.join("caffeine.bridge")
    }

    /// Cloned bundle directory (`.noindex` excludes copies from Spotlight).
    #[must_use]
    pub fn code_dir(&self) -> PathBuf {
        self.state_dir.join("caffeine-code.noindex")
    }

    /// Session-only launchd plist when Enable at Startup is off.
    #[must_use]
    pub fn session_plist(&self, label: &str) -> PathBuf {
        self.state_dir
            .join("caffeine")
            .join(format!("{label}.plist"))
    }

    /// Where launchd finds the jobs it loads at every login: `~/Library/LaunchAgents`.
    #[must_use]
    pub fn launch_agents(&self) -> PathBuf {
        self.home.join("Library").join("LaunchAgents")
    }

    /// Login-time launchd plist when Enable at Startup is on.
    #[must_use]
    pub fn startup_plist(&self, label: &str) -> PathBuf {
        self.launch_agents().join(format!("{label}.plist"))
    }

    /// The job's stderr.
    #[must_use]
    pub fn log_file(&self) -> PathBuf {
        self.logs_dir.join("caffeine.log")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_lives_under_its_root() {
        let paths = Paths {
            state_dir: PathBuf::from("/s"),
            home: PathBuf::from("/h"),
            logs_dir: PathBuf::from("/l"),
        };
        assert_eq!(paths.state_file(), PathBuf::from("/s/caffeine.state"));
        assert_eq!(paths.lock_file(), PathBuf::from("/s/caffeine.lock"));
        assert_eq!(paths.bridge_file(), PathBuf::from("/s/caffeine.bridge"));
        assert_eq!(paths.code_dir(), PathBuf::from("/s/caffeine-code.noindex"));
        assert_eq!(
            paths.session_plist("x.y"),
            PathBuf::from("/s/caffeine/x.y.plist")
        );
        assert_eq!(
            paths.startup_plist("x.y"),
            PathBuf::from("/h/Library/LaunchAgents/x.y.plist")
        );
        assert_eq!(
            paths.startup_plist("x.y").parent(),
            Some(paths.launch_agents().as_path())
        );
        assert_eq!(paths.log_file(), PathBuf::from("/l/caffeine.log"));
    }
}
