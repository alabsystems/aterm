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

use super::account_names::Word;
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
    /// `aterm harness accounts use <account> --cli <id>`: the owner's pin on the row of an
    /// account, named by its [`Word`] (its slug, else its key; never its address or a roster
    /// label).
    Use(Cli, Word),
    /// `aterm harness accounts login <account> --cli <id>`: a person's sign-in at a terminal of
    /// the row of an account, named by its [`Word`] (its slug, else its key; a row with no
    /// address, which has no account, by its label: [`super::account_names::sign_in_word`]).
    Login(Cli, Word),
    /// `aterm harness accounts renew <account>… --cli <id>`: a renewal, headless or at a terminal,
    /// of the rows of these accounts, each named by its [`Word`] as [`Step::Login`] names one.
    Renew(Cli, Vec<Word>),
    /// `aterm harness accounts browser <account> --cli <id>`: the browser profile of the row of an
    /// account in a window, the account named by its [`Word`] as [`Step::Login`] names one.
    Browser(Cli, Word),
    /// `aterm harness accounts add <account> --cli <id>`: a row of the account a person names
    /// ([`Step::CHOSEN_LABEL`]): its slug or key where the lab knows it, else its provider word and
    /// address, which a step never types, since a step reaches logs and captured stderr.
    Add(Cli),
    /// `aterm harness accounts adopt-default <account> --cli <id>`: this machine's own login listed
    /// as a row of the account a person names ([`Step::CHOSEN_LABEL`]), as [`Step::Add`] names one.
    AdoptDefault(Cli),
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
    /// `aterm harness accounts token <account> [--replace]`: a Claude Code setup-token made for
    /// one account and stored in the vault (`docs/DESIGN-claude-setup-token-2026-11-05.md`
    /// §4.6). `word` names the account by its slug, else its key (never its address or a roster
    /// label: a step reaches logs and captured stderr); `replace` is set where the token the step
    /// offers for is still in use (`soon`, `exposed`), so the step never meets the verb's refusal
    /// of a bare run.
    Token {
        /// The account's word ([`Word`]).
        word: Word,
        /// `--replace`.
        replace: bool,
    },
}

impl Step {
    /// The sign-in of a row, named by `word` ([`super::account_names::sign_in_word`]), in the
    /// verb that signs `cli`'s row in ([`Cli::sign_in_verb`]).
    #[must_use]
    pub fn sign_in(cli: Cli, word: Word, addressed: bool) -> Self {
        match cli.sign_in_verb(addressed) {
            "renew" => Self::Renew(cli, vec![word]),
            _ => Self::Login(cli, word),
        }
    }

    /// The renewal of the one row `word` names.
    #[must_use]
    pub fn renew(cli: Cli, word: Word) -> Self {
        Self::Renew(cli, vec![word])
    }

    /// `aterm pkg install` of `cli`.
    #[must_use]
    pub fn install(cli: Cli) -> Self {
        Self::Install(cli.id().to_owned())
    }

    /// `aterm ai login` of `cli`.
    #[must_use]
    pub fn ai_login(cli: Cli) -> Self {
        Self::AiLogin(cli.id().to_owned())
    }

    /// The account a person names, as a step that takes one spells it: a step that lists a row
    /// ([`Step::Add`], [`Step::AdoptDefault`]) names an account no row here holds yet, which no word
    /// of this Mac's names, as [`Word::chosen`] spells one.
    pub const CHOSEN_LABEL: &'static str = "<account>";
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
            Self::Use(cli, word) => write!(f, "{accounts} use {word}{}", flag(cli)),
            Self::Login(cli, word) => write!(f, "{accounts} login {word}{}", flag(cli)),
            Self::Renew(cli, words) => {
                write!(f, "{accounts} renew")?;
                for word in words {
                    write!(f, " {word}")?;
                }
                f.write_str(&flag(cli))
            }
            Self::Browser(cli, word) => write!(f, "{accounts} browser {word}{}", flag(cli)),
            Self::Add(cli) => write!(f, "{accounts} add {chosen}{}", flag(cli)),
            Self::AdoptDefault(cli) => write!(f, "{accounts} adopt-default {chosen}{}", flag(cli)),
            Self::Install(id) => write!(f, "aterm pkg install {id}"),
            Self::Update(id) => write!(f, "aterm pkg update {id}"),
            Self::AiLogin(id) => write!(f, "aterm ai login {id}"),
            Self::Doctor => f.write_str("aterm ai doctor"),
            Self::VendorLogin(cli) => write!(f, "{} login", cli.id()),
            Self::Token { word, replace } => {
                write!(f, "{accounts} token {word}")?;
                if *replace {
                    f.write_str(" --replace")?;
                }
                Ok(())
            }
        }
    }
}
