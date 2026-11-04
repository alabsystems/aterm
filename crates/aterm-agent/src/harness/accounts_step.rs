// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The commands the account surfaces name, as values. A sentence that sends a person to a command
//! is built from a [`Step`], so what it names is structure and not words. The law
//! `every_step_a_surface_names_can_be_taken` (`accounts_law_tests.rs`) reads what each surface
//! printed, parses every command there back into a `Step`, fails one no `Step` spells, and takes
//! each through the real parser and the real entry.
//!
//! Not `Step`s, because they send nobody anywhere: usage text, an error line's own prefix
//! (`aterm harness accounts sync: …`), a generated file's header comment, and the markup of the
//! page `accounts serve` draws. Two forms a surface names only for a source or a root's decision are
//! not spelled here either and stay as words where they stand, since nothing about the machine a
//! surface reads can fill their arguments: `accounts sync <path-or-url>` and `accounts shared-renew`.

use super::accounts::Cli;
use std::fmt;

/// One command a surface names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// `aterm harness accounts --cli <id>`: the report, which says each row's state.
    Report(Cli),
    /// `aterm harness accounts sync --cli <id>` with no source named.
    Sync(Cli),
    /// `aterm harness accounts use --auto --cli <id>`: the owner's pin ends.
    Auto(Cli),
    /// `aterm harness accounts use <label> --cli <id>`: the owner's pin on a row.
    Use(Cli, String),
    /// `aterm harness accounts login <label> --cli <id>`: a person's sign-in at a terminal.
    Login(Cli, String),
    /// `aterm harness accounts renew <label> --cli <id>`: a renewal, headless or at a terminal.
    Renew(Cli, String),
    /// `aterm harness accounts browser <label> --cli <id>`: the row's browser profile in a window.
    Browser(Cli, String),
    /// `aterm harness accounts add <label> --cli <id> [--email <address>]`: a row a person labels.
    Add {
        /// The pool.
        cli: Cli,
        /// The address, where the surface knows it.
        email: Option<String>,
    },
    /// `aterm harness accounts adopt-default <label> --email <e> --cli <id>`: this machine's own
    /// login listed as a row, under this label or one a person chooses ([`Step::CHOSEN_LABEL`]).
    AdoptDefault(Cli, String),
    /// `aterm pkg install <id>`: the CLI of that id is not here.
    Install(String),
    /// `aterm pkg update <id>`: the CLI is here, and a newer build does what this one cannot.
    Update(String),
    /// `aterm ai login <id>`: the sign-in `aterm ai` picks.
    AiLogin(String),
    /// `aterm ai doctor`: the look, and each fix it may make.
    Doctor,
    /// `<program> login`: the vendor's own sign-in in this machine's default home, a person's act
    /// that aterm does not run for them.
    VendorLogin(Cli),
}

impl Step {
    /// The sign-in of a row, in the verb that signs `cli`'s row in ([`Cli::sign_in_verb`]).
    #[must_use]
    pub fn sign_in(cli: Cli, label: &str, addressed: bool) -> Self {
        match cli.sign_in_verb(addressed) {
            "renew" => Self::Renew(cli, label.to_owned()),
            _ => Self::Login(cli, label.to_owned()),
        }
    }

    /// `aterm pkg install` of `cli`.
    #[must_use]
    pub fn install(cli: Cli) -> Self {
        Self::Install(cli.id().to_owned())
    }

    /// `accounts adopt-default` under a label a person chooses.
    #[must_use]
    pub fn adopt_default(cli: Cli) -> Self {
        Self::AdoptDefault(cli, Self::CHOSEN_LABEL.to_owned())
    }

    /// `aterm ai login` of `cli`.
    #[must_use]
    pub fn ai_login(cli: Cli) -> Self {
        Self::AiLogin(cli.id().to_owned())
    }

    /// The label a person chooses, as a step that takes one spells it.
    pub const CHOSEN_LABEL: &'static str = "<label>";
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let flag = |cli: &Cli| {
            if *cli == Cli::Claude {
                String::new()
            } else {
                format!(" --cli {}", cli.id())
            }
        };
        let accounts = "aterm harness accounts";
        let chosen = Self::CHOSEN_LABEL;
        match self {
            Self::Report(cli) => write!(f, "{accounts}{}", flag(cli)),
            Self::Sync(cli) => write!(f, "{accounts} sync{}", flag(cli)),
            Self::Auto(cli) => write!(f, "{accounts} use --auto{}", flag(cli)),
            Self::Use(cli, label) => write!(f, "{accounts} use {label}{}", flag(cli)),
            Self::Login(cli, label) => write!(f, "{accounts} login {label}{}", flag(cli)),
            Self::Renew(cli, label) => write!(f, "{accounts} renew {label}{}", flag(cli)),
            Self::Browser(cli, label) => write!(f, "{accounts} browser {label}{}", flag(cli)),
            Self::Add { cli, email } => {
                write!(f, "{accounts} add {chosen}{}", flag(cli))?;
                match email {
                    Some(address) => write!(f, " --email {address}"),
                    None => Ok(()),
                }
            }
            Self::AdoptDefault(cli, label) => {
                write!(
                    f,
                    "{accounts} adopt-default {label} --email <e>{}",
                    flag(cli)
                )
            }
            Self::Install(id) => write!(f, "aterm pkg install {id}"),
            Self::Update(id) => write!(f, "aterm pkg update {id}"),
            Self::AiLogin(id) => write!(f, "aterm ai login {id}"),
            Self::Doctor => f.write_str("aterm ai doctor"),
            Self::VendorLogin(cli) => write!(f, "{} login", cli.id()),
        }
    }
}
