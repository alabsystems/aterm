// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verb table used by the CLI parser, help renderer, schema output, and command dispatch.

use super::{
    Ctx, Outcome, args::Invocation, cmd_api, cmd_asset, cmd_auth, cmd_can, cmd_content,
    cmd_credential, cmd_git, cmd_intent, cmd_meta, cmd_policy, cmd_rate, cmd_ref, cmd_release,
    cmd_release_write, cmd_repo, cmd_wait, cmd_whoami,
};
use crate::error::Result;

/// How a verb touches GitHub.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// One read.
    Read,
    /// A listing.
    List,
    /// This machine only (`whoami --verify` also asks GitHub as each profile, and with no
    /// agh.toml, `whoami` and `auth status` learn a vault token's login once, design §6.10).
    Local,
    /// Provisions an identity (an agent as a person does, owner ruling 2026-10-26).
    Provision,
    /// As its method says (`api`).
    ByMethod,
    /// An additive or modifying write (design §9.4's A and M).
    Write,
    /// A write that removes something, which needs `--confirm` (D).
    Destructive,
    /// Runs git or answers it.
    Git,
}

/// What a flag takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Takes {
    /// Nothing.
    None,
    /// One value.
    Value,
    /// One value per use, any number of uses.
    Repeat,
}

/// One flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlagSpec {
    /// The long name, with its dashes.
    pub name: &'static str,
    /// The short name.
    pub short: Option<char>,
    /// What it takes.
    pub takes: Takes,
    /// One line of help.
    pub help: &'static str,
}

/// What a verb's answer is a document of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Noun {
    /// A release.
    Release,
    /// A row of a release list.
    ReleaseList,
    /// A release asset.
    Asset,
    /// A directory entry.
    ContentEntry,
    /// A tree entry.
    TreeEntry,
    /// A commit.
    Commit,
    /// A repository.
    Repo,
    /// A branch.
    Branch,
    /// A tag.
    Tag,
    /// A user.
    User,
    /// A rate limit.
    Rate,
    /// An identity.
    Identity,
    /// A policy report.
    Policy,
    /// A routing decision.
    Route,
    /// A comparison.
    Compare,
    /// A raw answer.
    Raw,
    /// A downloaded file.
    Download,
    /// A wait's state.
    Wait,
    /// A permission answer.
    Permission,
    /// agh's own documents (help, schema, version).
    Meta,
    /// A write's outcome (`write_out::WriteReport`).
    Write,
    /// One journal intent.
    Intent,
    /// What the git helper would answer (`whoami --git`).
    Credential,
}

impl Noun {
    /// Every noun, in declaration order.
    pub const ALL: [Noun; 23] = [
        Noun::Release,
        Noun::ReleaseList,
        Noun::Asset,
        Noun::ContentEntry,
        Noun::TreeEntry,
        Noun::Commit,
        Noun::Repo,
        Noun::Branch,
        Noun::Tag,
        Noun::User,
        Noun::Rate,
        Noun::Identity,
        Noun::Policy,
        Noun::Route,
        Noun::Compare,
        Noun::Raw,
        Noun::Download,
        Noun::Wait,
        Noun::Permission,
        Noun::Meta,
        Noun::Write,
        Noun::Intent,
        Noun::Credential,
    ];

    /// The canonical kebab-case slug for this noun.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Noun::Release => "release",
            Noun::ReleaseList => "release-list",
            Noun::Asset => "asset",
            Noun::ContentEntry => "content-entry",
            Noun::TreeEntry => "tree-entry",
            Noun::Commit => "commit",
            Noun::Repo => "repo",
            Noun::Branch => "branch",
            Noun::Tag => "tag",
            Noun::User => "user",
            Noun::Rate => "rate",
            Noun::Identity => "identity",
            Noun::Policy => "policy",
            Noun::Route => "route",
            Noun::Compare => "compare",
            Noun::Raw => "raw",
            Noun::Download => "download",
            Noun::Wait => "wait",
            Noun::Permission => "permission",
            Noun::Meta => "meta",
            Noun::Write => "write",
            Noun::Intent => "intent",
            Noun::Credential => "credential",
        }
    }

    /// The noun `slug` names.
    #[must_use]
    pub fn parse(text: &str) -> Option<Noun> {
        Noun::ALL.into_iter().find(|noun| noun.slug() == text)
    }
}

/// One verb.
pub struct Verb {
    /// The words of its path.
    pub path: &'static [&'static str],
    /// How it touches GitHub.
    pub class: Class,
    /// The usage line.
    pub usage: &'static str,
    /// One line, at most 100 characters.
    pub summary: &'static str,
    /// One command line that parses.
    pub example: &'static str,
    /// What its answer is a document of.
    pub noun: Noun,
    /// The gh command it replaces (`-` for none).
    pub gh: &'static str,
    /// The milestone it arrived in.
    pub milestone: u8,
    /// Its own flags.
    pub flags: &'static [FlagSpec],
    /// Its positionals' names.
    pub positionals: &'static [&'static str],
    /// What runs it.
    pub run: fn(&mut Ctx, &Invocation) -> Result<Outcome>,
}

impl std::fmt::Debug for Verb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.path.join("."))
    }
}

impl PartialEq for Verb {
    fn eq(&self, other: &Verb) -> bool {
        self.path == other.path
    }
}

impl Eq for Verb {}

/// A refused verb.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused {
    /// The words of its path.
    pub path: &'static [&'static str],
    /// Why it is refused.
    pub why: &'static str,
    /// What to do instead.
    pub next: &'static str,
}

const fn on(name: &'static str, help: &'static str) -> FlagSpec {
    FlagSpec {
        name,
        short: None,
        takes: Takes::None,
        help,
    }
}

const fn value(name: &'static str, help: &'static str) -> FlagSpec {
    FlagSpec {
        name,
        short: None,
        takes: Takes::Value,
        help,
    }
}

const fn repeat(name: &'static str, help: &'static str) -> FlagSpec {
    FlagSpec {
        name,
        short: None,
        takes: Takes::Repeat,
        help,
    }
}

const fn short(flag: FlagSpec, short: char) -> FlagSpec {
    FlagSpec {
        short: Some(short),
        ..flag
    }
}

const INCLUDE_DRAFTS: FlagSpec = on(
    "--include-drafts",
    "read as the owner's writer, so a draft release is seen",
);
const ID: FlagSpec = value("--id", "the release's numeric id, in place of TAG");
const TAG_REGEX: FlagSpec = value("--tag-regex", "only tags this regular expression matches");
const PRERELEASE: FlagSpec = on("--prerelease", "only prereleases");
const NO_PRERELEASE: FlagSpec = on("--no-prerelease", "no prereleases");
const DRY_RUN: FlagSpec = on("--dry-run", "say what would be sent, and send nothing");
const NOTIFY: FlagSpec = on(
    "--notify",
    "post the settled event to this session's fabric",
);
const CONFIRM: FlagSpec = value("--confirm", "the name being removed, typed again");
const TITLE: FlagSpec = value("--title", "the release's title (default: its tag)");
const NOTES: FlagSpec = value("--notes", "the release notes, as text");
const NOTES_FILE: FlagSpec = value("--notes-file", "the release notes, read from this file");
const TARGET: FlagSpec = value("--target", "the branch or sha a new tag is made at");
const MARK_PRERELEASE: FlagSpec = on("--prerelease", "mark it a prerelease");
const MARK_FULL: FlagSpec = on("--no-prerelease", "mark it a full release");
const TIER: FlagSpec = value("--tier", "dev, release or external");
const OWNER: FlagSpec = repeat(
    "--owner",
    "an owner the profile reads for; give it once per owner",
);
const WRITE: FlagSpec = on("--write", "make it those owners' writer");
const DRAFTS: FlagSpec = on(
    "--drafts",
    "vouch that this token may read drafts (needs --write)",
);

/// Every verb, in table order.
pub static VERBS: &[Verb] = &[
    Verb {
        path: &["cat"],
        class: Class::Read,
        usage: "agh cat REPO[@REF]:PATH [--max-bytes N]",
        summary: "print one file of a repository, exactly",
        example: "agh cat alabsystems/aterm@main:README.md",
        noun: Noun::Raw,
        gh: "gh api repos/O/R/contents/P",
        milestone: 1,
        flags: &[value(
            "--max-bytes",
            "refuse a file larger than N bytes (default 32 MiB)",
        )],
        positionals: &["CONTENT"],
        run: cmd_content::cat,
    },
    Verb {
        path: &["ls"],
        class: Class::Read,
        usage: "agh ls REPO[@REF][:PATH]",
        summary: "one directory's entries, by name",
        example: "agh ls alabsystems/aterm:docs",
        noun: Noun::ContentEntry,
        gh: "gh api repos/O/R/contents/P",
        milestone: 1,
        flags: &[],
        positionals: &["CONTENT"],
        run: cmd_content::ls,
    },
    Verb {
        path: &["find"],
        class: Class::Read,
        usage: "agh find REPO[@REF] [PREFIX] [--type blob|tree] [--match RE]",
        summary: "every path of a repository's tree at one ref, by path",
        example: "agh find alabsystems/aterm crates --type blob --match '\\.toml$'",
        noun: Noun::TreeEntry,
        gh: "gh api repos/O/R/git/trees/REF?recursive=1",
        milestone: 1,
        flags: &[
            value("--type", "blob or tree"),
            value("--match", "only paths this regular expression matches"),
        ],
        positionals: &["CONTENT", "PREFIX?"],
        run: cmd_content::find,
    },
    Verb {
        path: &["rev"],
        class: Class::Read,
        usage: "agh rev REPO@REF",
        summary: "the commit a branch, tag or sha names",
        example: "agh rev alabsystems/aterm@main",
        noun: Noun::Commit,
        gh: "gh api repos/O/R/commits/REF",
        milestone: 1,
        flags: &[],
        positionals: &["CONTENT"],
        run: cmd_git::rev,
    },
    Verb {
        path: &["log"],
        class: Class::List,
        usage: "agh log REPO[@REF] [--path P] [--since T] [--author L] [--limit N]",
        summary: "commits, newest first: 30 unless --limit",
        example: "agh log alabsystems/aterm@main --limit 3",
        noun: Noun::Commit,
        gh: "gh api repos/O/R/commits",
        milestone: 1,
        flags: &[
            value("--path", "only commits that touch this path"),
            value("--since", "only commits after this RFC 3339 UTC time"),
            value("--author", "only commits by this login or address"),
        ],
        positionals: &["CONTENT"],
        run: cmd_git::log,
    },
    Verb {
        path: &["compare"],
        class: Class::Read,
        usage: "agh compare REPO BASE...HEAD",
        summary: "how two refs differ: ahead, behind, commits and files",
        example: "agh compare alabsystems/aterm <base>...<head>",
        noun: Noun::Compare,
        gh: "gh api repos/O/R/compare/A...B",
        milestone: 1,
        flags: &[],
        positionals: &["REPO", "RANGE"],
        run: cmd_git::compare,
    },
    Verb {
        path: &["branches"],
        class: Class::List,
        usage: "agh branches REPO",
        summary: "a repository's branches, by name",
        example: "agh branches alabsystems/aterm",
        noun: Noun::Branch,
        gh: "gh api repos/O/R/branches",
        milestone: 1,
        flags: &[],
        positionals: &["REPO"],
        run: cmd_git::branches,
    },
    Verb {
        path: &["tags"],
        class: Class::List,
        usage: "agh tags REPO",
        summary: "a repository's tags, by name",
        example: "agh tags alabsystems/aterm",
        noun: Noun::Tag,
        gh: "gh api repos/O/R/tags",
        milestone: 1,
        flags: &[],
        positionals: &["REPO"],
        run: cmd_git::tags,
    },
    Verb {
        path: &["repo", "public-refs"],
        class: Class::Read,
        usage: "agh --anonymous repo public-refs REPO",
        summary: "complete public smart-HTTP refs, including peeled tags, without REST quota",
        example: "agh --anonymous repo public-refs alabsystems/aterm",
        noun: Noun::Repo,
        gh: "git ls-remote",
        milestone: 2,
        flags: &[],
        positionals: &["REPO"],
        run: cmd_repo::repo_public_refs,
    },
    Verb {
        path: &["asset", "fetch-public"],
        class: Class::Read,
        usage: "agh --anonymous asset fetch-public REPO TAG NAME",
        summary: "stream at most 2 GiB from the public release URL without REST quota",
        example: "agh --anonymous asset fetch-public alabsystems/aterm v1 SHA256SUMS",
        noun: Noun::Asset,
        gh: "curl",
        milestone: 2,
        flags: &[],
        positionals: &["REPO", "TAG", "NAME"],
        run: cmd_asset::asset_fetch_public,
    },
    Verb {
        path: &["repo", "create"],
        class: Class::Write,
        usage: "agh repo create OWNER/NAME --public [--dry-run]",
        summary: "create a public repository for the authenticated User owner",
        example: "agh repo create alabsystems/new-project --public --dry-run",
        noun: Noun::Write,
        gh: "gh repo create",
        milestone: 2,
        flags: &[
            on("--public", "required: create public, with no initial files"),
            DRY_RUN,
        ],
        positionals: &["REPO"],
        run: cmd_repo::repo_create,
    },
    Verb {
        path: &["repo", "view"],
        class: Class::Read,
        usage: "agh repo view REPO",
        summary: "one repository",
        example: "agh repo view alabsystems/aterm",
        noun: Noun::Repo,
        gh: "gh repo view",
        milestone: 1,
        flags: &[],
        positionals: &["REPO"],
        run: cmd_repo::repo_view,
    },
    Verb {
        path: &["repo", "list"],
        class: Class::List,
        usage: "agh repo list OWNER [--type all|public|private|forks|sources]",
        summary: "an owner's repositories, by name",
        example: "agh repo list alabsystems --type public",
        noun: Noun::Repo,
        gh: "gh repo list",
        milestone: 1,
        flags: &[value(
            "--type",
            "all (the default), public, private, forks or sources",
        )],
        positionals: &["OWNER"],
        run: cmd_repo::repo_list,
    },
    Verb {
        path: &["release", "list"],
        class: Class::List,
        usage: "agh release list REPO [--drafts] [--tag-regex RE] [--prerelease|--no-prerelease]",
        summary: "a repository's releases, newest first",
        example: "agh release list alabsystems/aterm --limit 3",
        noun: Noun::ReleaseList,
        gh: "gh release list",
        milestone: 1,
        flags: &[
            on("--drafts", "drafts too, read as the owner's writer"),
            TAG_REGEX,
            PRERELEASE,
            NO_PRERELEASE,
        ],
        positionals: &["REPO"],
        run: cmd_release::release_list,
    },
    Verb {
        path: &["release", "view"],
        class: Class::Read,
        usage: "agh release view REPO TAG | --id ID [--include-drafts]",
        summary: "one release, by tag or id",
        example: "agh release view alabsystems/aterm <tag>",
        noun: Noun::Release,
        gh: "gh release view",
        milestone: 1,
        flags: &[ID, INCLUDE_DRAFTS],
        positionals: &["REPO", "TAG?"],
        run: cmd_release::release_view,
    },
    Verb {
        path: &["release", "latest"],
        class: Class::Read,
        usage: "agh release latest REPO",
        summary: "the release GitHub marks latest",
        example: "agh release latest alabsystems/aterm",
        noun: Noun::Release,
        gh: "gh release view",
        milestone: 1,
        flags: &[],
        positionals: &["REPO"],
        run: cmd_release::release_latest,
    },
    Verb {
        path: &["release", "newest"],
        class: Class::Read,
        usage: "agh release newest REPO [--tag-regex RE] [--prerelease|--no-prerelease]",
        summary: "the newest published release that passes the filters",
        example: "agh release newest alabsystems/aterm --no-prerelease",
        noun: Noun::Release,
        gh: "-",
        milestone: 1,
        flags: &[TAG_REGEX, PRERELEASE, NO_PRERELEASE],
        positionals: &["REPO"],
        run: cmd_release::release_newest,
    },
    Verb {
        path: &["release", "assets"],
        class: Class::List,
        usage: "agh release assets REPO TAG | --id ID [--include-drafts]",
        summary: "one release's assets",
        example: "agh release assets alabsystems/aterm <tag>",
        noun: Noun::Asset,
        gh: "gh release view --json assets",
        milestone: 1,
        flags: &[ID, INCLUDE_DRAFTS],
        positionals: &["REPO", "TAG?"],
        run: cmd_release::release_assets,
    },
    Verb {
        path: &["release", "download"],
        class: Class::Read,
        usage: "agh release download REPO TAG | --id ID --asset NAME... [--dir D | --output F | --output -] [--clobber] [--include-drafts]",
        summary: "download release assets by name, each checked against its size and GitHub's digest if any",
        example: "agh release download alabsystems/aterm <tag> --asset SHA256SUMS --dir <dir>",
        noun: Noun::Download,
        gh: "gh release download",
        milestone: 1,
        flags: &[
            ID,
            repeat("--asset", "an asset's name; give it once per asset"),
            value("--dir", "the directory the assets go to (default .)"),
            value("--output", "the file one asset goes to; - for stdout"),
            on("--clobber", "replace a file that holds different bytes"),
            INCLUDE_DRAFTS,
        ],
        positionals: &["REPO", "TAG?"],
        run: cmd_release::release_download,
    },
    Verb {
        path: &["release", "wait"],
        class: Class::Read,
        usage: "agh release wait REPO TAG COND... [--include-drafts] [--timeout DUR] [--notify]",
        summary: "wait until every COND holds on a release: 30m unless --timeout",
        example: "agh release wait alabsystems/aterm <tag> published --timeout 30s",
        noun: Noun::Wait,
        gh: "-",
        milestone: 1,
        flags: &[INCLUDE_DRAFTS, NOTIFY],
        positionals: &["REPO", "TAG", "COND…"],
        run: cmd_wait::release_wait,
    },
    Verb {
        path: &["release", "create"],
        class: Class::Write,
        usage: "agh release create REPO TAG --draft [--title T] [--notes TEXT | --notes-file F] [--target REF] [--prerelease] [--dry-run] [--notify]",
        summary: "create a draft release, once: a rerun converges on the draft it made",
        example: "agh release create alabsystems/aterm <tag> --draft --dry-run",
        noun: Noun::Write,
        gh: "gh release create --draft",
        milestone: 2,
        flags: &[
            on(
                "--draft",
                "create it as a draft: publishing is its own step",
            ),
            TITLE,
            NOTES,
            NOTES_FILE,
            TARGET,
            MARK_PRERELEASE,
            DRY_RUN,
            NOTIFY,
        ],
        positionals: &["REPO", "TAG"],
        run: cmd_release_write::release_create,
    },
    Verb {
        path: &["release", "upload"],
        class: Class::Write,
        usage: "agh release upload REPO TAG | --id ID FILE... [--clobber] [--dry-run] [--notify]",
        summary: "upload files to a release as assets, each checked against its digest",
        example: "agh release upload alabsystems/aterm <tag> <file> --dry-run",
        noun: Noun::Write,
        gh: "gh release upload",
        milestone: 2,
        flags: &[
            ID,
            on(
                "--clobber",
                "replace an asset of the same name that holds other bytes",
            ),
            DRY_RUN,
            NOTIFY,
        ],
        positionals: &["REPO", "TAG?", "FILE…?"],
        run: cmd_release_write::release_upload,
    },
    Verb {
        path: &["release", "publish"],
        class: Class::Write,
        usage: "agh release publish REPO TAG | --id ID [--prerelease | --no-prerelease] [--expect-asset NAME=SHA256]... [--dry-run] [--notify]",
        summary: "publish a draft release, its assets checked against the digests expected",
        example: "agh release publish alabsystems/aterm <tag> --expect-asset <name>=<sha256> --dry-run",
        noun: Noun::Write,
        gh: "gh release edit --draft=false",
        milestone: 2,
        flags: &[
            ID,
            on("--prerelease", "publish it as a prerelease"),
            on("--no-prerelease", "publish it as a full release"),
            repeat(
                "--expect-asset",
                "NAME=SHA256 an asset must hold; give it once per asset",
            ),
            DRY_RUN,
            NOTIFY,
        ],
        positionals: &["REPO", "TAG?"],
        run: cmd_release_write::release_publish,
    },
    Verb {
        path: &["release", "edit"],
        class: Class::Write,
        usage: "agh release edit REPO TAG | --id ID [--title T] [--notes TEXT | --notes-file F] [--prerelease | --no-prerelease] [--dry-run]",
        summary: "change a release's title, notes or prerelease mark",
        example: "agh release edit alabsystems/aterm <tag> --title <title> --dry-run",
        noun: Noun::Write,
        gh: "gh release edit",
        milestone: 2,
        flags: &[
            ID,
            TITLE,
            NOTES,
            NOTES_FILE,
            MARK_PRERELEASE,
            MARK_FULL,
            DRY_RUN,
        ],
        positionals: &["REPO", "TAG?"],
        run: cmd_release_write::release_edit,
    },
    Verb {
        path: &["release", "ensure"],
        class: Class::Write,
        usage: "agh release ensure REPO TAG [--title T] [--notes TEXT | --notes-file F] [--target REF] [--prerelease] [--file F]... [--publish] [--dry-run] [--notify]",
        summary: "make a release hold its files, and publish it: each step once, a rerun converges",
        example: "agh release ensure alabsystems/aterm <tag> --file <file> --publish --dry-run",
        noun: Noun::Write,
        gh: "-",
        milestone: 2,
        flags: &[
            TITLE,
            NOTES,
            NOTES_FILE,
            TARGET,
            MARK_PRERELEASE,
            repeat(
                "--file",
                "a file the release must hold as an asset; once per file",
            ),
            on("--publish", "publish it once every file is there"),
            DRY_RUN,
            NOTIFY,
        ],
        positionals: &["REPO", "TAG"],
        run: cmd_release_write::release_ensure,
    },
    Verb {
        path: &["release", "delete"],
        class: Class::Destructive,
        usage: "agh release delete REPO TAG | --id ID --confirm TAG [--dry-run] [--notify]",
        summary: "delete a release, its tag typed again; the git tag stays",
        example: "agh release delete alabsystems/aterm <tag> --confirm <tag> --dry-run",
        noun: Noun::Write,
        gh: "gh release delete",
        milestone: 2,
        flags: &[ID, CONFIRM, DRY_RUN, NOTIFY],
        positionals: &["REPO", "TAG?"],
        run: cmd_release_write::release_delete,
    },
    Verb {
        path: &["asset", "delete"],
        class: Class::Destructive,
        usage: "agh asset delete REPO TAG NAME --confirm NAME [--dry-run] [--notify]",
        summary: "delete one release asset, its name typed again",
        example: "agh asset delete alabsystems/aterm <tag> <name> --confirm <name> --dry-run",
        noun: Noun::Write,
        gh: "gh release delete-asset",
        milestone: 2,
        flags: &[CONFIRM, DRY_RUN, NOTIFY],
        positionals: &["REPO", "TAG", "NAME"],
        run: cmd_asset::asset_delete,
    },
    Verb {
        path: &["ref", "create"],
        class: Class::Write,
        usage: "agh ref create REPO REF SHA [--dry-run]",
        summary: "create a branch or tag ref at a sha, once: a rerun at the same sha converges",
        example: "agh ref create alabsystems/aterm refs/tags/<tag> <sha> --dry-run",
        noun: Noun::Write,
        gh: "gh api repos/O/R/git/refs",
        milestone: 2,
        flags: &[DRY_RUN],
        positionals: &["REPO", "REF", "SHA"],
        run: cmd_ref::ref_create,
    },
    Verb {
        path: &["ref", "delete"],
        class: Class::Destructive,
        usage: "agh ref delete REPO REF --expect SHA --confirm REF [--dry-run]",
        summary: "delete a ref only while it names the sha expected: a leased git push",
        example: "agh ref delete alabsystems/aterm <ref> --expect <sha> --confirm <ref> --dry-run",
        noun: Noun::Write,
        gh: "git push --delete",
        milestone: 2,
        flags: &[
            value("--expect", "the sha the ref must still name"),
            CONFIRM,
            DRY_RUN,
        ],
        positionals: &["REPO", "REF"],
        run: cmd_ref::ref_delete,
    },
    Verb {
        path: &["ref", "update-atomic"],
        class: Class::Write,
        usage: "agh ref update-atomic REPO --input FILE [--dry-run]",
        summary: "compare and update 1–128 refs atomically, without force or deletion",
        example: "agh ref update-atomic alabsystems/aterm --input updates.json --dry-run",
        noun: Noun::Write,
        gh: "GitHub GraphQL updateRefs",
        milestone: 2,
        flags: &[
            value("--input", "a JSON array of {ref, before, after} objects"),
            DRY_RUN,
        ],
        positionals: &["REPO"],
        run: cmd_ref::ref_update_atomic,
    },
    Verb {
        path: &["repo", "check-local"],
        class: Class::Local,
        usage: "agh repo check-local REPO DIR",
        summary: "admit local object reads without identity loading, GitHub requests or source writes",
        example: "agh repo check-local alabsystems/aterm .",
        noun: Noun::Raw,
        gh: "-",
        milestone: 2,
        flags: &[],
        positionals: &["REPO", "DIR"],
        run: cmd_repo::repo_check_local,
    },
    Verb {
        path: &["repo", "push"],
        class: Class::Write,
        usage: "agh repo push REPO DIR --input FILE [--dry-run]",
        summary: "push exact local objects atomically with expected refs, preserving the Git graph",
        example: "agh repo push alabsystems/aterm . --input updates.json --dry-run",
        noun: Noun::Write,
        gh: "git push --atomic --force-with-lease",
        milestone: 2,
        flags: &[
            value("--input", "a JSON array of {ref, before, after} objects"),
            DRY_RUN,
        ],
        positionals: &["REPO", "DIR"],
        run: cmd_repo::repo_push,
    },
    Verb {
        path: &["repo", "fetch"],
        class: Class::Git,
        usage: "agh repo fetch REPO DIR (--ref REF --into REF | --input FILE) [--anonymous]",
        summary: "fetch exact sources into agh's private namespace without moving other refs",
        example: "agh repo fetch alabsystems/aterm . --input refs.json --anonymous",
        noun: Noun::Repo,
        gh: "git fetch",
        milestone: 2,
        flags: &[
            value("--ref", "full ref or forty-digit object id"),
            value("--into", "destination beneath refs/agh/"),
            value(
                "--input",
                "1–2048 JSON rows: {source, into, expected_oid}; all OIDs must match",
            ),
        ],
        positionals: &["REPO", "DIR"],
        run: cmd_repo::repo_fetch,
    },
    Verb {
        path: &["repo", "fetch-main"],
        class: Class::Git,
        usage: "agh repo fetch-main REPO DIR",
        summary: "refresh refs/agh/fetched-main with a reflog; leave other refs and files untouched",
        example: "agh repo fetch-main alabsystems/aterm <dir> --meta",
        noun: Noun::Repo,
        gh: "git fetch",
        milestone: 2,
        flags: &[],
        positionals: &["REPO", "DIR"],
        run: cmd_repo::repo_fetch_main,
    },
    Verb {
        path: &["repo", "clone"],
        class: Class::Git,
        usage: "agh repo clone REPO [DIR] [--depth N | --mirror]",
        summary: "clone a repository through agh's isolated git runner and its credential helper",
        example: "agh repo clone alabsystems/aterm <dir> --depth 1",
        noun: Noun::Repo,
        gh: "gh repo clone",
        milestone: 2,
        flags: &[
            value("--depth", "a shallow clone of the last N commits"),
            on(
                "--mirror",
                "a complete bare mirror of every ref (default DIR: REPO.git)",
            ),
        ],
        positionals: &["REPO", "DIR?"],
        run: cmd_repo::repo_clone,
    },
    Verb {
        path: &["auth", "import"],
        class: Class::Provision,
        usage: "agh auth import NAME --from-gh [--user LOGIN] --tier dev|release|external [--owner OWNER]... [--write [--drafts]]",
        summary: "provision a profile from gh's stored login, with or without a terminal",
        example: "agh auth import <name> --from-gh --tier dev --owner <owner>",
        noun: Noun::Identity,
        gh: "gh auth token",
        milestone: 2,
        flags: &[
            on("--from-gh", "read gh's stored login for github.com"),
            value(
                "--user",
                "the gh login to import (default: gh's active one)",
            ),
            TIER,
            OWNER,
            WRITE,
            DRAFTS,
        ],
        positionals: &["NAME"],
        run: cmd_auth::auth_import,
    },
    Verb {
        path: &["auth", "git-credential"],
        class: Class::Git,
        usage: "agh auth git-credential get|store|erase",
        summary: "answer git's credential protocol for the run agh's git runner started",
        example: "agh auth git-credential get",
        noun: Noun::Credential,
        gh: "gh auth git-credential",
        milestone: 2,
        flags: &[],
        positionals: &["OP"],
        run: cmd_credential::auth_git_credential,
    },
    Verb {
        path: &["intent", "list"],
        class: Class::Local,
        usage: "agh intent list [REPO] [--open]",
        summary: "the write journal's intents, by repository, then step",
        example: "agh intent list alabsystems/aterm --open",
        noun: Noun::Intent,
        gh: "-",
        milestone: 2,
        flags: &[on("--open", "only the intents not yet settled")],
        positionals: &["REPO?"],
        run: cmd_intent::intent_list,
    },
    Verb {
        path: &["intent", "show"],
        class: Class::Local,
        usage: "agh intent show REPO ID",
        summary: "one intent of the write journal",
        example: "agh intent show alabsystems/aterm <id>",
        noun: Noun::Intent,
        gh: "-",
        milestone: 2,
        flags: &[],
        positionals: &["REPO", "ID"],
        run: cmd_intent::intent_show,
    },
    Verb {
        path: &["intent", "retract"],
        class: Class::Local,
        usage: "agh intent retract REPO ID --confirm ID",
        summary: "retract an open intent, its id typed again, once its write is known not to have landed",
        example: "agh intent retract alabsystems/aterm <id> --confirm <id>",
        noun: Noun::Intent,
        gh: "-",
        milestone: 2,
        flags: &[CONFIRM],
        positionals: &["REPO", "ID"],
        run: cmd_intent::intent_retract,
    },
    Verb {
        path: &["policy", "grant"],
        class: Class::Provision,
        usage: "agh policy grant CALLER REPO",
        summary: "grant a caller writes to one repository: a [[writers]] row in agh.toml",
        example: "agh policy grant <caller> alabsystems/aterm",
        noun: Noun::Policy,
        gh: "-",
        milestone: 2,
        flags: &[],
        positionals: &["CALLER", "REPO"],
        run: cmd_policy::policy_grant,
    },
    Verb {
        path: &["policy", "revoke"],
        class: Class::Provision,
        usage: "agh policy revoke CALLER REPO",
        summary: "take a caller's grant on one repository out of agh.toml",
        example: "agh policy revoke <caller> alabsystems/aterm",
        noun: Noun::Policy,
        gh: "-",
        milestone: 2,
        flags: &[],
        positionals: &["CALLER", "REPO"],
        run: cmd_policy::policy_revoke,
    },
    Verb {
        path: &["whoami"],
        class: Class::Local,
        usage: "agh whoami [--for REPO [--op read|writer-read|write]] [--verify] [--git URL]",
        summary: "every profile and ambient token, or with --for, how one request would be routed",
        example: "agh whoami --for alabsystems/aterm --op read",
        noun: Noun::Identity,
        gh: "gh auth status",
        milestone: 1,
        flags: &[
            value("--for", "the repository a request would be about"),
            value("--op", "read (the default), writer-read or write"),
            on(
                "--verify",
                "ask GitHub which login each present profile's token is; not with --for",
            ),
            value(
                "--git",
                "the identity the git helper would answer for this URL",
            ),
        ],
        positionals: &[],
        run: cmd_whoami::whoami,
    },
    Verb {
        path: &["auth", "status"],
        class: Class::Local,
        usage: "agh auth status",
        summary: "every profile, from agh.toml or the vault, and ambient token, with presence and faults",
        example: "agh auth status",
        noun: Noun::Policy,
        gh: "gh auth status",
        milestone: 1,
        flags: &[],
        positionals: &[],
        run: cmd_auth::auth_status,
    },
    Verb {
        path: &["auth", "add"],
        class: Class::Provision,
        usage: "agh auth add NAME (--file PATH | --stdin) --tier dev|release|external [--owner OWNER]... [--write [--drafts]] [--legacy-loose]",
        summary: "provision a profile from a token file or stdin, with or without a terminal",
        example: "agh auth add <name> --file <path> --tier dev --owner <owner>",
        noun: Noun::Identity,
        gh: "gh auth login",
        milestone: 1,
        flags: &[
            value(
                "--file",
                "the private token file, which agh.toml names in place; agh never copies it",
            ),
            on(
                "--stdin",
                "read the token from a pipe or file on stdin, never a terminal; agh keeps a copy",
            ),
            TIER,
            OWNER,
            WRITE,
            DRAFTS,
            on(
                "--legacy-loose",
                "accept a group- or world-readable --file (M2 only, for a builder's lanes)",
            ),
        ],
        positionals: &["NAME"],
        run: cmd_auth::auth_add,
    },
    Verb {
        path: &["auth", "remove"],
        class: Class::Provision,
        usage: "agh auth remove NAME",
        summary: "remove a profile, its references and the token file --stdin wrote; or forget a vault login",
        example: "agh auth remove <name>",
        noun: Noun::Policy,
        gh: "gh auth logout",
        milestone: 1,
        flags: &[],
        positionals: &["NAME"],
        run: cmd_auth::auth_remove,
    },
    Verb {
        path: &["policy", "check"],
        class: Class::Local,
        usage: "agh policy check [--config PATH]",
        summary: "check agh.toml against its rules",
        example: "agh policy check",
        noun: Noun::Policy,
        gh: "-",
        milestone: 1,
        flags: &[value("--config", "check this file instead of agh.toml")],
        positionals: &[],
        run: cmd_policy::policy_check,
    },
    Verb {
        path: &["can"],
        class: Class::Read,
        usage: "agh can REPO pull|push|admin",
        summary: "whether the routed identity holds pull, push or admin on a repository; exit 5 if not",
        example: "agh can alabsystems/aterm pull",
        noun: Noun::Permission,
        gh: "gh api repos/O/R --jq .permissions",
        milestone: 1,
        flags: &[],
        positionals: &["REPO", "OP"],
        run: cmd_can::can,
    },
    Verb {
        path: &["rate"],
        class: Class::Read,
        usage: "agh rate [--as PROFILE | --anonymous]",
        summary: "rate limits of each present profile and anonymous: GitHub's (no quota) and agh's rate book",
        example: "agh rate --anonymous",
        noun: Noun::Rate,
        gh: "gh api rate_limit",
        milestone: 1,
        flags: &[],
        positionals: &[],
        run: cmd_rate::rate,
    },
    Verb {
        path: &["api"],
        class: Class::ByMethod,
        usage: "agh api PATH [-X GET|HEAD|POST|PATCH|PUT|DELETE] [-f k=v]... [-F k=v]... [--input FILE] [--all] [--include] [--accept MEDIA] [-R REPO] [--confirm PATH] [--dry-run]",
        summary: "one request to any API path: a read, every page of one, or a write",
        example: "agh api /repos/alabsystems/aterm --jq .default_branch",
        noun: Noun::Raw,
        gh: "gh api",
        milestone: 1,
        flags: &[
            short(
                value("--method", "GET, HEAD, POST, PATCH, PUT or DELETE"),
                'X',
            ),
            short(
                repeat(
                    "--raw-field",
                    "a query pair k=v; a write's JSON body field: k[]=v an array element, k[m]=v an object member",
                ),
                'f',
            ),
            short(
                repeat(
                    "--field",
                    "the same as -f, but a write's body reads true, false, null and an integer as JSON; k=@file is refused",
                ),
                'F',
            ),
            on("--all", "every page; not with -X HEAD"),
            value(
                "--input",
                "a write's JSON body from a regular file, at most 8 MiB; not with -f/-F",
            ),
            on("--include", "the status and headers too; not with --all"),
            value("--accept", "the Accept media type"),
            value("--confirm", "a DELETE's path, typed again"),
            on(
                "--dry-run",
                "a write: say what would be sent, and send nothing",
            ),
        ],
        positionals: &["PATH"],
        run: cmd_api::api,
    },
    Verb {
        path: &["help"],
        class: Class::Local,
        usage: "agh help [VERB...]",
        summary: "the manual, a verb's page, the verbs under a word, or the exit, jq or routing page",
        example: "agh help release download",
        noun: Noun::Meta,
        gh: "gh help",
        milestone: 1,
        flags: &[],
        positionals: &["WORD…?"],
        run: cmd_meta::help,
    },
    Verb {
        path: &["schema"],
        class: Class::Local,
        usage: "agh schema [exit|nouns|verbs|NOUN]",
        summary: "the exit table, the nouns and the verbs as JSON, or one noun's row",
        example: "agh schema exit",
        noun: Noun::Meta,
        gh: "-",
        milestone: 1,
        flags: &[],
        positionals: &["WHAT?"],
        run: cmd_meta::schema,
    },
    Verb {
        path: &["version"],
        class: Class::Local,
        usage: "agh version",
        summary: "agh's version and the API version it pins",
        example: "agh version",
        noun: Noun::Meta,
        gh: "gh --version",
        milestone: 1,
        flags: &[],
        positionals: &[],
        run: cmd_meta::version,
    },
];

/// The global flags, in table order.
pub static GLOBAL_FLAGS: &[FlagSpec] = &[
    short(
        value(
            "--repo",
            "the repository, OWNER/NAME or @TARGET, in place of the first argument's REPO \
             (api: routes a /repositories/ID path)",
        ),
        'R',
    ),
    value("--as", "send as this profile, which the routing must allow"),
    value("--profile", "the same as --as"),
    on("--anonymous", "send no credential"),
    on(
        "--no-anonymous-api",
        "refuse anonymous API sends, including read-chain fallback; web and git reads remain available",
    ),
    value(
        "--timeout",
        "each request's time limit (release wait: the whole wait): 30s, 2m, 1h or seconds, \
         at most 24h",
    ),
    on("--meta", "always print the receipt line on stderr"),
    value("--format", "json (the default), ndjson, tsv or text"),
    on("--header", "a header row for tsv"),
    value("--fields", "the fields to print, comma-separated"),
    on("--raw", "print the one --fields value bare"),
    short(
        value(
            "--jq",
            "filter the answer with agh's jq subset (agh help jq)",
        ),
        'q',
    ),
    value("--limit", "at most N items"),
    on(
        "--partial",
        "accept a capped or truncated answer instead of exit 14",
    ),
    on(
        "--as-read",
        "the pages as read, not revalidated as one snapshot",
    ),
    value("--max-pages", "the page cap, 1 to 1000 (default 20)"),
    on(
        "--no-probe",
        "after a 404, skip the repository probe that tells not_found from not_visible",
    ),
    value("--record", "tape this run to a new file PATH"),
    value(
        "--replay",
        "run offline from the tape PATH (with no verb, the tape's own command line); exit 3 on \
         a divergence; with --record, re-record it",
    ),
    short(on("--help", "this verb's page, or the manual"), 'h'),
];

/// Every refused verb (exit 1).
pub static REFUSED: &[Refused] = &[
    Refused {
        path: &["auth", "login"],
        why: "agh has no device flow",
        next: "agh auth add NAME --file PATH --tier TIER",
    },
    Refused {
        path: &["auth", "token"],
        why: "agh never prints a credential",
        next: "agh auth status shows each profile's fingerprint",
    },
    Refused {
        path: &["secret"],
        why: "agh does not seal secrets",
        next: "use gh secret set",
    },
    Refused {
        path: &["workflow"],
        why: "agh does not dispatch workflows",
        next: "use gh workflow run",
    },
    Refused {
        path: &["repo", "delete"],
        why: "agh never deletes a repository",
        next: "a person deletes it on github.com",
    },
    Refused {
        path: &["pr"],
        why: "pr arrives in M4",
        next: "use gh pr for now",
    },
    Refused {
        path: &["issue"],
        why: "issue arrives in M4",
        next: "use gh issue for now",
    },
    Refused {
        path: &["run"],
        why: "run arrives in M4",
        next: "use gh run for now",
    },
    Refused {
        path: &["search"],
        why: "search arrives in M4",
        next: "use gh search for now",
    },
    Refused {
        path: &["gist"],
        why: "gist arrives in M4",
        next: "use gh gist for now",
    },
];

/// The verb with exactly this path.
#[must_use]
pub fn find(path: &[&str]) -> Option<&'static Verb> {
    VERBS.iter().find(|verb| verb.path == path)
}
