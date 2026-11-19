// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The single root steps a person applies from the System Resources page: its macOS
//! configuration section's (SD-97) and its Spotlight document's buttons (SD-96), each
//! `sudo aterm machines here apply --plain --only <step>`, a compiled plan of that one step's
//! items. The window's argv ([`Step::argv`], [`KEEP_OUT`]) and the CLI's parser ([`Step::parse`])
//! read this one table, and so do the password sheet's words and the items that run Spotlight's
//! own tool ([`Step::mdutil`]).

/// The line a single-step run prints on its standard output before it reads or changes
/// anything. sudo writes nothing there, so the window that runs the step under sudo reads this
/// line as the step itself, past sudo's password check: a run that ends without it never ran.
pub const STARTED: &str = "aterm machines: the step started";

/// The option that names the one folder [`Step::SpotlightKeepOut`] keeps out, an absolute path
/// as its own argument after it.
pub const KEEP_OUT: &str = "--keep-out";

/// One step a single-step root run applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
    /// Setup's Spotlight scope (`init.spotlight.scope`).
    SpotlightScope,
    /// The starter baseline's `[power.ac]`.
    Power,
    /// The starter baseline's `[updates]`.
    Updates,
    /// Indexing off on the data volume, the person's pause (`mdutil -i off`).
    SpotlightPause,
    /// Indexing back on on the data volume (`mdutil -i on`).
    SpotlightResume,
    /// The data volume's index erased and rebuilt (`mdutil -E`).
    SpotlightRebuild,
    /// One folder the person chose ([`KEEP_OUT`]) added to the data volume's excluded folders
    /// by the scope step's own edit, and recorded as theirs.
    SpotlightKeepOut,
}

impl Step {
    /// Every step: the macOS configuration section's, in the order it lists them, then the
    /// Spotlight document's own.
    pub const ALL: [Self; 7] = [
        Self::SpotlightScope,
        Self::Power,
        Self::Updates,
        Self::SpotlightPause,
        Self::SpotlightResume,
        Self::SpotlightRebuild,
        Self::SpotlightKeepOut,
    ];

    /// Its word after `--only`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::SpotlightScope => "spotlight-scope",
            Self::Power => "power",
            Self::Updates => "updates",
            Self::SpotlightPause => "spotlight-pause",
            Self::SpotlightResume => "spotlight-resume",
            Self::SpotlightRebuild => "spotlight-rebuild",
            Self::SpotlightKeepOut => "spotlight-keep-out",
        }
    }

    /// The step `word` names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|step| step.word() == word)
    }

    /// The item family every row of its run belongs to.
    #[must_use]
    pub const fn family(self) -> &'static str {
        match self {
            Self::SpotlightScope
            | Self::SpotlightPause
            | Self::SpotlightResume
            | Self::SpotlightRebuild
            | Self::SpotlightKeepOut => "spotlight",
            Self::Power => "power",
            Self::Updates => "updates",
        }
    }

    /// Whether the System Resources page's Spotlight document offers it: its four buttons
    /// (Apply scope…, Pause or Resume indexing, Rebuild index…, Keep a folder out…).
    #[must_use]
    pub const fn spotlight(self) -> bool {
        !matches!(self, Self::Power | Self::Updates)
    }

    /// Whether its run names one folder ([`KEEP_OUT`]).
    #[must_use]
    pub const fn takes_folder(self) -> bool {
        matches!(self, Self::SpotlightKeepOut)
    }

    /// What it changes, in the words the password sheet gives, after "to".
    #[must_use]
    pub const fn change(self) -> &'static str {
        match self {
            Self::SpotlightScope => "keep the folders setup scopes out of Spotlight",
            Self::Power => "set this Mac's power and sleep settings on AC power to ALab's baseline",
            Self::Updates => "set this Mac's Software Update settings to ALab's baseline",
            Self::SpotlightPause => {
                "pause Spotlight indexing on the data volume until you resume it"
            }
            Self::SpotlightResume => "resume Spotlight indexing on the data volume",
            Self::SpotlightRebuild => {
                "erase Spotlight's index of the data volume and rebuild it: hours of indexing \
                 CPU and disk reads, and search finds less until it finishes"
            }
            Self::SpotlightKeepOut => "keep a folder out of Spotlight",
        }
    }

    /// The arguments Spotlight's indexing manager ([`super::spotlight_scope::MDUTIL`]) takes
    /// before the data volume ([`super::spotlight_scope::DATA_VOLUME`]) where the step runs it,
    /// as root: off, on, or erase and rebuild.
    #[must_use]
    pub const fn mdutil(self) -> Option<&'static [&'static str]> {
        match self {
            Self::SpotlightPause => Some(&["-i", "off"]),
            Self::SpotlightResume => Some(&["-i", "on"]),
            Self::SpotlightRebuild => Some(&["-E"]),
            Self::SpotlightScope | Self::Power | Self::Updates | Self::SpotlightKeepOut => None,
        }
    }

    /// The arguments after the aterm executable, before any [`KEEP_OUT`].
    #[must_use]
    pub const fn argv(self) -> [&'static str; 6] {
        [
            "machines",
            "here",
            "apply",
            "--plain",
            "--only",
            self.word(),
        ]
    }
}
