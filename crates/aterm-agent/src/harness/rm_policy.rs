// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Is this Bash command an `rm` the owner's policy approves? (design §5.1)
//!
//! [`evaluate`] is a pure function of four values — the command line the vendor
//! hook carries in `tool_input.command`, the session's working directory, which
//! hook event asked, and the owner's [`RmPolicy`] — and it answers
//! [`RmDecision::Allow`] or [`RmDecision::Abstain`]. There is no third answer:
//! **the harness never denies.** Abstaining means the host says nothing on the
//! vendor's decision channel, so the vendor's own prompt shows and a person
//! decides. Every rule below therefore has one failure direction, and every tie
//! breaks toward it: a false `Abstain` costs the owner one keypress, a false
//! `Allow` deletes something.
//!
//! # What is reused, and what is added
//!
//! The shell reading is [`crate::supervise::classify`]'s, not a fresh split on
//! `&&`, `||`, `;`, `|` (design §0.1): `strip_quotes` and `split_segments`
//! give the segments the shell will run, `raw_words` gives the same line with
//! its quotes RESOLVED (what `rm` is really handed), and `danger_scan`,
//! `segment_head` and `program_scan` are the read-only judgment, applied to
//! every segment that is not an `rm`. Nine items of that module became
//! `pub(crate)` for this (those six, `program`, `redirect_target`,
//! `redirect_is_safe`); none changed behaviour. What this module adds is only
//! what is specific to `rm`: which segments are a DIRECT `rm`, its flags, its
//! redirects, where its operands point, and what may stand beside it.
//!
//! The two readings of one line are compared token for token before any operand
//! is believed (`align`): where the quote-stripped tokens and the quote-resolved
//! words disagree about how many words a segment has, or about the text outside
//! the quotes, the verdict is `Abstain`. The one divergence the two readers are
//! known to have — `raw_words` ends a segment at the `&` of `2>&1`, the
//! splitter does not — is modelled rather than forbidden, because `2>&1` on an
//! `rm` is ordinary.
//!
//! # The rules, in the order they decide
//!
//! 1. **Mode.** [`RmMode::Off`] abstains on everything.
//!    [`RmMode::PromptOnly`] (the default) answers
//!    [`HookEvent::PermissionRequest`] only; [`RmMode::All`] answers
//!    [`HookEvent::PreToolUse`] too. See "Reading §5.1" below.
//! 2. **The line is small and plain.** Longer than [`MAX_LINE_BYTES`], a
//!    control character other than newline or tab, or whitespace outside ASCII
//!    (Rust splits on it, the shell does not): abstain. So does any `$(`,
//!    backtick, subshell or group — the segmentation cannot say which words
//!    such a construct contributes to the `rm` around it (`rm -rf x$(echo) cat
//!    /etc` reads as three harmless segments and is one `rm` of three
//!    operands). And so does anything that lets the shell's idea of "inside
//!    quotes" part from classify's, judged on the raw text, quoted or not: a
//!    `#` (a comment's apostrophe opens a quote for this reader that the shell
//!    never opened, and the NEXT LINE vanishes into it), `<<` (a here-document
//!    body, likewise), and `$'`, `$"`, `${`. See "What this slice found in
//!    classify" below: those lines are in the corpus.
//! 3. **The prefix is sane.** With [`CwdPrefix::SessionCwd`] or
//!    [`CwdPrefix::Dir`] the prefix must be absolute, free of `..`, and not
//!    "too wide": the filesystem root, `/Users`, `/home`, a direct child of
//!    either, or the injected home directory or any ancestor of it. A session
//!    started in `~` does not get every `rm` under `~` approved.
//! 4. **Every segment is an `rm` or a read.** Segments with no `rm` token go
//!    through classify's three filters unchanged, and must then also start
//!    with one of `COMPANIONS` — a plain subset of classify's read-only list,
//!    by bare name or under `/bin/`, `/usr/bin/` (classify takes a basename, so
//!    `/tmp/x/ls` is `ls` to it). An approval here answers for the whole line,
//!    and that list holds programs that run other programs. On top, because a
//!    read that is harmless alone can change what a LATER `rm` means: `cd`,
//!    `export`, `local`, `let`, or a leading `NAME=value`, abstain by name
//!    (`cd /etc && rm -rf x` would resolve against the wrong directory;
//!    `PATH=/x; rm y` runs a different `rm`). `cd <dir> && rm …` is the most
//!    common shape this gives up; tracking `cd` is left to a later slice
//!    because `;` versus `&&` and a failed `cd` change the answer and the
//!    segmentation does not keep the separator. Three more, for the same
//!    reason or because classify lets them through: a compound command (`for
//!    PATH in /x; do …` assigns) and `printf -v`; a `>&word` redirect, which
//!    writes a file and which classify's `redirect_is_safe` accepts; and a
//!    segment whose head starts with a flag or holds a redirect, which
//!    classify reads as the tail of a `\( … \)` group — on a line with no
//!    groups it is `rm>/dev/null -rf /`, an `rm` with no `rm` token.
//! 5. **`rm` is direct.** A segment containing an `rm` token must START with
//!    `rm`, `/bin/rm` or `/usr/bin/rm`. `sudo rm`, `xargs rm`, `find … -exec
//!    rm`, `env rm`, `timeout 5 rm`, `command rm`, `git rm`, `then rm`, a
//!    leading assignment, and `./rm` all abstain with the head named; `find
//!    -delete`, `bash -c`, `sh -c` and `eval "…"` carry no `rm` token at all
//!    and abstain under rule 4 with classify's own reason (`find -delete`,
//!    `bash -c`, `eval`). The alarm idiom classify strips is NOT stripped here,
//!    so a line carrying it abstains.
//! 6. **Arguments are understood.** Flags are read from the quote-resolved word
//!    (quotes do not hide `-rf` from `rm`): short clusters over `dfiIPrRvx` and
//!    the GNU long forms; anything else abstains. `--no-preserve-root` is a
//!    deny pattern. Redirects are read from the quote-stripped token (that is
//!    what the shell sees) and must be to `/dev/null` or a descriptor. Since
//!    2026-09-23 classify's splitter detaches a redirect glued to a word the
//!    way the shell does (`/etc/x>/dev/null` is the operand `/etc/x` and a
//!    redirect), so the operand is judged like any other; the glued-redirect
//!    abstention below stays as a guard. A backslash or a `<` in an `rm`
//!    segment abstains.
//! 7. **Operands resolve, lexically.** These can NOT be resolved without a
//!    shell and always abstain, whatever the deny list says — removing them
//!    would not widen the policy, it would make the resolver wrong: any `$`
//!    (`$HOME`, `$VAR`; `${…}`, `$'…'` and a backtick already fell to rule 2),
//!    a leading `~`, a leading `=` (zsh's `=cmd` is the path of `cmd`), `{`/`}`
//!    (brace expansion can spell `..`), any `..` component (behind a symlink,
//!    `a/../b` is not `b`),
//!    a glob component that starts with `.` (in bash `.*` matches `..`), and
//!    the empty word. A quoted `'$x'` is a literal to the shell and abstains
//!    anyway: the resolved word no longer says it was quoted.
//! 8. **Deny patterns** ([`DenyPattern`]) — owner policy, removable.
//! 9. **The prefix.** A target must sit STRICTLY inside the prefix; the prefix
//!    directory itself abstains.
//!
//! # Lexical means lexical
//!
//! Nothing here touches the filesystem. A target is "inside" when its
//! components, after dropping `.` and empty ones, extend the prefix's. **A
//! symlink escape is outside a lexical check**: `rm -rf build/cache/x` is
//! approved when `build/cache` is a link to somewhere else, exactly as the
//! vendor's own rule matcher would. Comparison is byte-exact for the prefix
//! (a differently-cased or differently-normalised spelling of the same macOS
//! path abstains) and ASCII-case-insensitive for the deny patterns (a
//! differently-cased spelling is still denied).
//!
//! # Reading §5.1
//!
//! §5.1 names the knob `rm.approve = prompt-only | all | off` and does not
//! define the values. The reading taken here: `PermissionRequest` "fires only
//! when a prompt would appear" (the design's words), which is what
//! "prompt-only" names, and the design's one example row pairs
//! `"event":"PermissionRequest"` with `"reason":"policy:rm.approve=prompt-only"`.
//! So `prompt-only` answers that event alone, and the `PreToolUse` "belt" —
//! which fires for every Bash call, prompt or not — is answered only under
//! `all`. The design does not say otherwise, and where it is silent the tie
//! goes to not approving.
//!
//! The Allow reason is the design's string, `policy:rm.approve=<mode>`. An
//! Abstain reason is `<family>:<rule>`, the family being one of `mode`, `line`,
//! `cwd`, `prefix`, `segment`, `wrapper`, `rm`, `unresolvable`, `deny`.
//!
//! # Where this departs from the design text
//!
//! * §5.1 lists `~`, `$HOME`, `..` and "any `$VAR` target" as default
//!   `deny_patterns`, which would make them removable. They are rule 7 here.
//! * Two defaults are ADDED to the deny list, each removable: a bare glob
//!   directly in the prefix (`rm -rf *`) and a `.git` component. New as fixed
//!   rules, not patterns: the too-wide prefix of rule 3, and rule 4's
//!   companion list, which is narrower than "read-only by classify's rules".
//! * `rm.budget` and `rm.persist_rule` are not fields: a budget needs a clock
//!   and belongs to the actuator, and `persist_rule` shapes the hook reply, not
//!   the verdict.
//!
//! # What this slice found in classify
//!
//! MEASURED while writing the corpus, by calling `classify_command` on each:
//! it answers `read_only = true` for `ls # it's⏎rm -rf / # '`, for
//! `cat <<EOF⏎echo '⏎EOF⏎rm -rf /`, for `echo $'a\'b'⏎rm -rf /⏎echo '`, for
//! `rm>/dev/null -rf /`, and for `ls >&out.txt` (⏎ is a newline). The first
//! three are one defect — the quote reader does not know comments, here-document
//! bodies or `$'…'` — and the supervisor that auto-approves reads inherits it.
//! It also answers `true` for five lines that can run a program of the line's
//! choosing or assign a variable: `git -c core.fsmonitor='…' status`, `rg --pre
//! ./x.sh foo`, `sort --compress-program=./x.sh f`, `/tmp/evil/ls`, `printf -v
//! PATH /evil` — the reason rule 4 has a companion list. This slice was allowed
//! to change that module's visibility only, so rules 2 and 4 refuse those
//! shapes here, and the lines are pinned in this module's corpus so that an
//! `rm` verdict cannot regress with them. (Fixed in classify itself on
//! 2026-09-23, audit APR-4: several of those lines now abstain on classify's
//! own reason, which the corpus below records.)
//!
//! # MEASURED versus assumed
//!
//! MEASURED, by this module's tests on the machine that ran them: the verdict
//! and the deciding rule for each line of the corpus below, and that a 1 MiB
//! line is answered without being parsed. ASSUMED, not measured: everything
//! about shells (that bash and zsh expand only what rule 7 lists in an operand
//! position, with default options; that the vendor runs the line unmodified in
//! one of them, in the `cwd` the hook reports; that `rm` on the session's PATH
//! is the system `rm` and not an alias or function; that no program in
//! `COMPANIONS` runs another or writes a file by a flag classify does not
//! know), BSD and GNU `rm` flag sets as their manuals state them,
//! and every vendor hook semantic §5.1 itself labels as not settled.
//!
//! Deliberately absent: the hook reply JSON, the `PostToolUse` row, any rate
//! budget, `cd` tracking, every wrapper, and a derived model — this is a
//! decision function, not a bounded machine, and §11 owes it none.
//!
//! STATUS: unit-tested; not wired into any shipped verb.

use std::path::{Path, PathBuf};

use crate::supervise::classify::{
    DEFAULT_PYTHON_ALLOW, danger_scan, glob_match, program, program_scan, raw_words,
    redirect_is_safe, redirect_target, segment_head, split_segments, strip_quotes,
};

/// A command line longer than this is not parsed: the verdict is `Abstain`.
/// classify's scans are quadratic in the worst case (every `sort` token rescans
/// the rest of its segment), and an `rm` a person would approve is short.
pub const MAX_LINE_BYTES: usize = 16 * 1024;

/// An `rm` line with more operands than this abstains.
pub const MAX_TARGETS: usize = 256;

/// The hard ceiling on one ledger row (design §4.4's quota for rows fed by
/// agent-controlled input). [`to_ledger_json`] shrinks the text fields until the
/// row fits.
pub const MAX_ROW_BYTES: usize = 4096;

/// The programs that may run BESIDE an `rm` on an approved line, by bare name
/// or under `/bin/` or `/usr/bin/`. A subset of classify's read-only list on
/// purpose: an approval here answers for the WHOLE line, and that list holds
/// programs that run other programs (`git -c core.fsmonitor=…`, `rg --pre`,
/// `sort --compress-program`, `find`, `xargs`, `env`, the python allowlist) or
/// whose arguments take a parser of their own to judge (`sed`, `awk`). classify
/// still judges every one of these segments; this list only narrows it.
const COMPANIONS: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
    "grep",
    "egrep",
    "fgrep",
    "wc",
    "echo",
    "printf",
    "stat",
    "which",
    "type",
    "df",
    "du",
    "uptime",
    "ps",
    "date",
    "pwd",
    "true",
    "test",
    "[",
    "[[",
    "diff",
    "comm",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "nl",
    "md5",
    "shasum",
    "sha256sum",
    "column",
    "cut",
    "tr",
    "uniq",
    "paste",
    "seq",
    "expr",
];

/// `rm.approve` (design §5.1 "Knobs").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RmMode {
    /// Answer [`HookEvent::PermissionRequest`] only. The default.
    #[default]
    PromptOnly,
    /// Answer [`HookEvent::PreToolUse`] as well.
    All,
    /// Answer nothing.
    Off,
}

impl RmMode {
    /// The config spelling: `prompt-only`, `all`, `off`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PromptOnly => "prompt-only",
            Self::All => "all",
            Self::Off => "off",
        }
    }

    /// The inverse of [`Self::as_str`]; anything else is `None`, and a caller
    /// that cannot read its config should fall back to [`RmMode::Off`].
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "prompt-only" => Some(Self::PromptOnly),
            "all" => Some(Self::All),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// Which vendor hook event is asking (design §5.1 "Mechanism").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    /// Fires when the vendor is about to show a permission prompt.
    PermissionRequest,
    /// Fires before every tool call, prompt or not.
    PreToolUse,
}

impl HookEvent {
    /// The vendor's spelling, as it appears in the hook JSON and the ledger.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PermissionRequest => "PermissionRequest",
            Self::PreToolUse => "PreToolUse",
        }
    }

    /// The inverse of [`Self::as_str`].
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "PermissionRequest" => Some(Self::PermissionRequest),
            "PreToolUse" => Some(Self::PreToolUse),
            _ => None,
        }
    }
}

/// `rm.require_cwd_prefix`: the directory every target must sit strictly
/// inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CwdPrefix {
    /// The `cwd` handed to [`evaluate`] — the session's. The default.
    SessionCwd,
    /// This absolute directory instead (a repo root above a session that runs
    /// in a subdirectory, say). Relative targets still resolve against `cwd`.
    Dir(PathBuf),
    /// No prefix requirement: rules 3 and 9 are both off, and only rule 7
    /// (the shape rules — globs, `..`, the unresolvable line) and the deny
    /// patterns are left.
    ///
    /// READ THIS BEFORE SETTING IT. The default deny patterns are narrow:
    /// [`DenyPattern::Home`] matches the home directory itself, an ancestor
    /// of it, and a glob DIRECTLY inside it — one level deeper is not
    /// matched. MEASURED under `Off` with the shipped deny list and a home
    /// of `/Users//_owner`: `rm -rf /etc/passwd`, `rm -rf /usr/local`,
    /// `rm -rf /System/Library`, `rm -rf /bin`, `rm -rf /var`,
    /// `rm -rf /Applications`, `rm -rf /Users//_owner/.ssh` and
    /// `rm -rf /Users//_owner/Documents` all come back [`Verdict::Allow`],
    /// while `rm -rf /` and `rm -rf /Users//_owner/*` stay abstentions. So
    /// `Off` does not mean "no directory requirement" in practice, it means
    /// "auto-approve the whole filesystem but a handful of shapes": give it
    /// [`RmPolicy::deny_patterns`] of your own, or do not set it. Whether the
    /// system roots belong in the SHIPPED deny list is an owner decision
    /// (design §5.1) this module does not take on its own.
    Off,
}

/// One removable refusal (`rm.deny_patterns`). Paths are compared after lexical
/// resolution, ASCII-case-insensitively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyPattern {
    /// The target is `/`.
    FilesystemRoot,
    /// The target is [`RmPolicy::home`], an ancestor of it, or a glob directly
    /// inside it. Inert while `home` is `None`.
    Home,
    /// `/Users`, `/home`, or a direct child of either (somebody's home).
    UsersDir,
    /// The `--no-preserve-root` flag.
    NoPreserveRoot,
    /// A glob in the first component of an absolute target (`/*`, `/u*/x`).
    GlobAtRoot,
    /// A target directly in the prefix whose name is only `*` and `?`
    /// (`rm -rf *`, `rm -rf ./*`): it empties the prefix directory.
    BareGlobAtPrefix,
    /// Any component of the target equals this name (`.git` by default).
    Component(String),
    /// [`glob_match`] over the resolved path, or over the word as written.
    Glob(String),
}

/// The owner's `rm` policy. `Default` is the design's default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RmPolicy {
    /// `rm.approve`.
    pub mode: RmMode,
    /// `rm.deny_patterns`: the removable refusals. The refusals of module-doc
    /// rule 7 are not in this list and cannot be removed.
    pub deny_patterns: Vec<DenyPattern>,
    /// `rm.require_cwd_prefix`.
    pub require_cwd_prefix: CwdPrefix,
    /// The owner's home directory, INJECTED by the host (this module reads no
    /// environment). `None` leaves [`DenyPattern::Home`] and the home half of
    /// the too-wide-prefix rule inert; `/Users//<x>` and `/home/<x>` are still
    /// caught by shape.
    pub home: Option<PathBuf>,
}

impl Default for RmPolicy {
    fn default() -> Self {
        Self {
            mode: RmMode::PromptOnly,
            deny_patterns: vec![
                DenyPattern::FilesystemRoot,
                DenyPattern::Home,
                DenyPattern::UsersDir,
                DenyPattern::NoPreserveRoot,
                DenyPattern::GlobAtRoot,
                DenyPattern::BareGlobAtPrefix,
                DenyPattern::Component(".git".to_string()),
            ],
            require_cwd_prefix: CwdPrefix::SessionCwd,
            home: None,
        }
    }
}

/// The two answers. There is no `Deny`: see the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RmDecision {
    /// Answer `allow` on the vendor's decision channel.
    Allow,
    /// Answer nothing; the vendor's prompt shows.
    Abstain,
}

impl RmDecision {
    /// The ledger spelling: `allow`, `abstain`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Abstain => "abstain",
        }
    }
}

/// The verdict on one command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RmVerdict {
    /// Allow or abstain.
    pub decision: RmDecision,
    /// The rule that decided: `policy:rm.approve=<mode>` for an allow,
    /// `<family>:<rule>` for an abstain (module doc, "Reading §5.1").
    pub reason: String,
    /// Each `rm` operand's lexical resolution, in line order. On an allow that
    /// is every operand. On an abstain it is the operands resolved BEFORE the
    /// rule decided, the offending one included when it resolves at all — so it
    /// may be empty, and it is never the basis of an approval.
    pub targets: Vec<String>,
}

/// Judge `cmd` as it would run in `cwd`, for `event`, under `policy`.
///
/// Pure: no filesystem, no environment, no clock. `cwd` must be absolute.
pub fn evaluate(cmd: &str, cwd: &Path, event: HookEvent, policy: &RmPolicy) -> RmVerdict {
    let mut targets = Vec::new();
    match judge(cmd, cwd, event, policy, &mut targets) {
        Ok(()) => RmVerdict {
            decision: RmDecision::Allow,
            reason: format!("policy:rm.approve={}", policy.mode.as_str()),
            targets,
        },
        Err(reason) => RmVerdict {
            decision: RmDecision::Abstain,
            reason,
            targets,
        },
    }
}

/// `Ok` is an allow; `Err` carries the abstain reason.
fn judge(
    cmd: &str,
    cwd: &Path,
    event: HookEvent,
    policy: &RmPolicy,
    targets: &mut Vec<String>,
) -> Result<(), String> {
    // Rule 1.
    match (policy.mode, event) {
        (RmMode::Off, _) => return Err("mode:rm.approve=off".to_string()),
        (RmMode::PromptOnly, HookEvent::PreToolUse) => {
            return Err("mode:rm.approve=prompt-only answers PermissionRequest only".to_string());
        }
        (RmMode::PromptOnly, HookEvent::PermissionRequest) | (RmMode::All, _) => {}
    }

    // Rule 2.
    if cmd.len() > MAX_LINE_BYTES {
        return Err(format!("line:longer than {MAX_LINE_BYTES} bytes"));
    }
    if cmd
        .chars()
        .any(|c| (c.is_control() && c != '\n' && c != '\t') || (c.is_whitespace() && !c.is_ascii()))
    {
        return Err("line:control or non-ASCII whitespace character".to_string());
    }
    // Where the shell's idea of "inside quotes" and classify's can part: a
    // comment, a here-document body and `$'…'` each let an apostrophe open a
    // quote for this reader that the shell never opened, and everything up to
    // the next apostrophe — a second command line, say — is then invisible
    // here and executed there. Judged on the RAW text, quoted or not.
    if cmd.contains('#') {
        return Err(
            "line:a # can start a comment, which hides quotes from this reader".to_string(),
        );
    }
    if cmd.contains("<<") {
        return Err("line:a here-document hides its body from this reader".to_string());
    }
    if cmd.contains("$'") || cmd.contains("$\"") || cmd.contains("${") {
        return Err("line:${…}, $'…' or $\"…\" is not modelled".to_string());
    }
    let stripped = strip_quotes(cmd);
    if cmd.contains("$(") || cmd.contains('`') || stripped.contains(['(', ')']) {
        return Err("line:command substitution, subshell or group".to_string());
    }
    let segments = split_segments(&stripped);
    if segments.is_empty() {
        return Err("line:empty command".to_string());
    }

    // Rule 3.
    let Some(cwd) = cwd.to_str().and_then(absolute_components) else {
        return Err("cwd:not an absolute UTF-8 path free of ..".to_string());
    };
    let home = match &policy.home {
        Some(h) => match h.to_str().and_then(absolute_components) {
            Some(h) => Some(h),
            None => return Err("prefix:home is not an absolute UTF-8 path free of ..".to_string()),
        },
        None => None,
    };
    let prefix = match &policy.require_cwd_prefix {
        CwdPrefix::SessionCwd => Some(cwd.clone()),
        CwdPrefix::Dir(d) => match d.to_str().and_then(absolute_components) {
            Some(d) => Some(d),
            None => return Err("prefix:not an absolute UTF-8 path free of ..".to_string()),
        },
        CwdPrefix::Off => None,
    };
    if let Some(p) = &prefix
        && (p.is_empty() || is_users_dir(p) || home.as_ref().is_some_and(|h| is_prefix_of(p, h)))
    {
        return Err(format!("prefix:too wide ({})", clip(&join(p))));
    }

    // Rule 4.
    let (rm_segments, others): (Vec<usize>, Vec<Vec<String>>) = {
        let mut rm = Vec::new();
        let mut others = Vec::new();
        for (i, seg) in segments.iter().enumerate() {
            if seg.iter().any(|t| program(t) == "rm") {
                rm.push(i);
            } else {
                others.push(seg.clone());
            }
        }
        (rm, others)
    };
    let not_a_read = |r: String| format!("segment:not rm and not read-only ({})", clip(&r));
    if let Some(r) = danger_scan(&others) {
        return Err(not_a_read(r));
    }
    if let Some(r) = program_scan(cmd) {
        return Err(not_a_read(r));
    }
    for seg in &others {
        if let Some(r) = segment_head(seg, DEFAULT_PYTHON_ALLOW) {
            return Err(not_a_read(r));
        }
        let head = seg.first().map_or("", String::as_str);
        if head.contains('=') {
            return Err("segment:an assignment on an rm line".to_string());
        }
        // classify reads a head that starts with a flag or holds a redirect as
        // the tail of a `\( … \)` group and waves it through. No group survives
        // rule 2, so here it is `rm>/dev/null -rf /`: an rm with no `rm` token.
        if head.starts_with('-') || head.contains(['<', '>']) {
            return Err("segment:starts with a redirect or a flag".to_string());
        }
        // A compound command can assign (`for PATH in /x; do …`), and so can
        // `printf -v`; what a loop or a branch does to a LATER rm is not tracked.
        if matches!(
            head,
            "for"
                | "do"
                | "done"
                | "if"
                | "then"
                | "else"
                | "elif"
                | "fi"
                | "while"
                | "until"
                | "!"
                | "{"
        ) {
            return Err(format!("segment:compound command ({head}) on an rm line"));
        }
        if matches!(head, "cd" | "export" | "local" | "let") {
            return Err(format!(
                "segment:{head} changes what a later rm means; this slice does not track it"
            ));
        }
        if seg.iter().any(|t| program(t) == "printf") && seg.iter().any(|t| t.starts_with("-v")) {
            return Err("segment:printf -v assigns a variable".to_string());
        }
        for (i, tok) in seg.iter().enumerate() {
            if let Some(target) = redirect_target(tok) {
                let target = target.or(seg.get(i + 1).map(String::as_str));
                if !target.is_some_and(redirect_is_harmless) {
                    return Err(format!(
                        "segment:redirect > {}",
                        clip(target.unwrap_or("(no target)"))
                    ));
                }
            }
        }
        let bare = head
            .strip_prefix("/usr/bin/")
            .or_else(|| head.strip_prefix("/bin/"))
            .unwrap_or(head);
        if !COMPANIONS.contains(&bare) {
            return Err(format!(
                "segment:{} is not a companion this slice approves beside rm",
                clip(head)
            ));
        }
    }
    if rm_segments.is_empty() {
        return Err("line:no rm invocation".to_string());
    }

    // Rule 5, and the two token shapes rule 6 refuses outright — decided
    // before the readings are aligned, so the reason names the cause (an
    // escaped space is also a disagreement between the readers) and not its
    // symptom.
    for &si in &rm_segments {
        let seg = &segments[si];
        let head = seg.first().map_or("", String::as_str);
        if !matches!(head, "rm" | "/bin/rm" | "/usr/bin/rm") {
            return Err(if head.contains('=') {
                "rm:an assignment before rm".to_string()
            } else if program(head) == "rm" {
                format!("rm:reached by an unrecognised path ({})", clip(head))
            } else {
                format!(
                    "wrapper:rm reached through {}; this slice approves a direct rm only",
                    clip(program(head))
                )
            });
        }
        if seg.iter().any(|t| t.contains('\\')) {
            return Err("rm:a backslash escape in an rm argument".to_string());
        }
        if seg.iter().any(|t| t.contains('<')) {
            return Err("rm:an input redirect on rm".to_string());
        }
    }

    let raw = raw_words(cmd);
    let Some(map) = align(&segments, &raw) else {
        return Err("line:the quote-stripped and quote-resolved readings disagree".to_string());
    };

    for &si in &rm_segments {
        let seg = &segments[si];
        // Rule 6.
        let mut operands: Vec<&str> = Vec::new();
        let mut after_double_dash = false;
        let mut k = 1;
        while k < seg.len() {
            let tok = seg[k].as_str();
            if let Some(target) = redirect_target(tok) {
                let before = tok.split('>').next().unwrap_or("");
                if !(before.is_empty()
                    || before == "&"
                    || before.bytes().all(|b| b.is_ascii_digit()))
                {
                    return Err("rm:a redirect glued to an argument".to_string());
                }
                let target = match target {
                    Some(t) => Some(t),
                    None => {
                        k += 1;
                        seg.get(k).map(String::as_str)
                    }
                };
                if !target.is_some_and(redirect_is_harmless) {
                    return Err(format!(
                        "rm:redirect > {}",
                        clip(target.unwrap_or("(no target)"))
                    ));
                }
                k += 1;
                continue;
            }
            // What rm is handed: the quote-resolved word at the same position.
            let Some(word) = map
                .get(si)
                .and_then(|row| row.get(k))
                .copied()
                .flatten()
                .and_then(|(rs, rt)| raw.get(rs)?.get(rt))
                .map(String::as_str)
            else {
                return Err(
                    "line:the quote-stripped and quote-resolved readings disagree".to_string(),
                );
            };
            k += 1;
            if !after_double_dash && word == "--" {
                after_double_dash = true;
            } else if !after_double_dash && word.len() > 1 && word.starts_with('-') {
                if word == "--no-preserve-root" {
                    if policy.deny_patterns.contains(&DenyPattern::NoPreserveRoot) {
                        return Err("deny:--no-preserve-root".to_string());
                    }
                } else if !is_known_flag(word) {
                    return Err(format!("rm:unrecognised flag {}", clip(word)));
                }
            } else {
                operands.push(word);
            }
        }
        if operands.is_empty() {
            return Err("rm:no target".to_string());
        }

        for word in operands {
            if targets.len() >= MAX_TARGETS {
                return Err(format!("rm:more than {MAX_TARGETS} targets"));
            }
            // Rule 7.
            if let Some(what) = unresolvable(word) {
                return Err(format!("unresolvable:{what}"));
            }
            let comps = resolve(&cwd, word);
            let path = join(&comps);
            targets.push(path.clone());
            // Rule 8.
            for pattern in &policy.deny_patterns {
                if let Some(name) = denied(
                    pattern,
                    &comps,
                    &path,
                    word,
                    prefix.as_deref(),
                    home.as_deref(),
                ) {
                    return Err(format!("deny:{}", clip(&name)));
                }
            }
            // Rule 9.
            if let Some(p) = &prefix {
                if comps == *p {
                    return Err("prefix:the target is the prefix directory itself".to_string());
                }
                if !(comps.len() > p.len() && comps[..p.len()] == p[..]) {
                    return Err(format!(
                        "prefix:{} is outside {}",
                        clip(&path),
                        clip(&join(p))
                    ));
                }
            }
        }
    }
    Ok(())
}

/// A redirect target that writes no file: `/dev/null`, a descriptor (`&1`), or
/// a close (`&-`). Narrower than classify's `redirect_is_safe`, which accepts
/// any `&word` — and `>&out.txt` is bash for "stdout and stderr into out.txt".
fn redirect_is_harmless(target: &str) -> bool {
    redirect_is_safe(target)
        && (target == "/dev/null"
            || target.strip_prefix('&').is_some_and(|fd| {
                fd == "-" || (!fd.is_empty() && fd.bytes().all(|b| b.is_ascii_digit()))
            }))
}

/// BSD and GNU `rm` flags that only change HOW the named operands are removed.
/// `-W` (BSD undelete) and `--help`/`--version` are not deletions and are left
/// unrecognised.
pub(crate) fn is_known_flag(word: &str) -> bool {
    if let Some(long) = word.strip_prefix("--") {
        return matches!(
            long,
            "force"
                | "recursive"
                | "dir"
                | "verbose"
                | "one-file-system"
                | "preserve-root"
                | "preserve-root=all"
                | "interactive"
                | "interactive=never"
                | "interactive=once"
                | "interactive=always"
        );
    }
    match word.strip_prefix('-') {
        Some(cluster) if !cluster.is_empty() => cluster.chars().all(|c| "dfiIPrRvx".contains(c)),
        _ => false,
    }
}

pub(crate) fn has_glob(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// Module-doc rule 7: why this word cannot be resolved without a shell.
fn unresolvable(word: &str) -> Option<&'static str> {
    if word.is_empty() {
        return Some("an empty target");
    }
    if word.contains("$HOME") {
        return Some("$HOME in a target");
    }
    if word.contains('$') {
        return Some("an unexpanded $ in a target");
    }
    if word.starts_with('~') {
        return Some("~ in a target");
    }
    if word.starts_with('=') {
        return Some("a leading = in a target (zsh expands =cmd to a path)");
    }
    if word.contains(['{', '}']) {
        return Some("brace expansion in a target");
    }
    for comp in word.split('/') {
        if comp == ".." {
            return Some("a .. component in a target");
        }
        if comp.starts_with('.') && has_glob(comp) {
            return Some("a glob that can match .. in a target");
        }
    }
    None
}

/// The components of an absolute path with `.` and empty ones dropped; `None`
/// for a relative path or one with a `..`.
fn absolute_components(path: &str) -> Option<Vec<String>> {
    let rest = path.strip_prefix('/')?;
    let mut out = Vec::new();
    for comp in rest.split('/') {
        match comp {
            "" | "." => {}
            ".." => return None,
            c => out.push(c.to_string()),
        }
    }
    Some(out)
}

/// `word` against `cwd`, lexically. The caller has already refused `..`.
fn resolve(cwd: &[String], word: &str) -> Vec<String> {
    let mut out = if word.starts_with('/') {
        Vec::new()
    } else {
        cwd.to_vec()
    };
    out.extend(
        word.split('/')
            .filter(|c| !c.is_empty() && *c != ".")
            .map(str::to_string),
    );
    out
}

fn join(comps: &[String]) -> String {
    format!("/{}", comps.join("/"))
}

/// `a` equals `b` or is an ancestor of it, ASCII-case-insensitively.
fn is_prefix_of(a: &[String], b: &[String]) -> bool {
    a.len() <= b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

/// `/Users`, `/home`, or a direct child of either.
fn is_users_dir(comps: &[String]) -> bool {
    comps.len() <= 2
        && comps
            .first()
            .is_some_and(|c| c.eq_ignore_ascii_case("Users") || c.eq_ignore_ascii_case("home"))
}

/// `Some(name of the pattern)` when `pattern` refuses this target.
pub(crate) fn denied(
    pattern: &DenyPattern,
    comps: &[String],
    path: &str,
    word: &str,
    prefix: Option<&[String]>,
    home: Option<&[String]>,
) -> Option<String> {
    let hit = match pattern {
        DenyPattern::FilesystemRoot => comps.is_empty(),
        DenyPattern::Home => home.is_some_and(|h| {
            is_prefix_of(comps, h)
                || (comps.len() == h.len() + 1
                    && is_prefix_of(h, comps)
                    && comps.last().is_some_and(|c| has_glob(c)))
        }),
        DenyPattern::UsersDir => is_users_dir(comps),
        // A flag, judged where flags are read.
        DenyPattern::NoPreserveRoot => false,
        DenyPattern::GlobAtRoot => comps.first().is_some_and(|c| has_glob(c)),
        DenyPattern::BareGlobAtPrefix => prefix.is_some_and(|p| {
            comps.len() == p.len() + 1
                && comps[..p.len()] == *p
                && comps
                    .last()
                    .is_some_and(|c| c.chars().all(|ch| ch == '*' || ch == '?'))
        }),
        DenyPattern::Component(name) => comps.iter().any(|c| c.eq_ignore_ascii_case(name)),
        DenyPattern::Glob(glob) => glob_match(glob, path) || glob_match(glob, word),
    };
    hit.then(|| match pattern {
        DenyPattern::FilesystemRoot => "filesystem root".to_string(),
        DenyPattern::Home => "home".to_string(),
        DenyPattern::UsersDir => "/Users".to_string(),
        DenyPattern::NoPreserveRoot => "--no-preserve-root".to_string(),
        DenyPattern::GlobAtRoot => "glob at root".to_string(),
        DenyPattern::BareGlobAtPrefix => "bare glob empties the prefix".to_string(),
        DenyPattern::Component(name) => format!("component {name}"),
        DenyPattern::Glob(glob) => format!("glob {glob}"),
    })
}

/// `[segment][token]` of the quote-stripped reading → `(segment, word)` of the
/// quote-resolved one; `None` for a token holding a redirect's `&`.
type WordMap = Vec<Vec<Option<(usize, usize)>>>;

/// Where `raw_words` put each quote-stripped token: `map[segment][token]` is
/// `Some((raw segment, raw word))`, or `None` for a token holding a redirect's
/// `&` (never an operand). `None` overall when the two readings do not have the
/// same shape, or disagree about text that is outside every quote.
///
/// The one modelled divergence: `raw_words` ends a segment at EVERY `&`, so
/// `rm x 2>&1 y` is `[rm x 2>] [1 y]` there and one segment to the splitter.
fn align(segments: &[Vec<String>], raw: &[Vec<String>]) -> Option<WordMap> {
    let mut expected: Vec<Vec<&str>> = Vec::new();
    let mut map = Vec::with_capacity(segments.len());
    for seg in segments {
        let mut cur: Vec<&str> = Vec::new();
        let mut row = Vec::with_capacity(seg.len());
        for tok in seg {
            if !tok.contains('&') {
                row.push(Some((expected.len(), cur.len())));
                cur.push(tok);
                continue;
            }
            row.push(None);
            for (n, piece) in tok.split('&').enumerate() {
                if n > 0 && !cur.is_empty() {
                    expected.push(std::mem::take(&mut cur));
                }
                if !piece.is_empty() {
                    cur.push(piece);
                }
            }
        }
        if !cur.is_empty() {
            expected.push(cur);
        }
        map.push(row);
    }
    if expected.len() != raw.len() {
        return None;
    }
    for (pieces, words) in expected.iter().zip(raw) {
        if pieces.len() != words.len() {
            return None;
        }
        if !pieces.iter().zip(words).all(|(p, w)| piece_matches(p, w)) {
            return None;
        }
    }
    Some(map)
}

/// Whether a quote-resolved `word` can be what the quote-stripped `piece`
/// stood for: equal when the piece had no quotes, else the piece's literal
/// parts (around each `""` placeholder) appear in order, anchored at both ends.
/// A piece with a backslash is not compared — the two readers spell an escape
/// differently, and an `rm` segment with one abstains anyway.
fn piece_matches(piece: &str, word: &str) -> bool {
    if piece.contains('\\') {
        return true;
    }
    let parts: Vec<&str> = piece.split("\"\"").collect();
    let (Some(first), Some(last)) = (parts.first(), parts.last()) else {
        return false;
    };
    if parts.len() == 1 {
        return piece == word;
    }
    let Some(mut rest) = word.strip_prefix(first) else {
        return false;
    };
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(at) => rest = &rest[at + mid.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

/// At most 96 bytes of `s`, cut at a character boundary, with `…` when cut:
/// a reason quotes the line it judged, and the line is not trusted to be short.
fn clip(s: &str) -> String {
    const MAX: usize = 96;
    if s.len() <= MAX {
        return s.to_string();
    }
    format!("{}…", super::truncate_bytes(s, MAX))
}

/// What the host knows about the hook call and [`evaluate`] does not: the
/// other columns of the design's §5.1 ledger row. Every field is INJECTED —
/// `ts` included, since this module reads no clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerFields<'a> {
    /// The row id: the caller's own sequence for the ledger it appends to
    /// (the hook-era ring that assigned it was deleted on 2026-09-23), so the
    /// row and its frame carry one number.
    pub id: u64,
    /// The timestamp, already formatted (RFC 3339 in the design's example).
    pub ts: &'a str,
    /// The aterm session id.
    pub sid: &'a str,
    /// The vendor's session id (`session_id` in the hook JSON).
    pub session_id: &'a str,
    /// The working directory the verdict was computed against.
    pub cwd: &'a str,
    /// The event that asked.
    pub event: HookEvent,
    /// `tool_input.command`, as received.
    pub command: &'a str,
    /// The vendor's permission mode (`permission_mode` in the hook JSON;
    /// `default` in the design's example) — not [`RmMode`].
    pub mode: &'a str,
    /// The session generation the row is bound to (`gen`).
    pub generation: u64,
}

/// One ledger row for `verdict`: a single line of JSON, no trailing newline, in
/// the key order of design §5.1 — `id ts sid session_id cwd event command
/// decision reason mode gen` — followed by `targets`, which the design's row
/// does not have, and by `"truncated":true` when any text was cut.
///
/// Every string goes through `aterm_json`'s writer, so a command holding quotes,
/// newlines or control bytes is still one line of valid JSON and never a second
/// row. The command and cwd are attacker-influenced text: they are DATA in this
/// row, never instructions to whoever reads it (design §4.4).
///
/// The row is at most [`MAX_ROW_BYTES`] bytes. Text budgets start at 1024 bytes
/// for the command, 512 for the cwd, 256 for the reason and each of at most 8
/// targets, 64 for the rest, and halve together until the row fits; at zero the
/// row is its keys and numbers alone, which is far under the ceiling.
pub fn to_ledger_json(verdict: &RmVerdict, fields: &LedgerFields<'_>) -> String {
    let mut shift = 0u32;
    loop {
        let row = ledger_row(verdict, fields, shift);
        if row.len() <= MAX_ROW_BYTES || shift >= FLOOR_SHIFT {
            return row;
        }
        shift += 1;
    }
}

/// The halving at which every text budget has reached zero (`1024 >> 11`).
const FLOOR_SHIFT: u32 = 11;

fn ledger_row(verdict: &RmVerdict, fields: &LedgerFields<'_>, shift: u32) -> String {
    const MAX_ROW_TARGETS: usize = 8;
    let mut truncated = false;
    let mut text = |s: &str, budget: usize| -> String {
        let budget = budget.checked_shr(shift).unwrap_or(0);
        let kept = super::truncate_bytes(s, budget);
        truncated |= kept.len() < s.len();
        // A `&str` cannot fail to serialize; the fallback keeps this total.
        aterm_json::to_string(kept).unwrap_or_else(|_| "\"\"".to_string())
    };
    let mut row = String::with_capacity(512);
    row.push_str(&format!("{{\"id\":{}", fields.id));
    row.push_str(&format!(",\"ts\":{}", text(fields.ts, 64)));
    row.push_str(&format!(",\"sid\":{}", text(fields.sid, 64)));
    row.push_str(&format!(",\"session_id\":{}", text(fields.session_id, 64)));
    row.push_str(&format!(",\"cwd\":{}", text(fields.cwd, 512)));
    row.push_str(&format!(",\"event\":\"{}\"", fields.event.as_str()));
    row.push_str(&format!(",\"command\":{}", text(fields.command, 1024)));
    row.push_str(&format!(",\"decision\":\"{}\"", verdict.decision.as_str()));
    row.push_str(&format!(",\"reason\":{}", text(&verdict.reason, 256)));
    row.push_str(&format!(",\"mode\":{}", text(fields.mode, 64)));
    row.push_str(&format!(",\"gen\":{}", fields.generation));
    row.push_str(",\"targets\":[");
    let shown = if shift >= FLOOR_SHIFT {
        0
    } else {
        MAX_ROW_TARGETS
    };
    for (i, target) in verdict.targets.iter().take(shown).enumerate() {
        if i > 0 {
            row.push(',');
        }
        row.push_str(&text(target, 256));
    }
    row.push(']');
    truncated |= verdict.targets.len() > shown;
    if truncated {
        row.push_str(",\"truncated\":true");
    }
    row.push('}');
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    const CWD: &str = "/work/proj";

    fn policy() -> RmPolicy {
        RmPolicy {
            home: Some(PathBuf::from("/Users//_owner")),
            ..RmPolicy::default()
        }
    }

    fn eval(cmd: &str) -> RmVerdict {
        evaluate(cmd, Path::new(CWD), HookEvent::PermissionRequest, &policy())
    }

    fn eval_with(cmd: &str, cwd: &str, policy: &RmPolicy) -> RmVerdict {
        evaluate(cmd, Path::new(cwd), HookEvent::PermissionRequest, policy)
    }

    enum Want {
        /// Allowed, with exactly these targets.
        Allow(&'static [&'static str]),
        /// Abstained, the reason starting with this.
        Abstain(&'static str),
    }
    use Want::{Abstain, Allow};

    /// The lines the policy approves: a direct `rm`, understood flags, harmless
    /// redirects, every other segment a read, every target strictly inside the
    /// session's directory.
    const APPROVED: &[(&str, Want)] = &[
        ("rm x", Allow(&["/work/proj/x"])),
        (
            "rm -rf target/debug/incremental",
            Allow(&["/work/proj/target/debug/incremental"]),
        ),
        // Flag orderings: split, clustered, after the operand, long forms.
        ("rm -f -r a b", Allow(&["/work/proj/a", "/work/proj/b"])),
        ("rm a -rf", Allow(&["/work/proj/a"])),
        ("rm -Rfv a", Allow(&["/work/proj/a"])),
        (
            "rm -rf --one-file-system --preserve-root cache",
            Allow(&["/work/proj/cache"]),
        ),
        (
            "rm --recursive --force --verbose a",
            Allow(&["/work/proj/a"]),
        ),
        // `--` ends the flags: what follows is a file, whatever it looks like.
        ("rm -- -rf", Allow(&["/work/proj/-rf"])),
        (
            "rm -f -- --no-preserve-root",
            Allow(&["/work/proj/--no-preserve-root"]),
        ),
        ("rm -", Allow(&["/work/proj/-"])),
        // Quoting: the target is the RESOLVED word; a quoted flag is a flag.
        ("rm \"my file.txt\"", Allow(&["/work/proj/my file.txt"])),
        (
            "rm 'a b' \"c d\" e",
            Allow(&["/work/proj/a b", "/work/proj/c d", "/work/proj/e"]),
        ),
        (
            "rm sub/\"quoted name\"/x",
            Allow(&["/work/proj/sub/quoted name/x"]),
        ),
        ("rm \"-rf\" build", Allow(&["/work/proj/build"])),
        ("rm 'it''s'", Allow(&["/work/proj/its"])),
        (
            "rm 'weird;name' \"a&b\" 'c|d'",
            Allow(&["/work/proj/weird;name", "/work/proj/a&b", "/work/proj/c|d"]),
        ),
        // Trailing slashes, `.` and doubled slashes are dropped.
        ("rm -rf build/", Allow(&["/work/proj/build"])),
        (
            "rm -rf ./build//cache/./x/",
            Allow(&["/work/proj/build/cache/x"]),
        ),
        // An absolute path inside the session's directory.
        ("rm -rf /work/proj/build", Allow(&["/work/proj/build"])),
        ("rm -rf /work/proj//build/", Allow(&["/work/proj/build"])),
        // The two system paths of rm.
        ("/bin/rm -f a.o", Allow(&["/work/proj/a.o"])),
        ("/usr/bin/rm a.o", Allow(&["/work/proj/a.o"])),
        // Globs stay inside the directory they are written in.
        ("rm *.o", Allow(&["/work/proj/*.o"])),
        ("rm -rf build/*", Allow(&["/work/proj/build/*"])),
        (
            "rm -rf \"build dir\"/*.tmp",
            Allow(&["/work/proj/build dir/*.tmp"]),
        ),
        ("rm foo~ *~", Allow(&["/work/proj/foo~", "/work/proj/*~"])),
        // Redirects that write no file.
        ("rm -f a 2>/dev/null", Allow(&["/work/proj/a"])),
        ("rm -f a > /dev/null 2>&1", Allow(&["/work/proj/a"])),
        ("rm -f a &>/dev/null", Allow(&["/work/proj/a"])),
        ("rm -f a 2>&1 | tail -3", Allow(&["/work/proj/a"])),
        // The `&` of `2>&1` splits one reader's segment and not the other's;
        // the operand AFTER it must still be judged.
        ("rm x 2>&1 y", Allow(&["/work/proj/x", "/work/proj/y"])),
        // Separators: every segment an rm or a read.
        ("rm a; rm b", Allow(&["/work/proj/a", "/work/proj/b"])),
        (
            "rm a && rm -r b || echo failed",
            Allow(&["/work/proj/a", "/work/proj/b"]),
        ),
        ("rm a & rm b", Allow(&["/work/proj/a", "/work/proj/b"])),
        ("rm a &", Allow(&["/work/proj/a"])),
        ("rm a\nrm b", Allow(&["/work/proj/a", "/work/proj/b"])),
        (
            "[ -f stale.lock ] && rm stale.lock",
            Allow(&["/work/proj/stale.lock"]),
        ),
        (
            "ls -la; rm -v old.log | head -5",
            Allow(&["/work/proj/old.log"]),
        ),
        ("  rm \t x  ", Allow(&["/work/proj/x"])),
        (
            "echo cleaning && rm -rf build && /bin/ls; /usr/bin/du -sh . | tail -1",
            Allow(&["/work/proj/build"]),
        ),
        // Unicode in a name is a name.
        (
            "rm -rf données/été \"日本語 ファイル\"",
            Allow(&["/work/proj/données/été", "/work/proj/日本語 ファイル"]),
        ),
    ];

    /// The lines it does not, each with the rule that must be the one to say so.
    const DECLINED: &[(&str, Want)] = &[
        // Empty input, and an rm of nothing.
        ("", Abstain("line:empty command")),
        ("   \n ", Abstain("line:empty command")),
        ("echo hi", Abstain("line:no rm invocation")),
        ("rm", Abstain("rm:no target")),
        ("rm -rf", Abstain("rm:no target")),
        ("rm -rf 2>/dev/null", Abstain("rm:no target")),
        // The filesystem root, however it is spelled.
        ("rm -rf /", Abstain("deny:filesystem root")),
        ("rm -rf //", Abstain("deny:filesystem root")),
        ("rm -rf /.", Abstain("deny:filesystem root")),
        ("rm -rf x /", Abstain("deny:filesystem root")),
        (
            "rm -rf x /Users//_owner/../..",
            Abstain("unresolvable:a .. component"),
        ),
        ("rm -rf /*", Abstain("deny:glob at root")),
        ("rm -rf /u*/x", Abstain("deny:glob at root")),
        (
            "rm --no-preserve-root -rf x",
            Abstain("deny:--no-preserve-root"),
        ),
        (
            "rm -rf \"--no-preserve-root\" x",
            Abstain("deny:--no-preserve-root"),
        ),
        // Home, by every spelling.
        ("rm -rf ~", Abstain("unresolvable:~")),
        ("rm -rf ~/x", Abstain("unresolvable:~")),
        ("rm -rf ~owner/x", Abstain("unresolvable:~")),
        ("rm -rf \"$HOME\"", Abstain("unresolvable:$HOME")),
        ("rm -rf $HOME/x", Abstain("unresolvable:$HOME")),
        (
            "rm -rf ${HOME}/x",
            Abstain("line:${…}, $'…' or $\"…\" is not modelled"),
        ),
        ("rm -rf '$HOME'", Abstain("unresolvable:$HOME")),
        ("rm -rf /Users//_owner", Abstain("deny:home")),
        ("rm -rf /users/_OWNER/", Abstain("deny:home")),
        ("rm -rf /Users", Abstain("deny:home")),
        ("rm -rf /Users//_someone", Abstain("deny:/Users")),
        ("rm -rf /home/someone", Abstain("deny:/Users")),
        // Anything a shell would still expand.
        ("rm -rf $TMPDIR/x", Abstain("unresolvable:an unexpanded $")),
        ("rm -rf \"${BUILD_DIR}\"", Abstain("line:${…}")),
        (
            "rm -rf \"$BUILD_DIR\"/x",
            Abstain("unresolvable:an unexpanded $"),
        ),
        ("rm -rf $\"x\"", Abstain("line:${…}")),
        ("rm -rf $'\\x2e\\x2e/x'", Abstain("line:${…}")),
        ("rm -rf $(cat list)", Abstain("line:command substitution")),
        ("rm -rf `pwd`", Abstain("line:command substitution")),
        ("rm \"x$(echo y)\"", Abstain("line:command substitution")),
        ("rm 'a`b'", Abstain("line:command substitution")),
        ("rm '$(x)'", Abstain("line:command substitution")),
        // The line that reads as three harmless segments and is one rm.
        (
            "rm -rf x$(echo) cat /etc",
            Abstain("line:command substitution"),
        ),
        ("(rm x)", Abstain("line:command substitution")),
        ("rm {a,b}", Abstain("unresolvable:brace")),
        ("rm =ls", Abstain("unresolvable:a leading =")),
        ("rm -rf \"\"", Abstain("unresolvable:an empty target")),
        // `..` escapes.
        ("rm -rf ../sibling", Abstain("unresolvable:a .. component")),
        (
            "rm -rf build/../../etc",
            Abstain("unresolvable:a .. component"),
        ),
        (
            "rm -rf /work/proj/../other",
            Abstain("unresolvable:a .. component"),
        ),
        ("rm -rf \"..\"", Abstain("unresolvable:a .. component")),
        (
            "rm -rf .*",
            Abstain("unresolvable:a glob that can match .."),
        ),
        (
            "rm -rf sub/.[!.]*/x",
            Abstain("unresolvable:a glob that can match .."),
        ),
        // Outside the session's directory, including the look-alike sibling.
        (
            "rm -rf /etc/hosts",
            Abstain("prefix:/etc/hosts is outside /work/proj"),
        ),
        (
            "rm -rf /work/project2/x",
            Abstain("prefix:/work/project2/x is outside"),
        ),
        (
            "rm -rf /work/PROJ/x",
            Abstain("prefix:/work/PROJ/x is outside"),
        ),
        ("rm -rf /work", Abstain("prefix:/work is outside")),
        ("rm a /tmp/b", Abstain("prefix:/tmp/b is outside")),
        (
            "rm -rf .",
            Abstain("prefix:the target is the prefix directory itself"),
        ),
        (
            "rm -rf ./",
            Abstain("prefix:the target is the prefix directory itself"),
        ),
        (
            "rm -rf /work/proj/",
            Abstain("prefix:the target is the prefix directory itself"),
        ),
        // The whole directory, and the repository.
        ("rm -rf *", Abstain("deny:bare glob")),
        ("rm -rf ./*", Abstain("deny:bare glob")),
        ("rm -rf .git", Abstain("deny:component .git")),
        ("rm -f .git/index.lock", Abstain("deny:component .git")),
        ("rm -rf sub/.GIT/x", Abstain("deny:component .git")),
        // Flags this slice does not know.
        (
            "rm -rf --frobnicate x",
            Abstain("rm:unrecognised flag --frobnicate"),
        ),
        ("rm -W x", Abstain("rm:unrecognised flag -W")),
        ("rm -rfz x", Abstain("rm:unrecognised flag -rfz")),
        // Env assignments before rm, and anything that moves a later rm.
        ("FOO=1 rm x", Abstain("rm:an assignment before rm")),
        (
            "PATH=/evil:$PATH rm x",
            Abstain("rm:an assignment before rm"),
        ),
        (
            "PATH=/evil; rm x",
            // classify refuses a PATH= assignment itself now, before this
            // module's own assignment rule is reached.
            Abstain("segment:not rm and not read-only (PATH="),
        ),
        (
            "export PATH=/evil; rm x",
            Abstain("segment:not rm and not read-only (PATH="),
        ),
        ("export FOO=/evil; rm x", Abstain("segment:export changes")),
        ("cd /etc && rm -rf x", Abstain("segment:cd changes")),
        ("cd build; rm -rf x", Abstain("segment:cd changes")),
        // Wrappers: every one abstains in this slice, and the reason names it.
        ("sudo rm -rf x", Abstain("wrapper:rm reached through sudo")),
        ("ls | xargs rm", Abstain("wrapper:rm reached through xargs")),
        (
            "find . -name '*.o' -exec rm {} \\;",
            Abstain("wrapper:rm reached through find"),
        ),
        (
            "find . -name '*.o' -delete",
            Abstain("segment:not rm and not read-only (find -delete)"),
        ),
        (
            "bash -c 'rm -rf x'",
            Abstain("segment:not rm and not read-only (bash -c)"),
        ),
        (
            "sh -c \"rm -rf x\"",
            Abstain("segment:not rm and not read-only (sh -c)"),
        ),
        ("env rm x", Abstain("wrapper:rm reached through env")),
        ("env FOO=1 rm x", Abstain("wrapper:rm reached through env")),
        ("eval rm x", Abstain("wrapper:rm reached through eval")),
        (
            "eval \"rm x\"",
            Abstain("segment:not rm and not read-only (eval)"),
        ),
        (
            "timeout 5 rm x",
            Abstain("wrapper:rm reached through timeout"),
        ),
        (
            "command rm x",
            Abstain("wrapper:rm reached through command"),
        ),
        ("git rm x", Abstain("wrapper:rm reached through git")),
        (
            "perl -e 'alarm 5; exec @ARGV' rm -rf x",
            Abstain("wrapper:rm reached through perl"),
        ),
        (
            "for f in a b; do rm $f; done",
            Abstain("segment:compound command (for)"),
        ),
        (
            "if [ -f x ]; then rm x; fi",
            Abstain("segment:compound command (if)"),
        ),
        (
            "./rm x",
            Abstain("rm:reached by an unrecognised path (./rm)"),
        ),
        (
            "/tmp/evil/rm x",
            Abstain("rm:reached by an unrecognised path"),
        ),
        ("\\rm x", Abstain("segment:not rm and not read-only")),
        ("\"rm\" x", Abstain("segment:not rm and not read-only")),
        // Redirects that write, read, or hide an operand.
        ("rm x > out.txt", Abstain("rm:redirect > out.txt")),
        ("rm x >> log", Abstain("rm:redirect > log")),
        ("rm x >&out.txt", Abstain("rm:redirect > &out.txt")),
        ("rm x 2>\"/dev/null\"", Abstain("rm:redirect >")),
        ("rm x >", Abstain("rm:redirect > (no target)")),
        // The operand in front of a glued redirect is an operand.
        ("rm /etc/x>/dev/null", Abstain("prefix:/etc/x is outside")),
        ("rm x < list", Abstain("rm:an input redirect")),
        ("rm x <<EOF\ny\nEOF", Abstain("line:a here-document")),
        // A line mixing rm with a write.
        (
            "rm x; make",
            Abstain("segment:not rm and not read-only (make)"),
        ),
        (
            "rm x && touch y",
            Abstain("segment:not rm and not read-only (touch)"),
        ),
        (
            "rm x; echo done > log",
            Abstain("segment:not rm and not read-only (redirect > log)"),
        ),
        (
            "rm x | tee log",
            Abstain("segment:not rm and not read-only (tee)"),
        ),
        (
            "rm a\nmake",
            Abstain("segment:not rm and not read-only (make)"),
        ),
        (
            "rm a & ./deploy.sh",
            Abstain("segment:not rm and not read-only (./deploy.sh (a program named by path))"),
        ),
        (
            "yes | rm -i x",
            Abstain("segment:not rm and not read-only (yes)"),
        ),
        // Read-only to classify, and still not a companion of an rm: each of
        // these can run a program of the line's choosing.
        (
            "git status; rm x",
            Abstain("segment:git is not a companion"),
        ),
        (
            "rm x; git -c core.fsmonitor='touch /tmp/p' status",
            Abstain("segment:not rm and not read-only (git -c"),
        ),
        (
            "rg --pre ./x.sh foo; rm x",
            Abstain("segment:not rm and not read-only (rg --pre"),
        ),
        (
            "sort --compress-program=./x.sh f; rm x",
            Abstain("segment:not rm and not read-only (sort --compress-program"),
        ),
        (
            "find . -name '*.o' | head; rm x",
            Abstain("segment:find is not"),
        ),
        ("ls | xargs cat; rm x", Abstain("segment:xargs is not")),
        ("env FOO=1 ls; rm x", Abstain("segment:env is not")),
        ("timeout 5 ls; rm x", Abstain("segment:timeout is not")),
        ("sed -n 1p f; rm x", Abstain("segment:sed is not")),
        (
            "python3 scripts/sat_score_table.py; rm x",
            Abstain(
                "segment:not rm and not read-only (python3 scripts/sat_score_table.py is not on",
            ),
        ),
        (
            "/tmp/evil/ls; rm x",
            Abstain("segment:not rm and not read-only (/tmp/evil/ls"),
        ),
        (
            "./ls; rm x",
            Abstain("segment:not rm and not read-only (./ls"),
        ),
        // classify accepts `>&word`; bash writes `word`.
        (
            "ls >&out.txt; rm x",
            Abstain("segment:not rm and not read-only (redirect > &out.txt"),
        ),
        // Escapes are not modelled.
        ("rm my\\ file", Abstain("rm:a backslash escape")),
        ("rm -rf \\/", Abstain("rm:a backslash escape")),
        // Three lines that run `rm -rf /` while this reader, left alone, would
        // see only quoted text: a comment, a here-document body and `$'…'`
        // each open an apostrophe the shell never opened.
        (
            "rm x # it's\nrm -rf / # '",
            Abstain("line:a # can start a comment"),
        ),
        (
            "rm x; cat <<EOF\necho '\nEOF\nrm -rf /",
            Abstain("line:a here-document"),
        ),
        (
            "rm x; echo $'a\\'b'\nrm -rf /\necho '",
            Abstain("line:${…}, $'…'"),
        ),
        ("rm \"file#1\"", Abstain("line:a # can start a comment")),
        ("rm x <<< y", Abstain("line:a here-document")),
        // An rm with no `rm` token: the redirect is glued to the head.
        ("rm a; rm>/dev/null -rf /", Abstain("deny:filesystem root")),
        (
            "rm a; /bin/rm&>/dev/null -rf /",
            Abstain("segment:starts with a redirect or a flag"),
        ),
        (
            "rm a; -rf /",
            Abstain("segment:starts with a redirect or a flag"),
        ),
        (
            "rm a; {rm,-rf,/}",
            Abstain("segment:not rm and not read-only"),
        ),
        // Reads that assign: a later `rm` may not be the rm it looks like.
        (
            "for PATH in /evil; do true; done; rm x",
            Abstain("segment:compound command (for)"),
        ),
        (
            "printf -v PATH /evil; rm x",
            Abstain("segment:not rm and not read-only (printf -v"),
        ),
        // GNU getopt takes any unambiguous prefix of a long flag.
        ("rm --no-p -rf x", Abstain("rm:unrecognised flag --no-p")),
        ("rm --no-preserve -rf x", Abstain("rm:unrecognised flag")),
        // Whitespace Rust splits on and the shell does not; control bytes.
        ("rm\u{a0}x", Abstain("line:control or non-ASCII whitespace")),
        (
            "rm x\u{3000}/",
            Abstain("line:control or non-ASCII whitespace"),
        ),
        ("rm x\r", Abstain("line:control or non-ASCII whitespace")),
        (
            "rm x\u{1b}[2K",
            Abstain("line:control or non-ASCII whitespace"),
        ),
        ("rm x\0y", Abstain("line:control or non-ASCII whitespace")),
    ];

    fn check(corpus: &[(&str, Want)]) {
        for (cmd, want) in corpus {
            let v = eval(cmd);
            match want {
                Allow(targets) => {
                    assert_eq!(v.decision, RmDecision::Allow, "{cmd:?}: {v:?}");
                    assert_eq!(v.reason, "policy:rm.approve=prompt-only", "{cmd:?}");
                    assert_eq!(v.targets, *targets, "{cmd:?}");
                }
                Abstain(reason) => {
                    assert_eq!(v.decision, RmDecision::Abstain, "{cmd:?}: {v:?}");
                    assert!(
                        v.reason.starts_with(reason),
                        "{cmd:?}: reason {:?} does not start with {reason:?}",
                        v.reason
                    );
                }
            }
        }
    }

    #[test]
    fn the_corpus_is_as_large_as_the_slice_owes() {
        assert!(APPROVED.len() + DECLINED.len() >= 45);
        assert!(APPROVED.len() >= 30 && DECLINED.len() >= 90);
        assert!(APPROVED.iter().all(|(_, w)| matches!(w, Allow(_))));
        assert!(DECLINED.iter().all(|(_, w)| matches!(w, Abstain(_))));
    }

    #[test]
    fn approved_lines_are_approved_with_their_resolved_targets() {
        check(APPROVED);
    }

    #[test]
    fn declined_lines_abstain_and_name_the_rule() {
        check(DECLINED);
    }

    /// The property the corpus exists for, stated over it: no approval ever
    /// carries a target that is not strictly inside the session's directory.
    #[test]
    fn every_approved_target_is_strictly_inside_the_cwd() {
        for (cmd, _) in APPROVED.iter().chain(DECLINED) {
            let v = eval(cmd);
            if v.decision == RmDecision::Allow {
                assert!(!v.targets.is_empty(), "{cmd:?} approved with no target");
                for t in &v.targets {
                    assert!(t.starts_with("/work/proj/") && t.len() > 11, "{cmd:?}: {t}");
                    assert!(!t.split('/').any(|c| c == ".."), "{cmd:?}: {t}");
                }
            }
        }
    }

    /// The negative control on the whole approved corpus: each approved line
    /// plus ONE bad thing is not approved. A rule that only looked at the first
    /// segment, the first operand or the head would pass the corpus and fail
    /// here.
    #[test]
    fn one_bad_addition_turns_every_approved_line_into_an_abstain() {
        for (cmd, _) in APPROVED {
            let mutants = [
                format!("sudo {cmd}"),
                format!("{cmd}; make"),
                format!("make; {cmd}"),
                format!("cd /etc && {cmd}"),
                format!("{cmd}\nrm -rf /"),
                format!("{cmd} && rm -rf ~"),
                format!("{cmd}; rm ../x"),
                format!("{cmd}; rm $X"),
                format!("{cmd} | tee log"),
                format!("{cmd}; echo $(id)"),
            ];
            for m in &mutants {
                let v = eval(m);
                assert_eq!(v.decision, RmDecision::Abstain, "{m:?}: {v:?}");
            }
        }
        // An extra operand outside the cwd, on the lines that end in an operand.
        for cmd in ["rm x", "rm -rf build/", "rm a; rm b", "rm 'a b' \"c d\" e"] {
            for extra in ["/etc/passwd", "/", "~", "..", "$HOME", "/work/proj"] {
                let m = format!("{cmd} {extra}");
                assert_eq!(eval(&m).decision, RmDecision::Abstain, "{m:?}");
            }
        }
    }

    #[test]
    fn mode_off_always_abstains_and_prompt_only_declines_pre_tool_use() {
        let cwd = Path::new(CWD);
        let mut p = policy();
        for event in [HookEvent::PermissionRequest, HookEvent::PreToolUse] {
            p.mode = RmMode::Off;
            let v = evaluate("rm x", cwd, event, &p);
            assert_eq!(v.decision, RmDecision::Abstain);
            assert_eq!(v.reason, "mode:rm.approve=off");
            assert!(v.targets.is_empty());
        }
        p.mode = RmMode::PromptOnly;
        let v = evaluate("rm x", cwd, HookEvent::PreToolUse, &p);
        assert_eq!(v.decision, RmDecision::Abstain);
        assert!(v.reason.starts_with("mode:rm.approve=prompt-only"), "{v:?}");
        let v = evaluate("rm x", cwd, HookEvent::PermissionRequest, &p);
        assert_eq!(v.decision, RmDecision::Allow);
        assert_eq!(v.reason, "policy:rm.approve=prompt-only");

        p.mode = RmMode::All;
        for event in [HookEvent::PermissionRequest, HookEvent::PreToolUse] {
            let v = evaluate("rm x", cwd, event, &p);
            assert_eq!(v.decision, RmDecision::Allow, "{event:?}");
            assert_eq!(v.reason, "policy:rm.approve=all");
            // `all` widens WHICH EVENT is answered, never which line.
            let v = evaluate("rm -rf /", cwd, event, &p);
            assert_eq!(v.decision, RmDecision::Abstain, "{event:?}");
        }
        assert_eq!(RmPolicy::default().mode, RmMode::PromptOnly);
        assert_eq!(RmMode::default(), RmMode::PromptOnly);
    }

    #[test]
    fn the_config_spellings_round_trip_and_reject_anything_else() {
        for m in [RmMode::PromptOnly, RmMode::All, RmMode::Off] {
            assert_eq!(RmMode::parse(m.as_str()), Some(m));
        }
        for e in [HookEvent::PermissionRequest, HookEvent::PreToolUse] {
            assert_eq!(HookEvent::parse(e.as_str()), Some(e));
        }
        for bad in ["", "ALL", "prompt_only", "on", "true", " all"] {
            assert_eq!(RmMode::parse(bad), None, "{bad:?}");
        }
        for bad in ["", "PostToolUse", "pretooluse", "Stop"] {
            assert_eq!(HookEvent::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_prefix_may_be_another_directory_or_off_and_never_too_wide() {
        // A repo root above a session that runs in a subdirectory.
        let mut p = policy();
        p.require_cwd_prefix = CwdPrefix::Dir(PathBuf::from("/work/proj/"));
        let v = eval_with("rm x /work/proj/other/y", "/work/proj/sub", &p);
        assert_eq!(v.decision, RmDecision::Allow, "{v:?}");
        assert_eq!(v.targets, ["/work/proj/sub/x", "/work/proj/other/y"]);
        let v = eval_with("rm /work/elsewhere/y", "/work/proj/sub", &p);
        assert!(
            v.reason
                .starts_with("prefix:/work/elsewhere/y is outside /work/proj")
        );
        // A session OUTSIDE the named prefix gets nothing relative approved.
        let v = eval_with("rm x", "/var/tmp", &p);
        assert!(
            v.reason.starts_with("prefix:/var/tmp/x is outside"),
            "{v:?}"
        );

        // Off: only rule 7 and the deny patterns stand.
        p.require_cwd_prefix = CwdPrefix::Off;
        let v = eval_with("rm -rf /var/tmp/scratch", CWD, &p);
        assert_eq!(v.decision, RmDecision::Allow, "{v:?}");
        for (cmd, reason) in [
            ("rm -rf /", "deny:filesystem root"),
            ("rm -rf /*", "deny:glob at root"),
            ("rm -rf /Users//_owner", "deny:home"),
            ("rm -rf /Users//_owner/*", "deny:home"),
            ("rm -rf /USERS/x", "deny:/Users"),
            ("rm -rf ~/x", "unresolvable:~"),
            ("rm -rf ../x", "unresolvable:a .. component"),
        ] {
            let v = eval_with(cmd, CWD, &p);
            assert_eq!(v.decision, RmDecision::Abstain, "{cmd}");
            assert!(v.reason.starts_with(reason), "{cmd}: {v:?}");
        }
        // With no prefix there is no "prefix directory" for a bare glob to empty.
        assert_eq!(eval_with("rm -rf *", CWD, &p).decision, RmDecision::Allow);

        // Too wide, by shape and by the injected home.
        let mut p = policy();
        for cwd in [
            "/",
            "/Users",
            "/Users//_owner",
            "/users/other",
            "/home/me",
            "/home",
        ] {
            let v = eval_with("rm x", cwd, &p);
            assert_eq!(v.decision, RmDecision::Abstain, "{cwd}");
            assert!(v.reason.starts_with("prefix:too wide"), "{cwd}: {v:?}");
        }
        p.home = Some(PathBuf::from("/data/homes/owner"));
        for cwd in ["/data", "/data/homes", "/data/homes/owner"] {
            let v = eval_with("rm x", cwd, &p);
            assert!(v.reason.starts_with("prefix:too wide"), "{cwd}: {v:?}");
        }
        let v = eval_with("rm x", "/data/homes/owner/src", &p);
        assert_eq!(v.decision, RmDecision::Allow, "{v:?}");
        p.require_cwd_prefix = CwdPrefix::Dir(PathBuf::from("/"));
        assert!(
            eval_with("rm x", CWD, &p)
                .reason
                .starts_with("prefix:too wide")
        );
    }

    #[test]
    fn a_cwd_home_or_prefix_that_is_not_a_plain_absolute_path_abstains() {
        let p = policy();
        for cwd in ["", "work/proj", "./proj", "/work/../etc", "/work/proj/.."] {
            let v = eval_with("rm x", cwd, &p);
            assert_eq!(v.decision, RmDecision::Abstain, "{cwd:?}");
            assert!(v.reason.starts_with("cwd:"), "{cwd:?}: {v:?}");
        }
        let mut q = policy();
        q.require_cwd_prefix = CwdPrefix::Dir(PathBuf::from("proj"));
        assert!(
            eval_with("rm x", CWD, &q)
                .reason
                .starts_with("prefix:not an absolute")
        );
        q.require_cwd_prefix = CwdPrefix::Dir(PathBuf::from("/work/proj/../.."));
        assert!(
            eval_with("rm x", CWD, &q)
                .reason
                .starts_with("prefix:not an absolute")
        );
        let mut q = policy();
        q.home = Some(PathBuf::from("~"));
        assert!(
            eval_with("rm x", CWD, &q)
                .reason
                .starts_with("prefix:home is not")
        );
        // `.` and doubled slashes in the cwd are dropped like anywhere else.
        let v = eval_with("rm x", "/work/./proj//", &p);
        assert_eq!(v.targets, ["/work/proj/x"]);
    }

    #[test]
    fn deny_patterns_are_the_owners_and_rule_seven_is_not() {
        // Removable: each pattern taken off the list stops deciding.
        let mut p = policy();
        p.deny_patterns
            .retain(|d| *d != DenyPattern::Component(".git".to_string()));
        assert_eq!(
            eval_with("rm -rf .git", CWD, &p).decision,
            RmDecision::Allow
        );
        p.deny_patterns
            .retain(|d| *d != DenyPattern::NoPreserveRoot);
        let v = eval_with("rm --no-preserve-root -rf x", CWD, &p);
        assert_eq!(v.decision, RmDecision::Allow, "{v:?}");
        p.deny_patterns
            .retain(|d| *d != DenyPattern::BareGlobAtPrefix);
        assert_eq!(eval_with("rm -rf *", CWD, &p).decision, RmDecision::Allow);

        // Addable: a glob over the resolved path or the word as written.
        let mut p = policy();
        p.deny_patterns
            .push(DenyPattern::Glob("*/secrets/*".to_string()));
        p.deny_patterns.push(DenyPattern::Glob("*.key".to_string()));
        p.deny_patterns
            .push(DenyPattern::Component("node_modules".to_string()));
        let v = eval_with("rm secrets/token", CWD, &p);
        assert_eq!(v.reason, "deny:glob */secrets/*");
        assert_eq!(eval_with("rm id.key", CWD, &p).reason, "deny:glob *.key");
        let v = eval_with("rm -rf web/node_modules/x", CWD, &p);
        assert_eq!(v.reason, "deny:component node_modules");
        assert_eq!(
            eval_with("rm notes.txt", CWD, &p).decision,
            RmDecision::Allow
        );

        // Not removable: with an EMPTY list and no prefix, what cannot be
        // resolved still abstains, and so does every structural rule.
        let bare = RmPolicy {
            mode: RmMode::All,
            deny_patterns: Vec::new(),
            require_cwd_prefix: CwdPrefix::Off,
            home: None,
        };
        for cmd in [
            "rm -rf ~",
            "rm -rf $HOME",
            "rm -rf $X",
            "rm -rf ..",
            "rm -rf a/../..",
            "rm -rf .*",
            "rm -rf {a,b}",
            "rm =ls",
            "rm -rf $(pwd)",
            "sudo rm x",
            "cd / && rm x",
            "rm x; make",
        ] {
            assert_eq!(
                eval_with(cmd, CWD, &bare).decision,
                RmDecision::Abstain,
                "{cmd}"
            );
        }
        // ... and the same empty policy does approve what is left, so the loop
        // above is not vacuous.
        assert_eq!(
            eval_with("rm -rf /", CWD, &bare).decision,
            RmDecision::Allow
        );
        assert_eq!(eval_with("rm x", CWD, &bare).decision, RmDecision::Allow);
    }

    #[test]
    fn the_default_policy_is_the_designs_default() {
        let d = RmPolicy::default();
        assert_eq!(d.mode, RmMode::PromptOnly);
        assert_eq!(d.require_cwd_prefix, CwdPrefix::SessionCwd);
        assert_eq!(d.home, None);
        for want in [
            DenyPattern::FilesystemRoot,
            DenyPattern::Home,
            DenyPattern::UsersDir,
            DenyPattern::NoPreserveRoot,
            DenyPattern::GlobAtRoot,
        ] {
            assert!(d.deny_patterns.contains(&want), "{want:?}");
        }
        // With no home injected the shape rules still catch a home directory.
        let v = evaluate(
            "rm -rf /Users//_owner",
            Path::new(CWD),
            HookEvent::PermissionRequest,
            &d,
        );
        assert_eq!(v.reason, "deny:/Users");
    }

    #[test]
    fn size_bounds_a_one_mebibyte_line_is_not_parsed() {
        let big = format!("rm {}", "a ".repeat(512 * 1024));
        assert!(big.len() > 1024 * 1024);
        let v = eval(&big);
        assert_eq!(v.decision, RmDecision::Abstain);
        assert_eq!(v.reason, format!("line:longer than {MAX_LINE_BYTES} bytes"));
        assert!(v.targets.is_empty());

        // The boundary itself: exactly MAX is judged, one byte more is not.
        let at = format!("rm {}", "a".repeat(MAX_LINE_BYTES - 3));
        assert_eq!(at.len(), MAX_LINE_BYTES);
        assert_eq!(eval(&at).decision, RmDecision::Allow);
        assert!(eval(&format!("{at}a")).reason.starts_with("line:longer"));

        // More operands than MAX_TARGETS, all of them fine one by one.
        let many = format!("rm {}", "f ".repeat(MAX_TARGETS + 1));
        let v = eval(&many);
        assert_eq!(v.reason, format!("rm:more than {MAX_TARGETS} targets"));
        assert_eq!(v.targets.len(), MAX_TARGETS);
        let ok = format!("rm {}", "f ".repeat(MAX_TARGETS));
        assert_eq!(eval(&ok).decision, RmDecision::Allow);

        // The worst line classify's quadratic scans can be handed under the cap
        // still comes back (every `sort` token rescans the rest of its segment).
        let worst = format!("{}; rm x", "sort ".repeat((MAX_LINE_BYTES - 8) / 5));
        assert!(worst.len() <= MAX_LINE_BYTES);
        assert!(
            eval(&worst)
                .reason
                .starts_with("segment:sort is not a companion")
        );
    }

    #[test]
    fn the_two_readings_are_aligned_or_the_verdict_abstains() {
        let line = "rm x 2>&1 y";
        let segs = split_segments(&strip_quotes(line));
        let raw = raw_words(line);
        assert_eq!(raw, vec![vec!["rm", "x", "2>"], vec!["1", "y"]]);
        let map = align(&segs, &raw);
        assert_eq!(
            map,
            Some(vec![vec![Some((0, 0)), Some((0, 1)), None, Some((1, 1))]])
        );

        // Forged successors: a raw reading with a word more, a word less, a
        // segment more, or different text outside the quotes, is refused.
        let good = vec![
            vec!["rm".to_string(), "x".to_string(), "2>".to_string()],
            vec!["1".to_string(), "y".to_string()],
        ];
        assert!(align(&segs, &good).is_some());
        let mut more = good.clone();
        more[0].push("z".to_string());
        assert_eq!(align(&segs, &more), None);
        let mut fewer = good.clone();
        fewer[1].pop();
        assert_eq!(align(&segs, &fewer), None);
        let mut extra_segment = good.clone();
        extra_segment.push(vec!["ls".to_string()]);
        assert_eq!(align(&segs, &extra_segment), None);
        let mut other_text = good.clone();
        other_text[1][1] = "/".to_string();
        assert_eq!(align(&segs, &other_text), None);

        assert!(piece_matches("a", "a"));
        assert!(!piece_matches("a", "b"));
        assert!(piece_matches("\"\"", "anything at all"));
        assert!(piece_matches("\"\"", ""));
        assert!(piece_matches("sub/\"\"/x", "sub/quoted name/x"));
        assert!(!piece_matches("sub/\"\"/x", "sub/quoted name/y"));
        assert!(!piece_matches("sub/\"\"/x", "other/quoted name/x"));
        assert!(piece_matches("a\"\"b\"\"c", "a1b2c"));
        assert!(!piece_matches("a\"\"b\"\"c", "a1c"));
        assert!(!piece_matches("ab\"\"", "a"));

        // An escaped space is one word to one reader and two to the other: the
        // line abstains rather than guess.
        let v = eval("ls my\\ dir; rm x");
        assert_eq!(v.decision, RmDecision::Abstain);
        assert!(v.reason.starts_with("line:the quote-stripped"), "{v:?}");
    }

    #[test]
    fn helpers_say_what_they_say() {
        assert_eq!(
            absolute_components("/a//b/./c/"),
            Some(vec!["a".into(), "b".into(), "c".into()])
        );
        assert_eq!(absolute_components("/"), Some(Vec::new()));
        assert_eq!(absolute_components("a/b"), None);
        assert_eq!(absolute_components("/a/../b"), None);
        assert!(redirect_is_harmless("/dev/null"));
        assert!(redirect_is_harmless("&1"));
        assert!(redirect_is_harmless("&-"));
        assert!(!redirect_is_harmless("&"));
        assert!(!redirect_is_harmless("&out.txt"));
        assert!(!redirect_is_harmless("/dev/null2"));
        assert!(!redirect_is_harmless("out"));
        assert!(is_known_flag("-rf") && is_known_flag("-dfiIPrRvx") && is_known_flag("--force"));
        assert!(!is_known_flag("-W") && !is_known_flag("--help") && !is_known_flag("--force=1"));
        assert_eq!(clip("short"), "short");
        let long = "é".repeat(100);
        let clipped = clip(&long);
        assert!(clipped.ends_with('…') && clipped.len() <= 96 + '…'.len_utf8());
        assert_eq!(crate::harness::truncate_bytes("héllo", 2), "h");
        assert_eq!(crate::harness::truncate_bytes("héllo", 3), "hé");
        assert_eq!(crate::harness::truncate_bytes("abc", 0), "");
    }

    fn fields<'a>(command: &'a str) -> LedgerFields<'a> {
        LedgerFields {
            id: 812,
            ts: "2026-09-19T10:11:12Z",
            sid: "s-41",
            session_id: "0b5c6f0e-vendor",
            cwd: "/work/proj",
            event: HookEvent::PermissionRequest,
            command,
            mode: "default",
            generation: 25,
        }
    }

    #[test]
    fn the_ledger_row_is_the_designs_row_plus_targets() {
        let cmd = "rm -rf target.noindex/debug/incremental";
        let v = eval(cmd);
        let row = to_ledger_json(&v, &fields(cmd));
        assert_eq!(
            row,
            "{\"id\":812,\"ts\":\"2026-09-19T10:11:12Z\",\"sid\":\"s-41\",\
             \"session_id\":\"0b5c6f0e-vendor\",\"cwd\":\"/work/proj\",\
             \"event\":\"PermissionRequest\",\
             \"command\":\"rm -rf target.noindex/debug/incremental\",\
             \"decision\":\"allow\",\"reason\":\"policy:rm.approve=prompt-only\",\
             \"mode\":\"default\",\"gen\":25,\
             \"targets\":[\"/work/proj/target.noindex/debug/incremental\"]}"
        );
        let parsed: aterm_json::Value = aterm_json::from_str(&row).expect("one JSON value");
        assert_eq!(
            parsed.get("decision").and_then(|d| d.as_str()),
            Some("allow")
        );
        assert!(parsed.get("truncated").is_none());

        let v = eval("rm -rf /");
        let row = to_ledger_json(&v, &fields("rm -rf /"));
        let parsed: aterm_json::Value = aterm_json::from_str(&row).expect("one JSON value");
        assert_eq!(
            parsed.get("decision").and_then(|d| d.as_str()),
            Some("abstain")
        );
        assert_eq!(
            parsed.get("reason").and_then(|d| d.as_str()),
            Some("deny:filesystem root")
        );
        assert_eq!(
            parsed
                .get("targets")
                .and_then(|t| t.as_array())
                .map(Vec::len),
            Some(1)
        );
    }

    /// A command is attacker-influenced text. It must stay ONE row: no raw
    /// newline, no way to close the string and open a forged second object.
    #[test]
    fn a_hostile_command_is_escaped_into_one_line_and_round_trips() {
        let cmd = "rm 'x'\n{\"id\":813,\"decision\":\"allow\"}\t\"\\\u{1}\u{7f} été";
        let v = eval(cmd);
        assert_eq!(v.decision, RmDecision::Abstain);
        let row = to_ledger_json(&v, &fields(cmd));
        assert!(!row.contains('\n') && !row.contains('\0') && !row.contains('\u{1}'));
        let parsed: aterm_json::Value = aterm_json::from_str(&row).expect("one JSON value");
        assert_eq!(parsed.get("command").and_then(|c| c.as_str()), Some(cmd));
        assert_eq!(
            parsed.get("id").and_then(aterm_json::Value::as_u64),
            Some(812)
        );
        assert_eq!(
            parsed.get("gen").and_then(aterm_json::Value::as_u64),
            Some(25)
        );
        assert_eq!(
            parsed.get("event").and_then(|c| c.as_str()),
            Some("PermissionRequest")
        );
    }

    #[test]
    fn a_row_never_exceeds_the_quota_whatever_it_is_fed() {
        // A 1 MiB command: cut, flagged, still one valid value.
        let big = format!("rm {}", "a ".repeat(512 * 1024));
        let v = eval(&big);
        let row = to_ledger_json(&v, &fields(&big));
        assert!(row.len() <= MAX_ROW_BYTES, "{}", row.len());
        let parsed: aterm_json::Value = aterm_json::from_str(&row).expect("one JSON value");
        assert_eq!(
            parsed.get("truncated").and_then(aterm_json::Value::as_bool),
            Some(true)
        );
        let kept = parsed.get("command").and_then(|c| c.as_str()).unwrap_or("");
        assert!(big.starts_with(kept) && kept.len() == 1024);

        // Every text field hostile at once: control bytes cost six output bytes
        // each, so the first budgets overflow and the halving has to run.
        let noise = "\u{1}".repeat(100_000);
        let verdict = RmVerdict {
            decision: RmDecision::Abstain,
            reason: noise.clone(),
            targets: vec![noise.clone(); 40],
        };
        let f = LedgerFields {
            id: u64::MAX,
            ts: &noise,
            sid: &noise,
            session_id: &noise,
            cwd: &noise,
            event: HookEvent::PreToolUse,
            command: &noise,
            mode: &noise,
            generation: u64::MAX,
        };
        let row = to_ledger_json(&verdict, &f);
        assert!(row.len() <= MAX_ROW_BYTES, "{}", row.len());
        assert!(!row.contains('\u{1}'));
        let parsed: aterm_json::Value = aterm_json::from_str(&row).expect("one JSON value");
        assert_eq!(
            parsed.get("truncated").and_then(aterm_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            parsed.get("id").and_then(aterm_json::Value::as_u64),
            Some(u64::MAX)
        );

        // A cut never lands inside a character.
        let wide = "é".repeat(5000);
        let row = to_ledger_json(&eval("rm x"), &fields(&wide));
        let parsed: aterm_json::Value = aterm_json::from_str(&row).expect("one JSON value");
        let kept = parsed.get("command").and_then(|c| c.as_str()).unwrap_or("");
        assert!(!kept.is_empty() && wide.starts_with(kept));

        // More targets than a row shows is a truncation, and says so.
        let many = format!("rm {}", "f ".repeat(20));
        let row = to_ledger_json(&eval(&many), &fields(&many));
        let parsed: aterm_json::Value = aterm_json::from_str(&row).expect("one JSON value");
        assert_eq!(
            parsed
                .get("targets")
                .and_then(|t| t.as_array())
                .map(Vec::len),
            Some(8)
        );
        assert_eq!(
            parsed.get("truncated").and_then(aterm_json::Value::as_bool),
            Some(true)
        );
    }
}
