// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verb table used by the CLI parser, help renderer, schema output, and command dispatch.

use super::{
    Ctx, Outcome, args::Invocation, cmd_api, cmd_auth, cmd_can, cmd_content, cmd_git, cmd_meta,
    cmd_policy, cmd_rate, cmd_release, cmd_repo, cmd_wait, cmd_whoami,
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
}

impl Noun {
    /// Every noun, in declaration order.
    pub const ALL: [Noun; 20] = [
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
        usage: "agh release wait REPO TAG COND... [--include-drafts] [--timeout DUR]",
        summary: "wait until every COND holds on a release: 30m unless --timeout",
        example: "agh release wait alabsystems/aterm <tag> published --timeout 30s",
        noun: Noun::Wait,
        gh: "-",
        milestone: 1,
        flags: &[INCLUDE_DRAFTS],
        positionals: &["REPO", "TAG", "COND…"],
        run: cmd_wait::release_wait,
    },
    Verb {
        path: &["whoami"],
        class: Class::Local,
        usage: "agh whoami [--for REPO [--op read|writer-read|write]] [--verify]",
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
        usage: "agh auth add NAME (--file PATH | --stdin) --tier dev|release|external [--owner OWNER]... [--write [--drafts]]",
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
            value("--tier", "dev, release or external"),
            repeat(
                "--owner",
                "an owner the profile reads for; give it once per owner",
            ),
            on("--write", "make it those owners' writer"),
            on(
                "--drafts",
                "vouch that this token may read drafts (needs --write)",
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
        usage: "agh api PATH [-X GET|HEAD] [-f k=v]... [-F k=v]... [--all] [--include] [--accept MEDIA] [-R REPO]",
        summary: "one read of any API path, or every page of it",
        example: "agh api /repos/alabsystems/aterm --jq .default_branch",
        noun: Noun::Raw,
        gh: "gh api",
        milestone: 1,
        flags: &[
            short(value("--method", "GET (the default) or HEAD"), 'X'),
            short(repeat("--raw-field", "a query pair k=v"), 'f'),
            short(repeat("--field", "the same as -f; k=@file is refused"), 'F'),
            on("--all", "every page; not with -X HEAD"),
            on("--include", "the status and headers too; not with --all"),
            value("--accept", "the Accept media type"),
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
    value(
        "--record",
        "tape this run to a new file PATH: anonymous GitHub reads, not release download",
    ),
    value(
        "--replay",
        "run offline from the tape PATH (with no verb, the tape's own command line); exit 3 on \
         a divergence; with --record, re-record it",
    ),
    short(on("--help", "this verb's page, or the manual"), 'h'),
];

const WRITES: &str = "writes arrive in M2";
const USE_GH: &str = "use gh for now";

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
        path: &["auth", "import"],
        why: "importing a gh login arrives in M2",
        next: "agh auth add NAME --stdin --tier TIER < FILE",
    },
    Refused {
        path: &["auth", "git-credential"],
        why: "the git credential helper arrives in M2",
        next: "use gh for git until M2",
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
        path: &["repo", "clone"],
        why: "agh does not clone; its git helper arrives in M2",
        next: "git clone https://github.com/OWNER/NAME",
    },
    Refused {
        path: &["repo", "create"],
        why: WRITES,
        next: USE_GH,
    },
    Refused {
        path: &["release", "create"],
        why: WRITES,
        next: USE_GH,
    },
    Refused {
        path: &["release", "upload"],
        why: WRITES,
        next: USE_GH,
    },
    Refused {
        path: &["release", "edit"],
        why: WRITES,
        next: USE_GH,
    },
    Refused {
        path: &["release", "publish"],
        why: WRITES,
        next: USE_GH,
    },
    Refused {
        path: &["release", "ensure"],
        why: WRITES,
        next: USE_GH,
    },
    Refused {
        path: &["release", "delete"],
        why: WRITES,
        next: USE_GH,
    },
    Refused {
        path: &["asset"],
        why: WRITES,
        next: USE_GH,
    },
    Refused {
        path: &["ref"],
        why: WRITES,
        next: USE_GH,
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
