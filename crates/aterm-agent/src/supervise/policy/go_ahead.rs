// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE GO-AHEAD READER (owner, 2026-10-29: "how dare you block on me for go ahead!"): whether a
//! reply parks the agent's OWN REVERSIBLE work on a person's go-ahead, and what Autopilot Cruise
//! and Land type to one ([`RULE_GO_AHEAD`]). Anything outward, irreversible, someone else's or
//! only a person's keeps the policy's other acts. It is lexical, like every reader beside it
//! (ATERM-KI-1020-2): `fixtures/go-ahead-table.txt` is the behaviour it holds to.

use crate::harness::harness_mark;

use super::needs;
use super::turn_end::{DESTRUCTIVE_WORDS, fit_units};

/// The answer's rule: the owner's standing go-ahead for the agent's own reversible work.
pub const RULE_GO_AHEAD: &str = "go-ahead@v1";

/// THE OWNER'S STANDING GO-AHEAD: what [`RULE_GO_AHEAD`] types, its limits before the owner's words.
pub const GO_AHEAD_ANSWER: &str = concat!(
    harness_mark!(),
    " Proceed now with your own work: build, test, commit, and push or merge your own branch to \
     its main. Not covered: anything that deletes, overwrites, resets or force-pushes, releases, \
     deploys, tags, publishes, sends, posts or spends money, or that another session or person \
     made. A go-ahead for your own reversible work is never a NEEDS YOU item; the owner ruled \
     it: \"how dare you block on me for go ahead!\""
);

/// The sentence Cruise's continuation and its carry say outside a person's hold
/// ([`super::turn_end::TurnEndState::go_ahead_held`]); within one it is cut
/// ([`unless_held`]), as the go-ahead itself is not typed there.
pub const NEVER_A_NEED: &str = "A go-ahead for your own reversible work is never a NEEDS YOU item.";

/// `text` without [`NEVER_A_NEED`] where a person's turn `held` the go-ahead: they may just have
/// said to hold.
#[must_use]
pub fn unless_held(text: &str, held: bool) -> std::borrow::Cow<'_, str> {
    if !held || !text.contains(NEVER_A_NEED) {
        return std::borrow::Cow::Borrowed(text);
    }
    let spaced = format!(" {NEVER_A_NEED}");
    let cut = if text.contains(&spaced) {
        spaced.as_str()
    } else {
        NEVER_A_NEED
    };
    std::borrow::Cow::Owned(text.replacen(cut, "", 1))
}

/// Whether a person's own message `said` holds the go-ahead, in the first person: `until I say`,
/// `once I've reviewed it`, `wait for my go-ahead`, `hold off`, `don't push`, `without my OK`,
/// `ask me first`. A turn it opened holds the go-ahead until their next turn
/// ([`super::turn_end::TurnEndState::go_ahead_held`]), not for an hour.
#[must_use]
pub fn holds(said: &str) -> bool {
    const WHEN: &[&str] = &["until", "till", "unless", "before", "once", "when", "after"];
    const SAYS: &[&str] = &[
        "say", "says", "tell", "give", "ok", "okay", "approve", "confirm", "review", "look",
        "check", "decide", "sign", "agree", "ask",
    ];
    const NOT: &[&str] = &["don't", "dont", "do", "never"];
    const BACK: &[&str] = &["back", "here", "done", "ready"];
    const WORK_HELD: &[&str] = &[
        "push", "merge", "land", "commit", "rebase", "build", "rebuild", "restart", "rerun", "run",
        "deploy", "publish", "release", "delete", "remove", "tag", "ship", "touch", "start",
        "continue", "proceed",
    ];
    const PHRASES: &[&[&str]] = &[
        &["hold", "off"],
        &["hold", "on"],
        &["hold", "it"],
        &["ask", "me"],
        &["check", "with", "me"],
        &["wait", "for", "me"],
        &["wait", "on", "me"],
        &["wait", "for", "my"],
        &["wait", "on", "my"],
        &["await", "my"],
        &["without", "my"],
        &["without", "asking"],
        &["my", "go-ahead"],
        &["my", "go", "ahead"],
        &["my", "ok"],
        &["my", "okay"],
        &["my", "approval"],
        &["my", "say-so"],
        &["my", "sign-off"],
        &["my", "signal"],
    ];
    let unit = Unit::new(said, false);
    (0..unit.len()).any(|i| {
        if PHRASES.iter().any(|p| unit.at(i, p)) {
            return true;
        }
        let w = unit.word(i);
        if WHEN.contains(&w) && matches!(unit.word(i + 1), "i" | "i'm" | "i've") {
            let act = skip_adverb_word(&unit, i + 2);
            if inflects_any(act, SAYS) || BACK.contains(&act) {
                return true;
            }
        }
        let negated = NOT.contains(&w) && (w != "do" || unit.word(i + 1) == "not");
        negated && {
            let at = i + 1 + usize::from(w == "do");
            let at = at + usize::from(matches!(unit.word(at), "yet" | "ever" | "even"));
            inflects_any(unit.word(at), WORK_HELD)
        }
    })
}

/// The word at `i`, past an adverb ([`SLOT_ADVERBS`]) there.
fn skip_adverb_word(unit: &Unit, i: usize) -> &str {
    unit.word(skip_adverb(unit, i))
}

/// The answer's sentence for a park that names memory or disk room ([`Park::waits_room`]): Claude
/// Code's low-memory stop note says to start the work again only when asked. It names waits
/// `aterm ctl @self await sys` takes (a bare one is refused): memory's, and disk's predicate.
pub const GO_AHEAD_ROOM: &str = "This answers a stop note's \"only when asked\": wait for room first \
                                 (`aterm ctl @self await sys mem=normal`, or `'free>=room'` for \
                                 disk).";

/// The answer's sentence for a park on a `NEEDS YOU:` line ([`Park::need`]): the line goes now,
/// not once the work runs, so a line kept after it is the agent's own.
pub const GO_AHEAD_DROP: &str = "Drop that NEEDS YOU line now.";

/// The answer's sentence when the reply's other needs are a person's ([`Park::others`]).
pub const GO_AHEAD_OTHERS: &str = "Act on none of your other NEEDS YOU lines.";

/// The most UTF-16 code units of the park's words the answer quotes.
pub const QUOTE_UNITS: usize = 150;

/// The most UTF-16 code units of the word the agent asked the person to say.
pub const TOKEN_UNITS: usize = 80;

/// A reply's park on a person's go-ahead for its own reversible work ([`parked`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Park {
    /// The sentence or need that parks, as written (hidden characters spaced, cut to [`QUOTE_UNITS`]).
    pub quote: String,
    /// The word the agent asked the person to say (`say "rerun the checks"`), as written.
    pub token: Option<String>,
    /// The park names low memory, disk or room: the answer says to wait for room first.
    pub waits_room: bool,
    /// The park is a `NEEDS YOU:` line.
    pub need: bool,
    /// Another `NEEDS YOU:` line of the reply asks what only a person can give.
    pub others: bool,
    /// What the repeat count keys on ([`super::turn_end`]): the token, lowercased with its digits
    /// folded, or empty for a park in prose (a rephrased refusal is the same refusal).
    pub mark: String,
    /// The park names a branch, a worktree or main: work to land by its own words, wherever the
    /// worktree is (Land's go-ahead rides it, [`super::turn_end::TurnEndState::go_ahead_owed`]).
    pub lands: bool,
}

impl Park {
    /// The answer's sentence naming the park, in at most `room` units: the token when there is
    /// one, else the quote; empty when even its frame does not fit.
    #[must_use]
    pub fn said(&self, room: usize) -> String {
        let (open, close, words, most) = match &self.token {
            Some(t) => (
                "You asked the owner to say \"",
                "\": consider it said.",
                t,
                TOKEN_UNITS,
            ),
            None => ("You parked on: \"", "\".", &self.quote, QUOTE_UNITS),
        };
        let frame = open.encode_utf16().count() + close.encode_utf16().count();
        let fit = fit_units(words, most.min(room.saturating_sub(frame)));
        if fit.is_empty() {
            return String::new();
        }
        format!("{open}{fit}{close}")
    }
}

/// Whether `words` park the agent's own reversible work on a person's go-ahead, read as
/// [`parked`] reads a reply.
#[must_use]
pub fn parks(words: &str) -> bool {
    parked("", words).is_some()
}

/// Whether `text`'s words of the go-ahead ask for one: a go-ahead it names counts outside an echo
/// of an answer (`the standing go-ahead`, `got the go-ahead`, the harness's own words), each
/// mention judged on its own, so an echo beside it, in its sentence or the next, echoes another
/// go-ahead; a table row Claude Code draws (`│ … │`) and a `>` quote count too, as before this
/// reader. Where `outward` (the reply names a destructive act) every other mention asks; where
/// not, a mention asks unless a negation undoes its need or wait (`I no longer need your
/// go-ahead`), it is a report (`Rebuilt and pushed with your go-ahead`, `without waiting for
/// your go-ahead`, `the rebuild that was waiting for your go-ahead`), or it names a thing (`a
/// go-ahead reader`). So `Give me the go-ahead and I'll rotate the keys` still asks, as the
/// turn-end policy's stop phrases of the go-ahead family always read.
#[must_use]
pub fn asks_go_ahead(text: &str, outward: bool) -> bool {
    let mut units: Vec<Unit> = sentences(text)
        .iter()
        .map(|(s, code)| Unit::coded(s, false, code))
        .collect();
    units.extend(quoted_cells(text).iter().map(|c| Unit::new(c, false)));
    units.iter().any(|unit| {
        let spans = go_ahead_words(unit);
        let words: Vec<&str> = spans.iter().map(|(_, w)| *w).collect();
        let harness = unit.lower.contains("[aterm harness]") && !unit.negated();
        (0..words.len()).any(|j| {
            // A thing the go-ahead names follows it with no mark between (`a go-ahead reader`,
            // not `your go-ahead, tests pass`).
            let names = spans.get(j + 1).is_some_and(|(next, w)| {
                NAMED.contains(w) && unit.lower[spans[j].0..*next].trim_end().ends_with("ahead")
            });
            words[j] == "go-ahead"
                && !harness
                && !echoed(&words, j)
                && (outward || (!names && asked_at(&words, j)))
        })
    })
}

/// The cells of `text`'s rows that [`sentences`] drops as quotes: a `>` quote's row, or a table
/// row Claude Code draws (`│ delete old tags │ waiting for your go-ahead │`), cut at its bars.
fn quoted_cells(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|t| t.starts_with(QUOTE_ROW_MARKS))
        .flat_map(|t| t.split(['│', '┃', '|']))
        .map(|c| c.trim().trim_start_matches(['>', '▎']).trim().to_string())
        .filter(|c| !c.is_empty())
        .collect()
}

/// `unit`'s words, each with its byte offset in `unit.lower`, and each go-ahead one word
/// (`go-ahead`): `goahead`, and `go ahead` in any use.
fn go_ahead_words(unit: &Unit) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some((at, w)) = unit.words.get(i) {
        if w == "go" && unit.word(i + 1) == "ahead" {
            out.push((*at, "go-ahead"));
            i += 2;
            continue;
        }
        out.push((
            *at,
            if w == "goahead" {
                "go-ahead"
            } else {
                w.as_str()
            },
        ));
        i += 1;
    }
    out
}

/// The echoes of an answer that name the go-ahead itself: a mention inside one asks nothing,
/// unless a negation within three words before it undoes it (`I don't have the go-ahead`).
const ECHOES: &[&[&str]] = &[
    &["per", "the", "go-ahead"],
    &["as", "the", "go-ahead"],
    &["the", "go-ahead", "says"],
    &["standing", "go-ahead"],
    &["go-ahead", "stands"],
    &["got", "the", "go-ahead"],
    &["have", "the", "go-ahead"],
    &["had", "the", "go-ahead"],
    &["given", "the", "go-ahead"],
    &["gave", "the", "go-ahead"],
    &["got", "your", "go-ahead"],
    &["have", "your", "go-ahead"],
    &["a", "go-ahead", "for", "your", "own", "reversible", "work"],
    &["how", "dare", "you", "block", "on", "me", "for", "go-ahead"],
];

/// Whether the go-ahead at `words[j]` sits inside an echo ([`ECHOES`]) no negation undoes.
fn echoed(words: &[&str], j: usize) -> bool {
    ECHOES.iter().any(|p| {
        p.iter().enumerate().any(|(k, w)| {
            *w == "go-ahead"
                && j.checked_sub(k).is_some_and(|start| {
                    words.get(start..start + p.len()) == Some(*p) && !negated_before(words, start)
                })
        })
    })
}

/// Whether a negation comes within three words before `words[i]`.
fn negated_before(words: &[&str], i: usize) -> bool {
    words[i.saturating_sub(3)..i].iter().any(|w| {
        matches!(
            *w,
            "not" | "no" | "never" | "nothing" | "nobody" | "none" | "longer"
        ) || w.ends_with("n't")
    })
}

/// The words after a go-ahead that make it a thing's name, not consent (`a go-ahead reader`).
const NAMED: &[&str] = &[
    "reader",
    "readers",
    "table",
    "tables",
    "rule",
    "rules",
    "answer",
    "answers",
    "act",
    "acts",
    "text",
    "texts",
    "sentence",
    "sentences",
    "phrase",
    "phrases",
    "row",
    "rows",
    "line",
    "lines",
    "family",
    "families",
    "test",
    "tests",
    "fix",
    "fixes",
    "change",
    "changes",
    "code",
    "logic",
    "policy",
    "counter",
    "count",
    "bound",
    "exception",
    "check",
    "checks",
    "case",
    "cases",
    "corpus",
    "wording",
];

/// The past forms no `-ed` ending names that make a `with your go-ahead` a report.
const DONE_FORMS: &[&str] = &[
    "built", "rebuilt", "ran", "reran", "made", "did", "done", "went", "got", "put", "set", "kept",
    "left", "took", "wrote", "began", "sent", "shipped",
];

/// Whether the go-ahead at `words[j]`, where no destructive act stands beside it, asks for one:
/// no negation undoes the need or wait before it (`no longer need`, `don't have to wait for`,
/// `no go-ahead needed`), and it is no report (`pushed with your go-ahead`, `without waiting
/// for your go-ahead`, a wait in the past tense).
fn asked_at(words: &[&str], j: usize) -> bool {
    let word = |i: usize| words.get(i).copied().unwrap_or("");
    let lead = j.saturating_sub(usize::from(matches!(
        word(j.wrapping_sub(1)),
        "your" | "the" | "a" | "an" | "my" | "their" | "owner" | "person" | "user" | "any"
    )));
    if word(lead.wrapping_sub(1)) == "no" {
        return false;
    }
    let waits = |w: &str| {
        w.starts_with("wait")
            || w.starts_with("await")
            || matches!(
                w,
                "need" | "needs" | "needed" | "require" | "requires" | "requiring"
            )
    };
    if let Some(k) = (lead.saturating_sub(4)..lead)
        .rev()
        .find(|&k| waits(word(k)))
    {
        if negated_before(words, k) {
            return false;
        }
        let w = word(k);
        let past = matches!(word(k.wrapping_sub(1)), "was" | "were")
            || (word(k.wrapping_sub(1)) == "been"
                && (word(k.wrapping_sub(2)) == "had" || word(k.wrapping_sub(2)).ends_with("'d")));
        if (w.starts_with("wait") || w.starts_with("await"))
            && (past || word(k.wrapping_sub(1)) == "without")
        {
            return false;
        }
    }
    let by = word(lead.wrapping_sub(1));
    if matches!(by, "with" | "without") {
        let at = lead - 1;
        let window = at.saturating_sub(6)..at;
        // A verb in the past tense before it, not a state (`I'm blocked`) or a thing (`the
        // merged branch`), and no intent beside it (`I'll push the merged branch`).
        let done = window.clone().any(|k| {
            let w = word(k);
            let before = word(k.wrapping_sub(1));
            let state = matches!(
                before,
                "i'm" | "am" | "is" | "are" | "stay" | "stays" | "remain" | "remains"
            );
            let thing = matches!(
                before,
                "the" | "a" | "an" | "my" | "your" | "our" | "its" | "this" | "that" | "their"
            );
            !state && !thing && ((w.len() > 3 && w.ends_with("ed")) || DONE_FORMS.contains(&w))
        });
        let intent = window.into_iter().any(|k| {
            matches!(
                word(k),
                "will" | "i'll" | "we'll" | "would" | "i'd" | "can" | "could" | "shall" | "want"
            )
        });
        if done && !intent {
            return false;
        }
    }
    true
}

/// The park on a person's go-ahead that `turn_said` (the turn's earlier messages) and `tail` (its
/// last) make, if any: a consent ask with a pending marker, own reversible work from the allow
/// list named in the same sentence or need, and no exclusion (only a person's, outward or
/// irreversible, someone else's, a hold the person gave, a decline, a choice) in it, the code it
/// carries or the sentence before, nor only a person's in the sentence after. An outward or
/// destructive act the turn's prose names anywhere and ties to the work the go-ahead would cover
/// ([`planned`]: `the next push runs the publish`, `Plan: delete the tags, then push`) is one the
/// go-ahead would be read as covering: such a turn parks nothing.
#[must_use]
pub fn parked(turn_said: &str, tail: &str) -> Option<Park> {
    let text = if turn_said.trim().is_empty() {
        tail.to_string()
    } else {
        format!("{turn_said}\n\n{tail}")
    };
    let prose = needs::without_needs(&text);
    let units: Vec<Unit> = sentences(&prose)
        .iter()
        .map(|(s, code)| Unit::coded(s, false, code))
        .collect();
    if kept(Part::Outward) && units.iter().any(planned) {
        return None;
    }
    let mut park: Option<(Unit, Option<String>)> = None;
    let mut others = false;
    let mut room = false;
    for need in needs::need_words(&text) {
        let (words, code) = uncoded(&need);
        let code: Vec<&str> = code.iter().map(|(_, c)| c.as_str()).collect();
        let unit = Unit::coded(&words, true, &code.join(" "));
        match judged(&unit, None, None) {
            Some(token) => {
                room |= names_room(&unit);
                if park.is_none() {
                    park = Some((unit, token));
                }
            }
            None => others |= !needs::sanitized(&need).is_empty(),
        }
    }
    for (i, unit) in units.iter().enumerate() {
        let before = i.checked_sub(1).map(|b| &units[b]);
        let after = units.get(i + 1);
        if let Some(token) = judged(unit, before, after) {
            room |= [before, Some(unit), after]
                .into_iter()
                .flatten()
                .any(names_room);
            match &mut park {
                None => park = Some((unit.clone(), token)),
                Some((_, held)) if held.is_none() => *held = token,
                Some(_) => {}
            }
        }
    }
    let (unit, token) = park?;
    let token = token
        .map(|t| needs::sanitized(&t))
        .filter(|t| !t.is_empty());
    let mark = token.as_deref().map_or_else(String::new, |t| {
        t.chars()
            .map(|c| if c.is_ascii_digit() { '#' } else { lowered(c) })
            .collect()
    });
    Some(Park {
        quote: fit_units(&needs::sanitized(&unit.said), QUOTE_UNITS).into_owned(),
        token,
        waits_room: room,
        need: unit.need,
        others,
        mark,
        lands: unit.any(&["branch", "branches", "worktree", "worktrees", "main"]),
    })
}

/// Whether a sentence of the turn's prose names an outward or destructive act ([`outward`]) as
/// part of the work the go-ahead would cover: beside a push, merge, landing or rebase (a
/// `force-push` included), as a plan (`next`, `then`, `plan`, `will`, `I'll`, `also`), or asked
/// (`Should I force-push the old branch?`). A report of what a test checks (`it fails if that
/// fallback is removed`) or of another's files (`deleting it is up to the person`) plans
/// nothing.
fn planned(unit: &Unit) -> bool {
    outward(unit, None)
        && (unit.words.iter().any(|(_, w)| {
            w.split('-')
                .any(|part| inflects_any(part, &["push", "merge", "land", "rebase"]))
        }) || unit.any(&["next", "then", "plan", "will", "i'll", "also"])
            || unit.lower.trim_end().ends_with('?'))
}

/// One sentence or need the reader judges: its words as written (quote marks and dashes
/// folded), lowercased (one char for one, so a range of one is a range of the other), its
/// words with their byte offsets in the lowercased form, and the code it carried (inline spans,
/// a fenced block after its paragraph), which only the exclusions read.
#[derive(Clone)]
struct Unit {
    said: String,
    lower: String,
    words: Vec<(usize, String)>,
    need: bool,
    code: String,
}

impl Unit {
    fn new(said: &str, need: bool) -> Self {
        Self::coded(said, need, "")
    }

    fn coded(said: &str, need: bool, code: &str) -> Self {
        let said: String = said.chars().map(folded).collect();
        let lower: String = said.chars().map(lowered).collect();
        let words = words_of(&lower);
        Self {
            said,
            lower,
            words,
            need,
            code: code.to_string(),
        }
    }

    fn word(&self, i: usize) -> &str {
        self.words.get(i).map_or("", |(_, w)| w.as_str())
    }

    fn before(&self, i: usize, back: usize) -> &str {
        i.checked_sub(back).map_or("", |b| self.word(b))
    }

    fn len(&self) -> usize {
        self.words.len()
    }

    fn at(&self, i: usize, phrase: &[&str]) -> bool {
        phrase
            .iter()
            .enumerate()
            .all(|(k, p)| self.words.get(i + k).is_some_and(|(_, w)| w == p))
    }

    fn has(&self, phrase: &[&str]) -> bool {
        (0..self.len()).any(|i| self.at(i, phrase))
    }

    fn any(&self, words: &[&str]) -> bool {
        self.words.iter().any(|(_, w)| words.contains(&w.as_str()))
    }

    /// Whether the unit is negated (`not`, `never`, `won't`, `no longer`, `refuse`).
    fn negated(&self) -> bool {
        self.negated_outside(0..0)
    }

    /// Whether a word outside `skip` (word indexes) negates the unit, as [`Self::negated`] reads.
    fn negated_outside(&self, skip: std::ops::Range<usize>) -> bool {
        (0..self.len()).filter(|i| !skip.contains(i)).any(|i| {
            let w = self.word(i);
            matches!(
                w,
                "not" | "never" | "cannot" | "refuse" | "refuses" | "refused" | "refusing"
            ) || w.ends_with("n't")
                || (w == "no" && self.word(i + 1) == "longer")
        })
    }

    /// Whether a negation comes within three words before word `i` (`don't have to wait`, `no
    /// need to wait`, `nothing here is waiting`).
    fn undone(&self, i: usize) -> bool {
        (1..=3).any(|back| {
            let w = self.before(i, back);
            matches!(w, "not" | "no" | "never" | "nothing" | "nobody" | "none")
                || w.ends_with("n't")
        })
    }

    /// The text of `lower`'s char range `from..to` (byte offsets) as written.
    fn said_at(&self, from: usize, to: usize) -> String {
        let skip = self.lower[..from].chars().count();
        let take = self.lower[from..to].chars().count();
        self.said.chars().skip(skip).take(take).collect()
    }
}

fn folded(c: char) -> char {
    match c {
        '\u{2018}' | '\u{2019}' => '\'',
        '\u{201C}' | '\u{201D}' => '"',
        '\u{2010}'..='\u{2013}' | '\u{2212}' => '-',
        c => c,
    }
}

fn lowered(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn words_of(lower: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut start = None;
    let keep = |c: char| c.is_alphanumeric() || matches!(c, '-' | '\'' | '$');
    for (at, c) in lower
        .char_indices()
        .chain(std::iter::once((lower.len(), ' ')))
    {
        match (start, keep(c)) {
            (None, true) => start = Some(at),
            (Some(s), false) => {
                let raw = &lower[s..at];
                let w = raw.trim_start_matches(['-', '\'']);
                let from = s + raw.len() - w.len();
                let w = w.trim_end_matches(['-', '\'']);
                let w = w.strip_suffix("'s").unwrap_or(w);
                if !w.is_empty() {
                    out.push((from, w.to_string()));
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// `word` is `base` or one of its inflections: plurals, tenses (`overwritten`, `overwrote`), a
/// doubled final consonant, and the nouns `-ion`, `-ation`, `-ment` and `-al` make
/// (`deployment`, `migration`, `removal`).
fn inflects(word: &str, base: &str) -> bool {
    if word == base {
        return true;
    }
    if let Some(stem) = base.strip_suffix("write")
        && word == format!("{stem}wrote")
    {
        return true;
    }
    const ENDS: &[&str] = &[
        "s", "es", "d", "ed", "ing", "ten", "ion", "ions", "ation", "ations", "ment", "ments", "al",
    ];
    let stem = base.strip_suffix('e').unwrap_or(base);
    if [base, stem].iter().any(|s| {
        word.strip_prefix(s)
            .is_some_and(|rest| ENDS.contains(&rest))
    }) {
        return true;
    }
    if let Some(y) = base.strip_suffix('y')
        && word
            .strip_prefix(y)
            .is_some_and(|rest| matches!(rest, "ies" | "ied" | "ication" | "ications"))
    {
        return true;
    }
    let last = base.chars().last().unwrap_or(' ');
    word.strip_prefix(base)
        .and_then(|rest| rest.strip_prefix(last))
        .is_some_and(|rest| matches!(rest, "ed" | "ing" | "er"))
}

fn inflects_any(word: &str, bases: &[&str]) -> bool {
    bases.iter().any(|b| inflects(word, b))
}

const QUOTE_ROW_MARKS: [char; 4] = ['>', '▎', '│', '┃'];

/// The sentences `prose` reads as, each with the code its words carried: quoted rows dropped,
/// wrapped rows of a paragraph joined, inline code and attributed quotes dropped, split at `. !
/// ? ;` and a space. The inline code goes beside its sentence and a fenced block beside the last
/// sentence of the paragraph before it, where only the exclusions read them.
fn sentences(prose: &str) -> Vec<(String, String)> {
    let mut paragraphs: Vec<(String, String)> = Vec::new();
    let mut open = String::new();
    let mut fence = None;
    let flush = |open: &mut String, paragraphs: &mut Vec<(String, String)>| {
        if !open.trim().is_empty() {
            paragraphs.push((std::mem::take(open), String::new()));
        }
        open.clear();
    };
    for row in prose.lines() {
        let t = row.trim();
        if needs::fenced_line(row, &mut fence) {
            flush(&mut open, &mut paragraphs);
            if let Some((_, fenced)) = paragraphs.last_mut() {
                fenced.push_str(t);
                fenced.push('\n');
            }
            continue;
        }
        if t.is_empty() || t.starts_with(QUOTE_ROW_MARKS) {
            flush(&mut open, &mut paragraphs);
            continue;
        }
        if list_start(t) {
            flush(&mut open, &mut paragraphs);
        }
        if !open.is_empty() && !open.ends_with('-') {
            open.push(' ');
        }
        open.push_str(t);
    }
    flush(&mut open, &mut paragraphs);
    let mut out = Vec::new();
    for (paragraph, fenced) in &paragraphs {
        let (words, spans) = uncoded(paragraph);
        let mut said: Vec<(String, String)> = split_sentences(&words)
            .into_iter()
            .map(|(range, sentence)| {
                let code: Vec<&str> = spans
                    .iter()
                    .filter(|(at, _)| range.contains(at))
                    .map(|(_, c)| c.as_str())
                    .collect();
                (sentence, code.join(" "))
            })
            .collect();
        if let Some((_, code)) = said.last_mut()
            && !fenced.is_empty()
        {
            code.push(' ');
            code.push_str(fenced);
        }
        out.append(&mut said);
    }
    out
}

fn list_start(t: &str) -> bool {
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("• ") {
        return true;
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") "))
}

/// The verbs that ask the person to say a word (`say "rerun the checks"`).
const SAYS: &[&str] = &["say", "type", "reply", "answer", "send", "write"];

/// The verbs that attribute a quote to someone (`the note said "…"`, `the worker posted: "…"`):
/// its words are theirs.
const ATTRIBUTES: &[&str] = &[
    "wrote",
    "said",
    "asked",
    "commented",
    "says",
    "reads",
    "read",
    "posted",
    "replied",
    "reported",
    "printed",
    "relayed",
];

/// `paragraph` without its inline code (kept, as a quote, right after a [`SAYS`] verb) and the
/// quotes an [`ATTRIBUTES`] verb attributes, and the code it dropped, each span with the byte
/// offset in the text returned where it stood.
fn uncoded(paragraph: &str) -> (String, Vec<(usize, String)>) {
    let mut out = String::new();
    let mut code = Vec::new();
    let mut rest = paragraph;
    while let Some(at) = rest.find(['`', '"', '\u{201C}']) {
        let mark = rest[at..].chars().next().unwrap_or('"');
        let close = if mark == '\u{201C}' { '\u{201D}' } else { mark };
        let after = &rest[at + mark.len_utf8()..];
        let Some(end) = after.find(close) else {
            break;
        };
        let lead = last_word(&format!("{out}{}", &rest[..at]));
        out.push_str(&rest[..at]);
        let inner = &after[..end];
        if mark == '`' {
            if SAYS.contains(&lead.as_str()) || lead == "with" {
                out.push('"');
                out.push_str(inner);
                out.push('"');
            } else {
                code.push((out.len(), inner.to_string()));
                out.push(' ');
            }
        } else if ATTRIBUTES.contains(&lead.as_str()) {
            out.push(' ');
        } else {
            out.push(mark);
            out.push_str(inner);
            out.push(close);
        }
        rest = &after[end + close.len_utf8()..];
    }
    out.push_str(rest);
    (out, code)
}

fn last_word(text: &str) -> String {
    text.trim_end()
        .trim_end_matches([':', ','])
        .rsplit(char::is_whitespace)
        .next()
        .unwrap_or_default()
        .chars()
        .map(lowered)
        .collect()
}

/// `paragraph`'s sentences, each with the byte range of `paragraph` it was cut from.
fn split_sentences(paragraph: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let mut out = Vec::new();
    let mut from = 0;
    let bytes = paragraph.as_bytes();
    let mut cut = |range: std::ops::Range<usize>| {
        let s = paragraph[range.clone()].trim();
        if !s.is_empty() {
            out.push((range, s.to_string()));
        }
    };
    for (i, b) in bytes.iter().enumerate() {
        if matches!(b, b'.' | b'!' | b'?' | b';') && matches!(bytes.get(i + 1), None | Some(b' ')) {
            cut(from..i + 1);
            from = i + 1;
        }
    }
    cut(from..paragraph.len());
    out
}

/// Whether `unit` parks, with the word it asked the person to say: a consent ask, own work,
/// no exclusion in it or in `before`, nothing only a person can give in `after`, and no answer
/// it echoes in it or `after`.
fn judged(unit: &Unit, before: Option<&Unit>, after: Option<&Unit>) -> Option<Option<String>> {
    let ask = asks(unit)?;
    if !works(unit, before, ask.stop_note) {
        return None;
    }
    if excluded(unit, ask.says_at)
        || before.is_some_and(|b| excluded(b, None))
        || after.is_some_and(|a| kept(Part::PersonOnly) && person_only(a))
    {
        return None;
    }
    if kept(Part::Echoes) && (echoes(unit) || after.is_some_and(echoes)) {
        return None;
    }
    Some(ask.token)
}

/// The reader's families and exclusions, each of which a test drops to show some table row
/// needs it ([`kept`]).
#[derive(Clone, Copy)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) enum Part {
    Consent = 1,
    Says = 1 << 1,
    Fixed = 1 << 2,
    Slots = 1 << 3,
    OwnInitiative = 1 << 4,
    StopNote = 1 << 5,
    PersonOnly = 1 << 6,
    PersonFirst = 1 << 7,
    Outward = 1 << 8,
    SomeoneElse = 1 << 9,
    Declined = 1 << 10,
    Chooses = 1 << 11,
    Echoes = 1 << 12,
    Attributed = 1 << 13,
}

#[cfg(test)]
thread_local! {
    /// The parts this thread's reader drops ([`Part`]).
    pub(super) static DROPPED: std::cell::Cell<u16> = const { std::cell::Cell::new(0) };
}

/// Whether the reader reads with `part` (every part, outside a test that drops one).
fn kept(part: Part) -> bool {
    #[cfg(test)]
    {
        DROPPED.with(|d| d.get() & part as u16 == 0)
    }
    #[cfg(not(test))]
    {
        let _ = part;
        true
    }
}

struct Ask {
    token: Option<String>,
    /// The [`SAYS`] verb's word index, which no exclusion reads (`send "go"` sends nothing).
    says_at: Option<usize>,
    /// A stop note's negated restart: the restart is the stopped work.
    stop_note: bool,
}

fn asks(unit: &Unit) -> Option<Ask> {
    if let Some((at, token)) = person_says(unit).filter(|_| kept(Part::Says)) {
        return Some(Ask {
            token,
            says_at: Some(at),
            stop_note: false,
        });
    }
    let stop_note = kept(Part::StopNote) && stop_note(unit);
    let asked = (kept(Part::Consent) && consent(unit))
        || (kept(Part::Fixed) && fixed_ask(unit))
        || (kept(Part::Slots) && slotted(unit))
        || (kept(Part::OwnInitiative) && on_my_own(unit));
    (asked || stop_note).then_some(Ask {
        token: None,
        says_at: None,
        stop_note,
    })
}

/// The words before a `go ahead` that make it the noun (`your go ahead`).
const DETERMINERS: &[&str] = &[
    "your", "the", "a", "an", "my", "our", "their", "that", "this", "any", "explicit", "owner",
    "person", "user", "human", "someone", "somebody", "final", "formal",
];

/// The consent noun at word `i` (family B), and its length in words.
fn consent_at(unit: &Unit, i: usize) -> Option<usize> {
    let prev = unit.before(i, 1);
    let next = unit.word(i + 1);
    match unit.word(i) {
        "go-ahead" | "goahead" | "say-so" | "sign-off" | "signoff" | "thumbs-up" | "greenlight"
        | "approval" | "confirmation" | "authorization" | "authorisation" | "clearance" => Some(1),
        "go" if next == "ahead" && DETERMINERS.contains(&prev) => Some(2),
        "green" if next == "light" => Some(2),
        "thumbs" if next == "up" => Some(2),
        "permission" if matches!(prev, "your" | "owner" | "person" | "user") && next == "to" => {
            Some(1)
        }
        "ok" | "okay" if matches!(prev, "the" | "an" | "your") => Some(1),
        "nod" if matches!(prev, "the" | "a" | "your") => Some(1),
        "word" if prev == "the" => Some(1),
        "yes" if prev == "a" => Some(1),
        _ => None,
    }
}

/// Family B: a consent noun with a pending marker near it, or any consent noun in a need (its
/// head is the marker). A wait for room after the consent noun waits on no person (`per the
/// go-ahead I'm waiting for room first`); one before it may (`waiting for memory and for your
/// go-ahead`).
fn consent(unit: &Unit) -> bool {
    (0..unit.len()).any(|i| {
        consent_at(unit, i).is_some_and(|n| {
            unit.need
                || (i.saturating_sub(6)..(i + n + 3).min(unit.len()))
                    .any(|m| pending(unit, m) && !(m > i && waits_for_room(unit, m)))
        })
    })
}

/// Whether the wait at word `i` waits for memory, disk or room (`waiting for room`, `waiting on
/// the disk`).
fn waits_for_room(unit: &Unit, i: usize) -> bool {
    if !matches!(unit.word(i + 1), "for" | "on") {
        return false;
    }
    let at = i
        + 2
        + usize::from(matches!(
            unit.word(i + 2),
            "the" | "more" | "free" | "enough" | "some"
        ));
    ROOM_WORDS.contains(&unit.word(at))
}

/// Whether word `i` marks a wait (`waiting for`, `until`, `needs your`, `only with`, `so I can`):
/// a wait or a need a negation undoes (`don't have to wait`, `no need to wait`) marks none.
fn pending(unit: &Unit, i: usize) -> bool {
    let prev = unit.before(i, 1);
    if matches!(prev, "not" | "no" | "never" | "longer" | "without") || prev.ends_with("n't") {
        return false;
    }
    let next = unit.word(i + 1);
    let w = unit.word(i);
    // A wait in the past tense is a report (`the rebuild that was waiting for your go-ahead is
    // done`).
    let past = matches!(prev, "was" | "were")
        || (prev == "been" && (unit.before(i, 2) == "had" || unit.before(i, 2).ends_with("'d")));
    match w {
        w if w.starts_with("wait") || w.starts_with("await") => !unit.undone(i) && !past,
        "until" | "till" | "unless" | "pending" | "before" => true,
        "need" | "needs" | "needed" | "require" | "requires" => {
            !unit.undone(i)
                && [next, unit.word(i + 2)]
                    .iter()
                    .any(|n| matches!(*n, "your" | "the" | "a" | "an" | "owner" | "person"))
        }
        "only" => matches!(
            next,
            "with" | "after" | "on" | "once" | "when" | "if" | "upon"
        ),
        "without" => (0..i).any(|b| {
            matches!(
                unit.word(b),
                "won't" | "can't" | "cannot" | "don't" | "shouldn't"
            )
        }),
        "so" => unit.at(i + 1, &["i", "can"]) || unit.at(i + 1, &["that", "i", "can"]),
        "as" => unit.at(i + 1, &["soon", "as", "i", "get"]),
        "once" | "when" => unit.at(i + 1, &["i", "get"]),
        _ => false,
    }
}

/// The words of the people a [`SAYS`] verb's subject names (`you'll need to say`).
const PEOPLE: &[&str] = &[
    "you", "you'll", "you'd", "owner", "person", "someone", "somebody", "user", "they", "human",
];

/// The words that make a [`SAYS`] verb someone else's (`the tests say "ok"`).
const NOT_PEOPLE: &[&str] = &[
    "i", "i'll", "i'd", "i'm", "we", "we'll", "it", "he", "she", "harness", "note", "notice",
    "prompt", "test", "tests", "check", "checks", "doc", "docs", "rule", "rules", "which", "who",
    "the", "a", "an", "my", "your", "this", "that", "its", "our", "their",
];

/// The first words that make what follows a [`SAYS`] verb its object or a question, not a word
/// to say (`answer the security prompt`, `say whether it should`).
const NOT_TOKENS: &[&str] = &[
    "the", "a", "an", "your", "my", "this", "that", "its", "our", "their", "these", "those",
    "whether", "what", "which", "how", "why", "who",
];

/// Family C's person-says: a [`SAYS`] verb (or `tell me`) the person is to use, then an optional
/// `:` or `with`, then the word to say, quoted (inline code is a quote here) or bare (in a need,
/// or in prose after `just`, `please` or `simply`, or opening it); a bare word is a word to say,
/// never an object (`send the report`). A verb after a person's own act (`click Allow, then say
/// "go"`) asks no word: the act comes first, unless it is a wait for room. Its verb's word index
/// and the word as written.
fn person_says(unit: &Unit) -> Option<(usize, Option<String>)> {
    (0..unit.len()).find_map(|i| {
        let w = unit.word(i);
        let tell = w == "tell" && unit.word(i + 1) == "me";
        if !(SAYS.contains(&w) || tell) || !says_lead(unit, i) || acts_first(unit, i) {
            return None;
        }
        let (start, word) = &unit.words[i + usize::from(tell)];
        let rest = &unit.lower[start + word.len()..];
        let trimmed = rest.trim_start().trim_start_matches(':').trim_start();
        let trimmed = trimmed
            .strip_prefix("with ")
            .unwrap_or(trimmed)
            .trim_start();
        let from = unit.lower.len() - trimmed.len();
        if let Some(inner) = trimmed.strip_prefix('"') {
            let end = inner.find('"')?;
            let token = inner[..end].trim();
            return (!token.is_empty()).then(|| (i, Some(unit.said_at(from + 1, from + 1 + end))));
        }
        let bare = unit.need || i == 0 || matches!(unit.before(i, 1), "just" | "please" | "simply");
        if !bare || tell {
            return tell.then_some((i, None));
        }
        if matches!(w, "send" | "write")
            || unit
                .words
                .iter()
                .find(|(at, _)| *at >= from)
                .is_none_or(|(_, first)| NOT_TOKENS.contains(&first.as_str()))
        {
            return None;
        }
        let mut end = from;
        let mut n = 0;
        for (at, word) in unit.words.iter().skip(i + 1) {
            if *at < from {
                continue;
            }
            if matches!(
                word.as_str(),
                "so" | "to" | "and" | "when" | "once" | "then" | "if" | "after" | "before" | "or"
            ) || unit.lower[end..*at].contains([',', '.', ';', ':', '('])
                || n == 8
            {
                break;
            }
            end = at + word.len();
            n += 1;
        }
        (n > 0).then(|| (i, Some(unit.said_at(from, end))))
    })
}

/// Whether the [`SAYS`] verb at `i`, after `then` or `and`, follows a clause of the person's own
/// act (`click Allow on the dialog, then say "go"`) that neither says a word itself nor waits for
/// room (`free disk room or let the other builds finish, then say …`).
fn acts_first(unit: &Unit, i: usize) -> bool {
    if !matches!(unit.before(i, 1), "then" | "and") {
        return false;
    }
    let Some((at, _)) = i.checked_sub(1).and_then(|k| unit.words.get(k)) else {
        return false;
    };
    let head = unit.lower[..*at].trim_end().trim_end_matches(',');
    let clause = Unit::new(
        &head[head.rfind([',', ';', ':', '(']).map_or(0, |k| k + 1)..],
        false,
    );
    clause.len() > 0
        && !SAYS.contains(&clause.word(0))
        && !clause.any(ROOM_NOUNS)
        && !clause.any(ROOM_VERBS)
}

/// Whether the [`SAYS`] verb at `i` is the person's: it opens the unit, follows `then`,
/// `just`, `please`, `and`, `or` or a person, or follows `to`/`can` after a person
/// (`you'll need to say`).
fn says_lead(unit: &Unit, i: usize) -> bool {
    let prev = unit.before(i, 1);
    if i == 0 || matches!(prev, "then" | "just" | "please" | "simply" | "and" | "or") {
        return true;
    }
    if PEOPLE.contains(&prev) {
        return true;
    }
    if NOT_PEOPLE.contains(&prev) {
        return false;
    }
    matches!(prev, "to" | "can" | "could" | "would" | "will" | "only")
        && (2..=4).any(|b| PEOPLE.contains(&unit.before(i, b)))
}

/// Family C's fixed phrases (`say the word`, `only when asked`, `without being asked` with a
/// negation).
fn fixed_ask(unit: &Unit) -> bool {
    const ASKS: &[&[&str]] = &[
        &["say", "the", "word"],
        &["give", "the", "word"],
        &["give", "me", "the", "word"],
        &["tell", "me", "to"],
        &["ask", "me", "to"],
        &["only", "when", "asked"],
        &["only", "if", "asked"],
        &["until", "asked"],
        &["unless", "asked"],
        &["until", "told"],
        &["unless", "told"],
    ];
    ASKS.iter().any(|p| unit.has(p))
        || (unit.negated()
            && (unit.has(&["without", "being", "asked"])
                || unit.has(&["without", "being", "told"])))
}

const SLOT_WHEN: &[&[&str]] = &[
    &["until"],
    &["unless"],
    &["till"],
    &["once"],
    &["before"],
    &["when"],
    &["if"],
    &["as", "soon", "as"],
    &["after"],
    &["the", "moment"],
    &["only", "when"],
    &["only", "if"],
];

const SLOT_WHO: &[&[&str]] = &[
    &["you"],
    &["someone"],
    &["somebody"],
    &["a", "person"],
    &["the", "person"],
    &["the", "owner"],
    &["the", "user"],
    &["a", "human"],
    &["they"],
];

const SLOT_ADVERBS: &[&str] = &["explicitly", "actually", "specifically"];

const SLOT_SAYS: &[&str] = &[
    "ask", "asks", "say", "says", "tell", "tells", "confirm", "confirms", "approve", "approves",
    "give", "gives", "ok", "okay", "oks", "okays",
];

/// The length of `phrases`' phrase at word `i`, if one is there.
fn slot(unit: &Unit, i: usize, phrases: &[&[&str]]) -> Option<usize> {
    phrases.iter().find(|p| unit.at(i, p)).map(|p| p.len())
}

fn skip_adverb(unit: &Unit, i: usize) -> usize {
    i + usize::from(SLOT_ADVERBS.contains(&unit.word(i)))
}

/// Family C's three slots (`until a person asks`, `unless you explicitly say`), its wait
/// (`waiting for you to ask`) and its passive (`until I'm asked`).
fn slotted(unit: &Unit) -> bool {
    (0..unit.len()).any(|i| {
        let w = unit.word(i);
        if (w.starts_with("wait") || w.starts_with("await"))
            && unit.word(i + 1) == "for"
            && !unit.undone(i)
            && let Some(n) = slot(unit, i + 2, SLOT_WHO)
            && unit.word(i + 2 + n) == "to"
            && SLOT_SAYS.contains(&unit.word(i + 3 + n))
        {
            return true;
        }
        let Some(when) = slot(unit, i, SLOT_WHEN) else {
            return false;
        };
        let at = skip_adverb(unit, i + when);
        if let Some(who) = slot(unit, at, SLOT_WHO) {
            let at = skip_adverb(unit, at + who);
            if SLOT_SAYS.contains(&unit.word(at)) {
                return true;
            }
        }
        let passive = matches!(unit.word(i), "until" | "unless")
            || unit.at(i, &["only", "when"])
            || unit.at(i, &["only", "if"]);
        let at = i + if unit.word(i) == "only" { 2 } else { 1 };
        passive
            && (unit.word(at) == "i'm" || unit.at(at, &["i", "am"]) || unit.at(at, &["i", "get"]))
            && {
                let after = at + if unit.word(at) == "i'm" { 1 } else { 2 };
                matches!(unit.word(after), "asked" | "told")
            }
    })
}

/// Family D: `on my own`, `by myself` or `myself` with a negation (`I won't restart it on my
/// own`).
fn on_my_own(unit: &Unit) -> bool {
    unit.negated()
        && (unit.has(&["on", "my", "own"]) || unit.has(&["by", "myself"]) || unit.any(&["myself"]))
}

/// Family D's stop note: a stop note or notice and a negated restart (`Per the stop notice, I'm
/// not restarting either.`).
fn stop_note(unit: &Unit) -> bool {
    (unit.has(&["stop", "note"]) || unit.has(&["stop", "notice"]))
        && unit.negated()
        && restarts(unit)
}

fn restarts(unit: &Unit) -> bool {
    (0..unit.len()).any(|i| {
        inflects(unit.word(i), "restart")
            || (inflects(unit.word(i), "start") && (i + 1..i + 5).any(|k| unit.word(k) == "again"))
    })
}

/// The agent's own reversible work: verbs in any inflection.
const WORK: &[&str] = &[
    "build", "rebuild", "compile", "test", "retest", "rerun", "re-run", "check", "verify", "retry",
    "resume", "finish", "continue", "commit", "land",
];

const WORK_IRREGULAR: &[&str] = &["built", "rebuilt", "reran", "re-ran"];

/// What a restart restarts when the unit or the one before names it (`the check run … I won't
/// restart it`).
const RESTARTED: &[&str] = &[
    "build", "rebuild", "compile", "test", "check", "verify", "run",
];

/// Whether `unit` names own reversible work from the allow list: a [`WORK`] verb, a merge of
/// main or a branch, a push (a tag, a release or upstream is an exclusion's), a rebase onto
/// main, its own cleanup, or a restart of a stopped build (`stop_note`, or a [`RESTARTED`] word
/// in it or `before`).
fn works(unit: &Unit, before: Option<&Unit>, stop_note: bool) -> bool {
    let names = |u: &Unit, bases: &[&str]| u.words.iter().any(|(_, w)| inflects_any(w, bases));
    if names(unit, WORK) || unit.any(WORK_IRREGULAR) || !own_cleanup(unit).is_empty() {
        return true;
    }
    let mains = unit.any(&["main", "branch"]);
    if (0..unit.len()).any(|i| {
        let w = unit.word(i);
        (inflects(w, "merge") && mains)
            || (inflects(w, "rebase") && unit.any(&["main"]))
            || inflects(w, "push")
    }) {
        return true;
    }
    restarts(unit)
        && (stop_note || names(unit, RESTARTED) || before.is_some_and(|b| names(b, RESTARTED)))
}

/// The branches the agent's own cleanup deletes (`delete my merged branch`).
const OWN_BRANCHES: &[&[&str]] = &[
    &["my", "merged", "branch"],
    &["the", "merged", "branch"],
    &["my", "local", "branch"],
    &["the", "local", "branch"],
    &["merged", "branch"],
    &["local", "branch"],
];

/// The word indexes of the agent's own cleanup verbs (`remove the worktree`, `delete my merged
/// branch`), which no outward word reads; a worktree or branch someone else owns (`remove your
/// old worktree`, `the worktree you made`) is no cleanup of the agent's.
fn own_cleanup(unit: &Unit) -> Vec<usize> {
    (0..unit.len())
        .filter(|&i| {
            let noun = (i + 1..i + 4)
                .find(|&k| unit.word(k) == "worktree")
                .or_else(|| {
                    OWN_BRANCHES
                        .iter()
                        .find(|p| unit.at(i + 1, p))
                        .map(|p| i + p.len())
                });
            inflects_any(unit.word(i), &["remove", "delete", "drop", "clean"])
                && noun.is_some_and(|n| !owned_after(unit, n))
                && !(i + 1..i + 4).any(|k| foreign(unit, k))
        })
        .collect()
}

/// Whether an exclusion word at `i` counts: one right after `no`, `never`, `won't`,
/// `without`, `nothing` or `not` does not (`no force-push`).
fn counts(unit: &Unit, i: usize) -> bool {
    !matches!(
        unit.before(i, 1),
        "no" | "never" | "won't" | "without" | "nothing" | "not"
    )
}

/// Whether an exclusion fires on `unit`, or on the code it carries: a command it names is read
/// for what it would do (`git push --force`, `gh auth login`).
fn excluded(unit: &Unit, says_at: Option<usize>) -> bool {
    let code = Unit::new(&unit.code, false);
    (kept(Part::PersonOnly) && (person_only(unit) || person_only(&code)))
        || (kept(Part::PersonFirst) && person_first(unit))
        || (kept(Part::Outward) && (outward(unit, says_at) || outward(&code, None)))
        || (kept(Part::SomeoneElse) && someone_else(unit))
        || (kept(Part::Declined) && declined(unit))
        || (kept(Part::Chooses) && chooses(unit))
        || (kept(Part::Attributed) && attributed(unit))
}

/// A hold the person gave, attributed to them (`as you asked`, `you told me`, `per your
/// instruction`, `you stopped the build`): a go-ahead never lifts their own word.
fn attributed(unit: &Unit) -> bool {
    const PHRASES: &[&[&str]] = &[
        &["as", "you", "asked"],
        &["you", "asked", "me"],
        &["as", "you", "said"],
        &["you", "said"],
        &["you", "told", "me"],
        &["as", "you", "told"],
        &["you", "stopped"],
        &["you", "wanted"],
        &["per", "your"],
        &["at", "your", "request"],
        &["as", "requested"],
        &["as", "instructed"],
        &["your", "instruction"],
        &["your", "instructions"],
    ];
    PHRASES.iter().any(|p| unit.has(p))
}

/// What only a person can give: a credential, a secret or sign-in, physical access, a Mac
/// restart, an OS grant or its dialog, access, money (credits, a budget, a quota), customers,
/// mail, production.
fn person_only(unit: &Unit) -> bool {
    const WORDS: &[&str] = &[
        "credential",
        "secret",
        "dialog",
        "automation",
        "camera",
        "microphone",
        "credit",
        "budget",
        "quota",
        "billable",
        "password",
        "passphrase",
        "passcode",
        "keychain",
        "login",
        "signin",
        "re-login",
        "relogin",
        "2fa",
        "mfa",
        "otp",
        "captcha",
        "oauth",
        "token",
        "pat",
        "physical",
        "physically",
        "cable",
        "usb",
        "yubikey",
        "accessibility",
        "privacy",
        "tcc",
        "sudo",
        "administrator",
        "admin",
        "admins",
        "access",
        "collaborator",
        "role",
        "roles",
        "scopes",
        "billing",
        "invoice",
        "subscription",
        "license",
        "licence",
        "customer",
        "email",
        "production",
        "prod",
        "certificate",
        "bought",
        "paid",
        "spent",
        "cost",
    ];
    const INFLECTED: &[&str] = &[
        "authenticate",
        "click",
        "grant",
        "plug",
        "reboot",
        "pay",
        "purchase",
        "buy",
        "spend",
        "announce",
    ];
    const PHRASES: &[&[&str]] = &[
        &["api", "key"],
        &["ssh", "key"],
        &["signing", "key"],
        &["deploy", "key"],
        &["private", "key"],
        &["log", "in"],
        &["logs", "in"],
        &["logged", "in"],
        &["one-time", "code"],
        &["verification", "code"],
        &["security", "key"],
        &["touch", "id"],
        &["face", "id"],
        &["restart", "the", "mac"],
        &["restart", "your", "mac"],
        &["restart", "this", "mac"],
        &["power", "cycle"],
        &["full", "disk", "access"],
        &["screen", "recording"],
        &["system", "settings"],
        &["permission", "denied"],
        &["not", "permitted"],
        &["credit", "card"],
    ];
    if unit.lower.contains('$') || unit.lower.contains(".env") {
        return true;
    }
    (0..unit.len()).any(|i| {
        let w = unit.word(i);
        let plural = w.strip_suffix('s').unwrap_or(w);
        let hit = WORDS.contains(&w)
            || WORDS.contains(&plural)
            || inflects_any(w, INFLECTED)
            || PHRASES.iter().any(|p| unit.at(i, p))
            || (matches!(w, "sign" | "signs") && (i + 1..i + 5).any(|k| unit.word(k) == "in"))
            || (matches!(w, "permission" | "permissions") && consent_at(unit, i).is_none());
        hit && counts(unit, i)
    })
}

/// A precondition only the person can meet: `after`, `once`, `when` or `if` a person signs,
/// logs in, reviews, reads, approves, decides…
fn person_first(unit: &Unit) -> bool {
    const ACTS: &[&str] = &[
        "sign", "log", "enter", "review", "read", "look", "accept", "approve", "grant", "plug",
        "confirm", "decide", "choose", "pick",
    ];
    const WHEN: &[&[&str]] = &[
        &["after"],
        &["once"],
        &["when"],
        &["if"],
        &["as", "soon", "as"],
    ];
    const WHO: &[&[&str]] = &[
        &["you"],
        &["you've"],
        &["you'll"],
        &["the", "owner"],
        &["a", "person"],
        &["the", "person"],
        &["someone"],
        &["somebody"],
        &["the", "user"],
        &["they"],
        &["they've"],
    ];
    (0..unit.len()).any(|i| {
        let Some(when) = slot(unit, i, WHEN) else {
            return false;
        };
        let Some(who) = slot(unit, i + when, WHO) else {
            return false;
        };
        let mut at = skip_adverb(unit, i + when + who);
        if matches!(unit.word(at), "have" | "has") {
            at += 1;
        }
        let act = unit.word(at);
        inflects_any(act, ACTS) || matches!(act, "chose" | "chosen" | "picked")
    })
}

/// Git's forms that destroy work no [`DESTRUCTIVE_WORDS`] word names, read on the lowercased
/// text (the words drop a flag's dashes).
const GIT_DESTROYS: &[&str] = &[
    "clean -f",
    "clean -x",
    "clean -d",
    "checkout -- ",
    "checkout .",
    "restore .",
    "restore --",
    "branch -d",
    "filter-repo",
    "filter-branch",
    "stash clear",
    "update-ref -d",
    "reflog expire",
];

/// Outward or irreversible work: [`DESTRUCTIVE_WORDS`], [`GIT_DESTROYS`] and what leaves the
/// machine or ends a process; the allow list's own cleanup is the only exception.
fn outward(unit: &Unit, says_at: Option<usize>) -> bool {
    const INFLECTED: &[&str] = &[
        "promote",
        "tag",
        "ship",
        "upload",
        "send",
        "post",
        "notify",
        "rotate",
        "revoke",
        "retire",
        "deprecate",
        "archive",
        "yank",
        "unpublish",
        "launch",
        "notarize",
        "codesign",
        "sign",
        "prune",
        "deinit",
        "amend",
        "squash",
        "kill",
        "pkill",
        "signal",
        "terminate",
        "quit",
    ];
    const WORDS: &[&str] = &[
        "sent",
        "gc",
        "gh-pages",
        "website",
        "hosting",
        "domain",
        "dns",
        "terraform",
        "public",
        "registry",
        "registries",
        "pypi",
        "rubygems",
        "ghcr",
        "dockerhub",
    ];
    const PREFIXES: &[&str] = &["deploy", "publish", "release"];
    const PHRASES: &[&[&str]] = &[
        &["message", "the"],
        &["message", "them"],
        &["comment", "on"],
        &["open", "a", "pr"],
        &["open", "a", "pull", "request"],
        &["open", "an", "issue"],
        &["file", "an", "issue"],
        &["rename", "the", "repo"],
        &["rename", "a", "repo"],
        &["go", "live"],
        &["close", "the", "tab"],
        &["close", "a", "tab"],
        &["close", "this", "tab"],
        &["close", "the", "window"],
        &["restart", "aterm"],
        &["relaunch", "aterm"],
        &["apply", "the", "plan"],
        &["host", "it"],
        &["host", "the"],
    ];
    if unit.lower.contains("push -f")
        || unit.lower.contains("push --force")
        || unit.lower.contains("/applications")
        || GIT_DESTROYS.iter().any(|g| unit.lower.contains(g))
    {
        return true;
    }
    let cleanup = own_cleanup(unit);
    // A report of what was done (`the build was stopped`), never one to come (`can be deleted`,
    // `will be purged`).
    let passive = |i: usize| {
        let reports = |w: &str| matches!(w, "was" | "were" | "been" | "got");
        reports(unit.before(i, 1)) || (unit.before(i, 1) == "already" && reports(unit.before(i, 2)))
    };
    (0..unit.len()).any(|i| {
        if cleanup.contains(&i) || says_at == Some(i) || !counts(unit, i) || passive(i) {
            return false;
        }
        let w = unit.word(i);
        let destructive = DESTRUCTIVE_WORDS.iter().any(|d| {
            let want: Vec<&str> = d.split(' ').map(|p| p.trim_start_matches('-')).collect();
            if want == ["push", "f"] {
                return false;
            }
            if want == ["drop"] {
                return inflects(w, "drop") && dropped_object(unit.word(i + 1));
            }
            want.iter()
                .enumerate()
                .all(|(k, base)| inflects(unit.word(i + k), base))
        });
        destructive
            || inflects_any(w, INFLECTED)
            || WORDS.contains(&w)
            || PREFIXES.iter().any(|p| w.starts_with(p))
            || PHRASES.iter().any(|p| unit.at(i, p))
            || (inflects(w, "push") && pushes_elsewhere(unit, i))
    })
}

/// Whether the push at word `i` names a destination that is not the agent's branch or main:
/// `to <X>` in its clause within six words, X (past a determiner) not main, master, origin, the
/// remote, `it`, nor a branch or worktree. `push 1.4.0 to crates.io` and `push the image to the
/// registry` publish; a bare push (`I'll push as soon as I get the go-ahead`) and `push it to
/// main` do not.
fn pushes_elsewhere(unit: &Unit, i: usize) -> bool {
    let Some((from, _)) = unit.words.get(i) else {
        return false;
    };
    let Some(to) = (i + 1..(i + 7).min(unit.len())).find(|&k| {
        let (at, _) = &unit.words[k];
        unit.word(k) == "to" && !unit.lower[*from..*at].contains([',', ';', ':', '('])
    }) else {
        return false;
    };
    if (i + 1..to).any(|k| {
        matches!(
            unit.word(k),
            "and" | "or" | "then" | "so" | "once" | "when" | "if" | "before" | "after" | "as"
        )
    }) {
        return false;
    }
    let at = (to + 1..unit.len())
        .find(|&k| !matches!(unit.word(k), "the" | "my" | "our" | "its" | "this" | "that"))
        .unwrap_or(unit.len());
    let own = matches!(
        unit.word(at),
        "main" | "master" | "origin" | "remote" | "it" | "branch" | "branches" | ""
    ) || (at..at + 4).any(|k| matches!(unit.word(k), "branch" | "worktree"));
    !own
}

/// Whether a `drop` takes `next` as its object (`drop the table`), not a fall (`memory dropped
/// below`).
fn dropped_object(next: &str) -> bool {
    matches!(
        next,
        "the"
            | "a"
            | "an"
            | "this"
            | "that"
            | "these"
            | "those"
            | "my"
            | "your"
            | "its"
            | "all"
            | "it"
            | "them"
            | "table"
            | "tables"
            | "database"
            | "column"
            | "stash"
            | "commit"
            | "commits"
            | "branch"
    )
}

/// The words that make a clause naming another session's work a wait for room, not their work
/// (`disk room, which other sessions' build caches are holding`).
const ROOM_NOUNS: &[&str] = &[
    "disk", "memory", "room", "space", "swap", "cache", "caches", "cores", "ram", "floor",
];

/// The verbs that make such a clause a wait for room only after the other session's words
/// (`let the other sessions' builds finish`), not before them, where they are the agent's own
/// work on the other session's (`finish the other session's merge`).
const ROOM_VERBS: &[&str] = &["finish", "finishes", "clear", "free", "frees"];

/// The work a possessor other than the agent can own (`your feature branch`, `Alice's PR`).
const WORK_NOUNS: &[&str] = &[
    "pr",
    "prs",
    "branch",
    "branches",
    "worktree",
    "worktrees",
    "change",
    "changes",
    "commit",
    "commits",
    "patch",
    "patches",
    "work",
    "fork",
    "checkout",
    "stash",
    "repo",
    "repository",
    "diff",
    "edits",
];

/// The people, and the other agents, sessions and tabs, whose possessive names their work (`the
/// owner's branch`, `the other agent's branch`).
const PEOPLE_NOUNS: &[&str] = &[
    "owner",
    "person",
    "user",
    "worker",
    "colleague",
    "teammate",
    "maintainer",
    "contributor",
    "reviewer",
    "author",
    "human",
    "agent",
    "session",
    "tab",
];

/// The capitalised words whose possessive names no person (`Main's changes`).
const NOT_POSSESSORS: &[&str] = &[
    "main", "origin", "it", "that", "there", "here", "what", "who", "today", "aterm", "git",
];

/// Whether word `k` names a possessor other than the agent: `your`, `their`, `his`, `her`, or
/// a person's possessive (`the owner's`, `Alice's`).
fn foreign(unit: &Unit, k: usize) -> bool {
    let Some((at, w)) = unit.words.get(k) else {
        return false;
    };
    if matches!(w.as_str(), "your" | "their" | "his" | "her") {
        return true;
    }
    let possessive = unit.lower[at + w.len()..].starts_with("'s")
        || (w.ends_with('s') && unit.lower[at + w.len()..].starts_with('\''));
    let named = unit
        .said_at(*at, at + w.len())
        .chars()
        .next()
        .is_some_and(char::is_uppercase)
        && !NOT_POSSESSORS.contains(&w.as_str());
    possessive && (named || PEOPLE_NOUNS.contains(&w.trim_end_matches('s')))
}

/// The subjects that make a clause right after a work noun its owner's (`the worktree you
/// made`, `the branch the other agent pushed`).
const OWNERS_AFTER: &[&[&str]] = &[
    &["you"],
    &["you've"],
    &["you'd"],
    &["you're"],
    &["they"],
    &["they've"],
    &["someone"],
    &["somebody"],
    &["the", "owner"],
    &["the", "person"],
    &["the", "user"],
    &["another", "session"],
    &["another", "agent"],
    &["another", "tab"],
    &["the", "other", "session"],
    &["the", "other", "agent"],
    &["the", "other", "tab"],
    &["the", "other", "sessions"],
    &["the", "other", "agents"],
    &["other", "sessions"],
    &["other", "agents"],
];

/// Whether the work noun at word `n` is followed by a clause whose subject is someone else and
/// a verb (`the worktree you made`, `the PR Alice opened`, `the branch that the other agent
/// pushed`): the work is theirs. The subject stands right after the noun, or after `that`,
/// `which` or `whom`, with no mark between (`remove the worktree. The build …` names no owner);
/// a capitalised name counts, `I` and the words [`NOT_POSSESSORS`] names do not.
fn owned_after(unit: &Unit, n: usize) -> bool {
    let at = n + 1 + usize::from(matches!(unit.word(n + 1), "that" | "which" | "whom"));
    let (Some((noun, w)), Some((subject, _))) = (unit.words.get(n), unit.words.get(at)) else {
        return false;
    };
    if unit.lower[noun + w.len()..*subject]
        .chars()
        .any(|c| !c.is_alphanumeric() && !c.is_whitespace())
    {
        return false;
    }
    let named = || {
        let who = unit.word(at);
        let capital = unit
            .said_at(*subject, subject + who.len())
            .chars()
            .next()
            .is_some_and(char::is_uppercase);
        let me = who == "i" || who.starts_with("i'");
        (capital && !me && !NOT_POSSESSORS.contains(&who)).then_some(1)
    };
    let Some(len) = slot(unit, at, OWNERS_AFTER).or_else(named) else {
        return false;
    };
    let verb = unit.word(at + len);
    !verb.is_empty()
        && !matches!(
            verb,
            "and" | "or" | "to" | "so" | "once" | "when" | "if" | "then" | "say" | "says"
        )
}

/// Whether a work noun has a foreign possessor ([`foreign`]) before it, at most two words
/// between them and none a consent noun or a word that starts another phrase (`your go-ahead to
/// merge the branch` is the agent's branch), or an owner named after it ([`owned_after`]).
fn possessed(unit: &Unit) -> bool {
    const BETWEEN: &[&str] = &[
        "to", "on", "for", "of", "with", "my", "the", "a", "an", "this", "that", "its", "and",
        "or", "in", "at", "into", "before", "after",
    ];
    (0..unit.len()).any(|i| {
        WORK_NOUNS.contains(&unit.word(i))
            && (owned_after(unit, i)
                || (1..=3).any(|back| {
                    i.checked_sub(back).is_some_and(|k| {
                        foreign(unit, k)
                            && (k + 1..i).all(|j| {
                                !BETWEEN.contains(&unit.word(j)) && consent_at(unit, j).is_none()
                            })
                    })
                }))
    })
}

/// Someone else's work: another session's or agent's, the owner's, the person's (`your
/// branch`), a named person's, upstream's, a worker's; skipped in a clause that waits on their
/// use of room ([`ROOM_NOUNS`], or a [`ROOM_VERBS`] verb after the other session's words).
fn someone_else(unit: &Unit) -> bool {
    const WORDS: &[&str] = &[
        "upstream",
        "maintainer",
        "maintainers",
        "contributor",
        "contributors",
        "worker",
        "workers",
    ];
    if possessed(unit) {
        return true;
    }
    const PHRASES: &[&[&str]] = &[
        &["another", "session"],
        &["other", "session"],
        &["other", "sessions"],
        &["another", "agent"],
        &["other", "agent"],
        &["other", "agents"],
        &["someone", "else"],
        &["somebody", "else"],
        &["belongs", "to"],
        &["belong", "to"],
        &["not", "mine"],
        &["isn't", "mine"],
        &["a", "worker"],
    ];
    unit.lower
        .split([',', ';', '(', ')', '\u{2014}', ':'])
        .any(|clause| {
            let c = Unit::new(clause, false);
            let others = (0..c.len()).find(|&i| {
                matches!(
                    c.word(i),
                    "session" | "sessions" | "agent" | "agents" | "tab" | "tabs"
                ) || session_id(c.word(i))
            });
            let waits =
                others.is_some_and(|k| (k..c.len()).any(|i| ROOM_VERBS.contains(&c.word(i))));
            if c.any(ROOM_NOUNS) || waits {
                return false;
            }
            let owned = |who: &str| {
                (0..c.len()).any(|i| {
                    c.word(i) == who && consent_at(&c, i + 1).is_none() && c.word(i + 1) != "go"
                })
            };
            c.any(WORDS)
                || PHRASES.iter().any(|p| c.has(p))
                || owned("their")
                || (clause.contains("owner's") && owned("owner"))
                || clause.contains("worker's")
                || c.words.iter().any(|(_, w)| session_id(w))
        })
}

fn session_id(word: &str) -> bool {
    word.strip_prefix("s-")
        .is_some_and(|hex| hex.len() >= 6 && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// A decline, a rejection or a deadlock: a go-ahead does not lift those.
fn declined(unit: &Unit) -> bool {
    const WORDS: &[&str] = &[
        "syspolicyd",
        "stuck",
        "deadlock",
        "deadlocked",
        "declined",
        "rejected",
        "denied",
        "hook",
        "hooks",
    ];
    unit.any(WORDS)
        || unit.has(&["not", "the", "user"])
        || unit.has(&["doesn't", "want", "to", "proceed"])
        || unit.has(&["does", "not", "want", "to", "proceed"])
}

/// A choice for the person (`which one`, `either … or …`, a question with ` or `), unless every
/// alternative is go on or wait.
fn chooses(unit: &Unit) -> bool {
    let question = unit.lower.trim_end().ends_with('?');
    let which = (0..unit.len()).any(|i| {
        unit.word(i) == "which"
            && (question
                || matches!(
                    unit.word(i + 1),
                    "one" | "option" | "options" | "would" | "do" | "should" | "of"
                ))
    });
    let or = unit.lower.rfind(" or ");
    let either = or.is_some() && (unit.any(&["either"]) || question);
    let waits = or.is_some_and(|at| {
        let after = unit.lower[at + 4..].trim_start();
        ["wait", "hold", "stop", "not ", "keep waiting", "leave it"]
            .iter()
            .any(|w| after.starts_with(w))
    });
    (which || either) && !waits
}

/// Whether `unit` echoes the answer (`consider it said`, `the standing go-ahead`, `per the
/// go-ahead`) with no negation outside the echo: an agent acting on it parks nothing, and one
/// that argues against it (`the harness's "consider it said" is not the person`) still parks.
fn echoes(unit: &Unit) -> bool {
    const PHRASES: &[&[&str]] = &[
        &["per", "the", "go-ahead"],
        &["as", "the", "go-ahead"],
        &["the", "go-ahead", "says"],
        &["consider", "it", "said"],
        &["standing", "go-ahead"],
        &["go-ahead", "stands"],
        &["that", "ask"],
        &["got", "the", "go-ahead"],
        &["have", "the", "go-ahead"],
        &["had", "the", "go-ahead"],
        &["given", "the", "go-ahead"],
        &["gave", "the", "go-ahead"],
        &["got", "your", "go-ahead"],
        &["have", "your", "go-ahead"],
        &["proceed", "now", "with", "your", "own", "work"],
        &["never", "a", "needs", "you", "item"],
        &["how", "dare", "you", "block", "on", "me"],
    ];
    if unit.lower.contains("[aterm harness]") {
        return !unit.negated();
    }
    (0..unit.len()).any(|i| {
        PHRASES
            .iter()
            .any(|p| unit.at(i, p) && !unit.negated_outside(i..i + p.len()))
    })
}

/// The words of memory, disk and room.
const ROOM_WORDS: &[&str] = &[
    "memory",
    "low-memory",
    "disk",
    "room",
    "swap",
    "space",
    "free-space",
    "floor",
    "ram",
];

/// Whether `unit` names memory, disk or room: the answer says to wait for room first.
fn names_room(unit: &Unit) -> bool {
    unit.any(ROOM_WORDS)
}

#[cfg(test)]
#[path = "go_ahead_tests.rs"]
mod tests;
