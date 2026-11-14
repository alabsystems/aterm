// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What `fleet-sim` holds a fleet read to, judged on its text alone (pure, so the judges are unit-tested on typed data): the survey's rendered text (`aterm ctl survey --fleet`) and its JSON (`--json`), against what the simulation knows it made — which instance answers with which pid on which port, and which session runs what.

use std::collections::BTreeMap;

use aterm_json::Value;

/// One assertion's outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// What was asserted.
    pub name: String,
    /// Whether it held.
    pub ok: bool,
    /// What was seen.
    pub detail: String,
}

impl Check {
    /// A check named `name` that held when `ok`, `detail` saying what was seen.
    #[must_use]
    pub fn new(name: impl Into<String>, ok: bool, detail: impl Into<String>) -> Check {
        Check {
            name: name.into(),
            ok,
            detail: detail.into(),
        }
    }
}

/// One session the simulation made, as a survey must show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WantSession {
    /// Its sid.
    pub sid: String,
    /// The `program=` it must show.
    pub program: String,
    /// The `agent=` word it must show (`-` for none).
    pub agent: String,
}

/// One instance, as a fleet read must show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WantInstance {
    /// Its index.
    pub i: usize,
    /// The instance's pid (its header's `pid`).
    pub pid: i32,
    /// Its fleet port (a peer's header names `127.0.0.1:<port>`).
    pub port: u16,
    /// The reader's own machine (headed by its host name, never `fleet`).
    pub local: bool,
    /// Its sessions.
    pub sessions: Vec<WantSession>,
}

/// One block of the rendered text: its header and the lines under it.
#[derive(Debug)]
struct Block<'a> {
    header: &'a str,
    lines: Vec<&'a str>,
}

/// The rendered text's blocks (each `== ` line and what follows it to the
/// next) and the lines before the first.
fn blocks(text: &str) -> Vec<Block<'_>> {
    let mut out: Vec<Block<'_>> = Vec::new();
    for line in text.lines() {
        if line.starts_with("== ") {
            out.push(Block {
                header: line,
                lines: Vec::new(),
            });
        } else if let Some(last) = out.last_mut() {
            last.lines.push(line);
        }
    }
    out
}

/// The pid a header names (`… pid <n> …`).
fn header_pid(header: &str) -> Option<i32> {
    let mut words = header.split(' ');
    while let Some(w) = words.next() {
        if w == "pid" {
            return words.next()?.parse().ok();
        }
    }
    None
}

/// The pid of every header in `text` that names one (an answered block).
#[must_use]
pub fn header_pids(text: &str) -> Vec<i32> {
    blocks(text)
        .iter()
        .filter_map(|b| header_pid(b.header))
        .collect()
}

/// The block of `text` whose header names `pid` — its header and every line under it, to the next header — when exactly one does.
#[must_use]
pub fn block_text(text: &str, pid: i32) -> Option<String> {
    let found: Vec<Block<'_>> = blocks(text)
        .into_iter()
        .filter(|b| header_pid(b.header) == Some(pid))
        .collect();
    match found.as_slice() {
        [one] => {
            let mut out = format!("{}\n", one.header);
            for line in &one.lines {
                out.push_str(line);
                out.push('\n');
            }
            Some(out)
        }
        _ => None,
    }
}

/// The headers of `text` that say the machine at `127.0.0.1:<port>` did not answer (`== 127.0.0.1:<port> did not answer: <reason> …`).
#[must_use]
pub fn unanswered_rows(text: &str, port: u16) -> Vec<String> {
    let head = format!("== 127.0.0.1:{port} did not answer: ");
    blocks(text)
        .iter()
        .map(|b| b.header)
        .filter(|h| h.starts_with(&head))
        .map(str::to_owned)
        .collect()
}

/// The characters a terminal would act on (`aterm_codec::tty::is_active`) that `text` holds raw, but for the line breaks between its lines.
#[must_use]
pub fn raw_active(text: &str) -> Vec<char> {
    text.chars()
        .filter(|&c| c != '\n' && aterm_codec::tty::is_active(c))
        .collect()
}

/// One Claude session as a read shows it ([`judge_text`]).
struct Seen {
    /// The client's rank of it ([`ranks`]); `None` when the json gave none.
    rank: Option<u8>,
    /// Its place among its block's Claude sessions in the text's order: the
    /// round of the client's round robin it is in.
    round: Option<usize>,
    /// Whether it is its machine's most pressing Claude session: the client's first of the
    /// block (the least rank, the earliest line among equals), whose leading excerpt line the
    /// client fills before any machine's second line of words.
    first: bool,
    /// Whether its `asked:` and its `said:` show.
    asked: bool,
    said: bool,
    /// How many of its excerpt lines show, its `attention:` and `goal:` lines included: its
    /// leading line, the client's first of the least rank, is `attention:` where it has one.
    lines: usize,
    /// Whether its line carries the client's mark that excerpts were cut.
    marked: bool,
}

impl Seen {
    /// Whether both its excerpt lines show — each perhaps cut to its width,
    /// as the narrowing judges ([`judge_narrowing`]).
    const fn both_shown(&self) -> bool {
        self.asked && self.said
    }
}

/// THE HEALTHY FLEET READ's text: within `budget` characters; one header per instance, each naming its host (the local machine's name, a peer's `fleet 127.0.0.1:<port>`) and none more, none unanswered; each session on a line of its block naming its program (and agent word), with content (` | ` and what follows) from a content-level machine, or — when the budget cut — counted on its block's `+N` line, the shells folded before any agent; the excerpt lines (`asked:`, `said:`) of every Claude session; and `host` the local host name.
#[must_use]
pub fn judge_text(
    text: &str,
    want: &[WantInstance],
    budget: usize,
    host: &str,
    every_top: bool,
    ranks: &BTreeMap<String, u8>,
) -> Vec<Check> {
    let mut checks = Vec::new();
    let chars = text.chars().count();
    checks.push(Check::new(
        "text within the budget",
        chars <= budget,
        format!("{chars} chars, {} bytes, budget {budget}", text.len()),
    ));
    let blocks = blocks(text);
    let unanswered: Vec<&str> = blocks
        .iter()
        .map(|b| b.header)
        .filter(|h| h.contains(" did not answer: "))
        .collect();
    checks.push(Check::new(
        "every machine answered",
        unanswered.is_empty(),
        if unanswered.is_empty() {
            "none unanswered".to_owned()
        } else {
            unanswered.join(" / ")
        },
    ));
    checks.push(Check::new(
        "one header block per instance",
        blocks.len() == want.len(),
        format!("{} blocks for {} instances", blocks.len(), want.len()),
    ));
    let closing = text
        .lines()
        .find(|l| l.starts_with(aterm_ctl::LEFT_OUT_LEAD) || l.contains(" more lines past the "));
    let budget_cut = closing.is_some();
    let mut misnamed = Vec::new();
    let mut uncounted = Vec::new();
    let mut missing = Vec::new();
    let mut twice = Vec::new();
    let mut wrong_rows = Vec::new();
    let mut no_content = Vec::new();
    let (mut unlisted, mut folded, mut agent_folded, mut shell_listed) =
        (0usize, 0usize, false, false);
    let mut unfolded = Vec::new();
    let mut claude: Vec<Seen> = Vec::new();
    let mut firsts: Vec<(usize, Option<usize>)> = Vec::new();
    let (mut wrong_end, mut ends_kept) = (Vec::new(), 0usize);
    for w in want {
        let found: Vec<&Block<'_>> = blocks
            .iter()
            .filter(|b| header_pid(b.header) == Some(w.pid))
            .collect();
        let block = match found.as_slice() {
            [] => {
                missing.push(format!("m{:02}", w.i));
                continue;
            }
            [one] => *one,
            _ => {
                twice.push(format!("m{:02}", w.i));
                continue;
            }
        };
        let names = if w.local {
            block.header.starts_with(&format!("== {host} pid "))
        } else {
            block
                .header
                .starts_with(&format!("== fleet 127.0.0.1:{} ", w.port))
        };
        if !names || !block.header.ends_with(" trust=screen") {
            misnamed.push(block.header.to_owned());
        }
        let facts = block
            .header
            .split_once(&format!(" · {} sessions", w.sessions.len()))
            .map(|(_, rest)| rest);
        let pressing = w
            .sessions
            .iter()
            .any(|s| s.agent == "busy" || s.agent == "prompt");
        let whole =
            facts.is_some_and(|rest| !rest.contains('…') && (!pressing || rest.starts_with(" (")));
        if !whole {
            uncounted.push(block.header.to_owned());
        }
        let counted: usize = block
            .lines
            .iter()
            .filter_map(|l| l.strip_prefix("  +"))
            .filter_map(|l| l.split(' ').next()?.parse::<usize>().ok())
            .sum();
        folded += counted;
        let mut here = 0usize;
        let mut seen: Vec<(usize, Seen)> = Vec::new();
        for s in &w.sessions {
            let at = block
                .lines
                .iter()
                .position(|l| l.split(' ').next() == Some(s.sid.as_str()));
            let Some(at) = at else {
                here += 1;
                if s.program == "claude" {
                    agent_folded = true;
                }
                continue;
            };
            shell_listed |= s.program != "claude";
            let line = block.lines[at];
            let rest = line
                .strip_prefix(s.sid.as_str())
                .unwrap_or(line)
                .trim_start_matches(" (you)");
            let shows = match (s.program.as_str(), s.agent.as_str()) {
                ("zsh", "-") => rest.starts_with(" zsh idle"),
                (program, "-") => rest.starts_with(&format!(" running {program}")),
                (program, agent) => {
                    let word = ranks
                        .get(&s.sid)
                        .map_or(agent, |&rank| aterm_ctl::agent_word_shown(agent, rank));
                    rest.starts_with(&format!(" {program} {word}"))
                }
            };
            if !shows {
                wrong_rows.push(format!("m{:02}: {line}", w.i));
            }
            if !line.contains(" | ") {
                no_content.push(format!("m{:02}: {line}", w.i));
            }
            if s.program == "claude" {
                let next: Vec<&str> = block.lines[at + 1..]
                    .iter()
                    .take_while(|l| l.starts_with("    "))
                    .copied()
                    .collect();
                seen.push((
                    at,
                    Seen {
                        rank: ranks.get(&s.sid).copied(),
                        round: None,
                        first: false,
                        asked: next.iter().any(|l| l.starts_with("    asked: ")),
                        said: next.iter().any(|l| l.starts_with("    said: ")),
                        lines: next.iter().filter(|l| is_excerpt_row(l)).count(),
                        marked: line.contains(aterm_ctl::EXCERPTS_CUT),
                    },
                ));
                if let Some(said) = next.iter().find_map(|l| l.strip_prefix("    said: ")) {
                    let finished = s.agent == "idle";
                    let (kept_end, kept_head) = (said.starts_with('…'), said.ends_with('…'));
                    if (finished && kept_head) || (!finished && kept_end) {
                        wrong_end.push(format!("m{:02} {} {}: {said}", w.i, s.sid, s.agent));
                    }
                    ends_kept += usize::from(finished && kept_end);
                }
            }
        }
        seen.sort_by_key(|(at, _)| *at);
        let lead = seen
            .iter()
            .enumerate()
            .min_by_key(|(_, (_, one))| one.rank.unwrap_or(u8::MAX))
            .map(|(n, _)| n);
        if let Some(n) = lead {
            firsts.push((w.i, Some(claude.len() + n)));
            seen[n].1.first = true;
        }
        for (round, (_, mut one)) in seen.into_iter().enumerate() {
            if !one.rank.is_some_and(aterm_ctl::survey_urgent) {
                one.round = Some(round);
            }
            claude.push(one);
        }
        if here != counted {
            unfolded.push(format!("m{:02}: {here} not listed, {counted} counted", w.i));
        }
        unlisted += here;
    }
    checks.push(Check::new(
        "each instance's block found once, by its pid",
        missing.is_empty() && twice.is_empty(),
        format!("missing {missing:?}, more than once {twice:?}"),
    ));
    checks.push(Check::new(
        "each header names its host",
        misnamed.is_empty(),
        if misnamed.is_empty() {
            format!("local `== {host} pid …`, peers `== fleet 127.0.0.1:<port> …`")
        } else {
            misnamed.join(" / ")
        },
    ));
    checks.push(Check::new(
        "each header carries its session and state counts whole",
        uncounted.is_empty(),
        if uncounted.is_empty() {
            "all".to_owned()
        } else {
            uncounted.join(" / ")
        },
    ));
    checks.push(Check::new(
        "every session listed, or counted on its block's +N line (the budget cut, shells first)",
        unfolded.is_empty() && (unlisted == 0 || budget_cut) && !(agent_folded && shell_listed),
        if unfolded.is_empty() {
            format!(
                "{unlisted} not listed{}",
                if agent_folded && shell_listed {
                    ", an agent's line folded while a shell's is listed"
                } else {
                    ""
                }
            )
        } else {
            unfolded.join(" / ")
        },
    ));
    checks.push(Check::new(
        "every session row shows its program (and agent word)",
        wrong_rows.is_empty(),
        if wrong_rows.is_empty() {
            "all".to_owned()
        } else {
            wrong_rows.join(" / ")
        },
    ));
    checks.push(Check::new(
        "every session row carries content",
        no_content.is_empty(),
        if no_content.is_empty() {
            "all".to_owned()
        } else {
            no_content.join(" / ")
        },
    ));
    let excerpted = claude.iter().filter(|seen| seen.both_shown()).count();
    let unranked = claude.iter().filter(|seen| seen.rank.is_none()).count();
    checks.push(Check::new(
        "every Claude session's excerpts (asked, said)",
        if budget_cut {
            unranked == 0 && filled_in_order(&claude)
        } else {
            excerpted == claude.len()
        },
        if budget_cut {
            format!(
                "{excerpted} of {} with both shown, the budget cut: the client's fill order \
                 (every asking and stalled session's leading line, then each other machine's \
                 most pressing session's, then the asking and stalled \
                 whole, then a round robin over the machines, said before asked) {} ({unranked} with no rank in the json)",
                claude.len(),
                if filled_in_order(&claude) {
                    "held"
                } else {
                    "BROKEN"
                }
            )
        } else {
            format!("{excerpted} of {}", claude.len())
        },
    ));
    if every_top {
        let short: Vec<String> = firsts
            .iter()
            .filter_map(|&(i, at)| {
                let seen = &claude[at?];
                let kept = if seen.round.is_none() {
                    seen.both_shown()
                } else {
                    seen.said
                };
                (!kept).then(|| format!("m{i:02}"))
            })
            .collect();
        checks.push(Check::new(
            "each machine's most pressing Claude session keeps its words",
            short.is_empty(),
            if short.is_empty() {
                format!("all {} machines", firsts.len())
            } else {
                format!("short: {}", short.join(" "))
            },
        ));
    }
    if budget_cut {
        let marks = text
            .lines()
            .filter(|l| l.starts_with("s-") && l.contains(aterm_ctl::EXCERPTS_CUT))
            .count();
        let wrong_marks = claude
            .iter()
            .filter(|seen| seen.marked == seen.both_shown())
            .count();
        let said = closing.map(closing_counts).unwrap_or_default();
        checks.push(Check::new(
            "every cut is marked, and the closing line counts the marks and the +N lines",
            wrong_marks == 0 && said == (marks, folded),
            format!(
                "{marks} lines marked, {folded} sessions on +N lines; the closing line says \
                 {} and {}; {wrong_marks} Claude sessions marked wrong",
                said.0, said.1
            ),
        ));
        let room = budget.saturating_sub(chars);
        checks.push(Check::new(
            "the budget is used, to within one excerpt line",
            room <= aterm_ctl::EXCERPT_CHARS,
            format!("{room} of {budget} chars unused"),
        ));
    }
    checks.push(judge_narrowing(text, budget, budget_cut));
    checks.push(Check::new(
        "a finished turn's cut said: keeps its end, any other its head",
        wrong_end.is_empty(),
        if wrong_end.is_empty() {
            format!("{ends_kept} finished turns' kept their end")
        } else {
            wrong_end.join(" / ")
        },
    ));
    checks
}

/// Whether `claude` (every Claude session of a cut read, in its order) shows
/// its excerpts as the client fills a tight read: every one it keeps first
/// across the read (no round, asking or stalled) and each machine's most
/// pressing one ([`Seen::first`]) its leading line (any excerpt line: the
/// client's first of the least rank) before any shows a line past it, the
/// ones kept first before the other machines' firsts; every one kept first
/// with both excerpt lines before any other shows a line past its leading
/// one; every other in a round with both before any in a later round shows
/// a word; and in a round, every one's `said:` before any one's `asked:`.
fn filled_in_order(claude: &[Seen]) -> bool {
    let shows = |seen: &Seen| seen.asked || seen.said;
    let urgent = |seen: &Seen| seen.round.is_none();
    let leads = |seen: &Seen| seen.first || urgent(seen);
    // A line past the first tier: any of a session that leads nothing, a leader's second.
    let past_lead = |seen: &Seen| seen.lines > usize::from(leads(seen));
    let firsts = || claude.iter().filter(|seen| seen.first);
    let led = claude
        .iter()
        .filter(|seen| leads(seen))
        .all(|seen| seen.lines > 0);
    let urgent_led = claude
        .iter()
        .filter(|seen| urgent(seen))
        .all(|seen| seen.lines > 0);
    let urgent_shown = claude
        .iter()
        .filter(|seen| seen.round.is_none())
        .all(Seen::both_shown);
    let others = || claude.iter().filter(|seen| seen.round.is_some());
    (led || !claude.iter().any(past_lead))
        && (urgent_led || !firsts().any(|seen| !urgent(seen) && seen.lines > 0))
        && firsts().all(|seen| seen.said || !seen.asked)
        && (urgent_shown || !others().any(past_lead))
        && others().all(|a| {
            others().all(|b| {
                let (ra, rb) = (a.round, b.round);
                (!shows(a) || rb >= ra || b.both_shown()) && (!(a.asked && ra == rb) || b.said)
            })
        })
}

/// What a closing line says the budget left out: how many sessions' excerpts
/// (each line marked) and how many sessions (on `+N` lines); `(0, 0)` for
/// the last resort's line, which counts lines.
fn closing_counts(line: &str) -> (usize, usize) {
    let Some(what) = line.strip_prefix(aterm_ctl::LEFT_OUT_LEAD) else {
        return (0, 0);
    };
    let what = what.split(';').next().unwrap_or_default();
    let number = |part: &str| {
        part.split(' ')
            .find_map(|w| w.parse::<usize>().ok())
            .unwrap_or(0)
    };
    let mut counts = (0, 0);
    for part in what.split(" and ") {
        if part.contains(" excerpts") {
            counts.0 = number(part);
        } else if part.contains("(+N lines)") {
            counts.1 = number(part);
        }
    }
    counts
}

/// Whether a text line is an excerpt line (`    asked: …`, `    said: …`,
/// `    attention: …`, `    goal: …`).
fn is_excerpt_row(line: &str) -> bool {
    line.strip_prefix("    ")
        .and_then(|rest| rest.split_once(": "))
        .is_some_and(|(word, _)| matches!(word, "asked" | "said" | "attention" | "goal"))
}

/// Whether an excerpt line was cut: it ends in `…`, or its words start with
/// one (a `said:` cut keeping its end).
fn is_cut(line: &str) -> bool {
    line.ends_with('…')
        || line
            .split_once(": ")
            .is_some_and(|(_, words)| words.starts_with('…'))
}

/// THE NARROWING: the client cuts every excerpt it cuts to ONE width, the WIDEST at which the read fits.
#[must_use]
pub fn judge_narrowing(text: &str, budget: usize, budget_cut: bool) -> Check {
    const NAME: &str = "the excerpts narrowed alike, as wide as the budget lets them";
    let rows: Vec<&str> = text.lines().filter(|l| is_excerpt_row(l)).collect();
    let cut: Vec<usize> = rows
        .iter()
        .filter(|l| is_cut(l))
        .map(|l| l.chars().count())
        .collect();
    let Some(&width) = cut.iter().max() else {
        return Check::new(
            NAME,
            true,
            format!(
                "none of {} excerpt lines cut{}",
                rows.len(),
                if budget_cut {
                    ""
                } else {
                    ": the whole read fits"
                }
            ),
        );
    };
    let alike = cut.iter().all(|&w| w == width);
    let chars = text.chars().count();
    let wider = chars + cut.len();
    let ok = alike
        && if budget_cut {
            width == aterm_ctl::EXCERPT_CHARS
        } else {
            width >= aterm_ctl::EXCERPT_CHARS
                && (width == aterm_ctl::EXCERPT_WIDE_CHARS || wider > budget)
        };
    Check::new(
        NAME,
        ok,
        format!(
            "{} of {} excerpt lines cut at {width} chars{}; {} chars at {}, budget {budget}",
            cut.len(),
            rows.len(),
            if alike { "" } else { " (NOT alike)" },
            wider,
            width + 1,
        ),
    )
}

/// THE ESCAPING: nothing a session wrote reaches the reader raw.
#[must_use]
pub fn judge_escaped(text: &str, escaped: &[&str], forbidden: &[&str]) -> Check {
    let raw = raw_active(text);
    let absent: Vec<&str> = escaped
        .iter()
        .copied()
        .filter(|e| !text.contains(e))
        .collect();
    let present: Vec<&str> = forbidden
        .iter()
        .copied()
        .filter(|f| text.contains(f))
        .collect();
    Check::new(
        "injected escapes arrive escaped (tty::one_line)",
        raw.is_empty() && absent.is_empty() && present.is_empty(),
        format!(
            "raw active characters {:?}; escaped forms not found {absent:?}; raw forms found {present:?}",
            raw.iter()
                .map(|c| format!("U+{:04X}", *c as u32))
                .collect::<Vec<_>>()
        ),
    )
}

/// Every session's rank in a `survey --json` read, by sid: the client's own ([`aterm_ctl::survey_rank`] over the record's string members), which [`judge_text`] holds a cut read's excerpts to.
#[must_use]
pub fn ranks(json: &str) -> BTreeMap<String, u8> {
    let parsed: Option<Value> = aterm_json::from_str(json).ok();
    let mut out = BTreeMap::new();
    for host in parsed
        .as_ref()
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let sessions = host.get("sessions").and_then(Value::as_array);
        for row in sessions.into_iter().flatten() {
            let (Some(sid), Some(members)) = (str_of(row, "sid"), row.as_object()) else {
                continue;
            };
            let fields = members
                .iter()
                .filter_map(|(k, v)| Some((k.as_str(), v.as_str()?)));
            out.insert(sid.to_owned(), aterm_ctl::survey_rank(fields));
        }
    }
    out
}

/// A JSON reply's instance object's string `key`.
fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// The content fields a survey row carries only at `content`.
pub const CONTENT_KEYS: &[&str] = &[
    "user_title",
    "title",
    "cwd",
    "branch",
    "prompt",
    "said",
    "cmd",
    "rows",
];

/// THE HEALTHY FLEET READ's JSON: an array of one object per instance, the peers marked `"fleet":true`, every one at `level` content, every session the simulation made with its program and agent word, a Claude session's `prompt` and `said`, a shell's `cwd`; and no raw active character.
#[must_use]
pub fn judge_json(json: &str, want: &[WantInstance]) -> Vec<Check> {
    let mut checks = Vec::new();
    let raw = raw_active(json.trim_end_matches('\n'));
    checks.push(Check::new(
        "json carries no raw active character",
        raw.is_empty(),
        format!("{} found", raw.len()),
    ));
    let parsed: Result<Value, _> = aterm_json::from_str(json);
    let Some(hosts) = parsed.as_ref().ok().and_then(Value::as_array) else {
        checks.push(Check::new(
            "json parses as an array",
            false,
            format!("{:?}", parsed.err()),
        ));
        return checks;
    };
    checks.push(Check::new(
        "json: one object per instance",
        hosts.len() == want.len(),
        format!("{} objects for {} instances", hosts.len(), want.len()),
    ));
    let mut wrong = Vec::new();
    for w in want {
        let host = hosts.iter().find(|h| {
            h.get("instance")
                .and_then(|i| str_of(i, "pid"))
                .and_then(|p| p.parse::<i32>().ok())
                == Some(w.pid)
        });
        let Some(host) = host else {
            wrong.push(format!("m{:02}: no object", w.i));
            continue;
        };
        let fleet = host.get("fleet").and_then(Value::as_bool) == Some(true);
        if fleet == w.local {
            wrong.push(format!("m{:02}: fleet={fleet}", w.i));
        }
        let level = host.get("instance").and_then(|i| str_of(i, "level"));
        if level != Some("content") {
            wrong.push(format!("m{:02}: level {level:?}", w.i));
        }
        let sessions: &[Value] = host
            .get("sessions")
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice);
        for s in &w.sessions {
            let Some(row) = sessions.iter().find(|r| str_of(r, "sid") == Some(&s.sid)) else {
                wrong.push(format!("m{:02} {}: no row", w.i, s.sid));
                continue;
            };
            let program = str_of(row, "program");
            let agent = str_of(row, "agent");
            if program != Some(s.program.as_str()) || agent != Some(s.agent.as_str()) {
                wrong.push(format!(
                    "m{:02} {}: program={program:?} agent={agent:?}",
                    w.i, s.sid
                ));
            }
            let has = |k: &str| str_of(row, k).is_some_and(|v| v != "-" && !v.is_empty());
            let content = if s.program == "claude" {
                has("prompt") && has("said")
            } else {
                has("cwd")
            };
            if !content {
                wrong.push(format!("m{:02} {}: no excerpt", w.i, s.sid));
            }
        }
    }
    checks.push(Check::new(
        "json: every session with its program, agent word and excerpt",
        wrong.is_empty(),
        if wrong.is_empty() {
            "all".to_owned()
        } else {
            wrong.join(" / ")
        },
    ));
    checks
}

/// A FACTS-ONLY machine in a fleet read's JSON: the instance at `pid` answers at `level` facts, no row carries a content field, `attention` is `1` or `-`, and none of `secrets` (a title a person typed, words a session wrote) appears anywhere in the reply.
#[must_use]
pub fn judge_facts_only(json: &str, pid: i32, secrets: &[&str]) -> Check {
    let parsed: Result<Value, _> = aterm_json::from_str(json);
    let host = parsed
        .as_ref()
        .ok()
        .and_then(Value::as_array)
        .and_then(|a| {
            a.iter().find(|h| {
                h.get("instance")
                    .and_then(|i| str_of(i, "pid"))
                    .and_then(|p| p.parse::<i32>().ok())
                    == Some(pid)
            })
        });
    let Some(host) = host else {
        return Check::new(
            "a member at allow=facts answers facts only",
            false,
            "its instance is not in the reply",
        );
    };
    let level = host.get("instance").and_then(|i| str_of(i, "level"));
    let rows: &[Value] = host
        .get("sessions")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);
    let carrying: Vec<String> = rows
        .iter()
        .flat_map(|r| {
            CONTENT_KEYS
                .iter()
                .filter(|k| r.get(k).is_some())
                .map(|k| format!("{}:{k}", str_of(r, "sid").unwrap_or("?")))
                .collect::<Vec<_>>()
        })
        .collect();
    let attention_words = rows
        .iter()
        .filter_map(|r| str_of(r, "attention"))
        .any(|a| a != "-" && a != "1");
    let leaked: Vec<&str> = secrets
        .iter()
        .copied()
        .filter(|s| json.contains(s))
        .collect();
    Check::new(
        "a member at allow=facts answers facts only",
        level == Some("facts")
            && !rows.is_empty()
            && carrying.is_empty()
            && !attention_words
            && leaked.is_empty(),
        format!(
            "level {level:?}, {} rows, content fields {carrying:?}, attention words {attention_words}, leaked {leaked:?}",
            rows.len()
        ),
    )
}

/// THE FAULT READ's text: the stopped machine's and the killed machine's ONE row each, with their reasons (`connected, but no fleet handshake within …` and `nothing listens on 127.0.0.1:<port>`), and every other machine's block.
#[must_use]
pub fn judge_faults(
    text: &str,
    answered: usize,
    stopped_port: u16,
    killed_port: u16,
) -> Vec<Check> {
    let blocks = blocks(text);
    let stopped = unanswered_rows(text, stopped_port);
    let killed = unanswered_rows(text, killed_port);
    let ok_blocks = blocks
        .iter()
        .filter(|b| !b.header.contains(" did not answer: "))
        .count();
    vec![
        Check::new(
            "the SIGSTOPped machine is one row with its reason",
            stopped.len() == 1 && stopped[0].contains("connected, but no fleet handshake within"),
            stopped.join(" / "),
        ),
        Check::new(
            "the killed machine is one row with its reason",
            killed.len() == 1
                && killed[0].contains(&format!("nothing listens on 127.0.0.1:{killed_port}")),
            killed.join(" / "),
        ),
        Check::new(
            "every other machine answered",
            ok_blocks == answered && blocks.len() == answered + 2,
            format!(
                "{ok_blocks} answered of {} blocks; {answered} expected",
                blocks.len()
            ),
        ),
    ]
}

#[cfg(test)]
#[path = "fleet_judge_tests.rs"]
mod tests;
