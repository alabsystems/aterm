// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl --host any spawn agent=<claude|codex> [need=…] -`: the fleet Mac a worker goes to,
//! picked from one survey of every member by what it needs of that Mac (a checkout, free disk,
//! a home that resolves `cwd=~/…`: [`Need`]) and the accounts it can run the worker on.

use super::survey::{self, Fields, PeerSurvey};

/// The word `--host` takes for whichever member can take a worker.
pub(crate) const ANY: &str = "any";

/// The programs whose account rows a pick weighs: an `agent=` word and the `pool=` of the rows
/// it runs on.
pub(crate) const PROGRAMS: [&str; 2] = ["claude", "codex"];

/// The conditions a row takes a session in (`usable`, `unseen` and `low`, as the harness's
/// `Condition::runs`).
const RUNS: [&str; 3] = [USABLE, "unseen", "low"];

/// The condition of a row read lately with room, from an aterm that says when it read it
/// (`read_at=`).
const USABLE: &str = "usable";

/// The condition of a row no use of which was read lately, or ever: a wall may stand on it, or its
/// login at a sign-in screen.
const UNSEEN: &str = "unseen";

/// The condition of a row a read showed nearly full (`Condition::Low`).
const LOW: &str = "low";

/// The condition of a row a wall stands on (`Condition::Limited`), until its `walled_until_ms`.
const LIMITED: &str = "limited";

/// The condition of a row whose login a read found ended (`Condition::SignedOut`): a sign-in there
/// mends it, not a wait.
const SIGNED_OUT: &str = "signed-out";

/// One account row of a member's survey, as a pick weighs it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Row {
    /// Its pool's program (`pool=`).
    pub(crate) program: String,
    /// Its label (`id=`).
    pub(crate) label: String,
    /// Its login counts as made there, as a launch there judges it (`enrolled=1`). A Claude row with
    /// no directory counts with no look at a login (`ATERM-KI-1105-9`).
    pub(crate) enrolled: bool,
    /// Its condition word (`condition=`).
    pub(crate) condition: String,
    /// The five-hour window used, in percent; `None` where no such window runs (it reset or was
    /// never read), and where the row's window has another length, full or not ([`used`]).
    pub(crate) five_hour: Option<u64>,
    /// The week used, in percent, as [`Self::five_hour`].
    pub(crate) seven_day: Option<u64>,
    /// When its seven-day window resets, in Unix milliseconds on the member's clock
    /// (`week_reset=`, in seconds).
    pub(crate) week_reset_ms: Option<u64>,
    /// When its wall ends, in Unix milliseconds on the member's clock (`walled_until=`, in
    /// seconds).
    pub(crate) walled_until_ms: Option<u64>,
    /// When its figures were last read, in Unix seconds on the member's clock (`read_at=`); `None`
    /// where it says `-`, or where its aterm is older than the field and says nothing.
    pub(crate) read_at: Option<u64>,
    /// Whether a launch on its member would start on it (`launch=1`, and any other word is no);
    /// `None` where its aterm is older than the field and says nothing.
    pub(crate) launch: Option<bool>,
}

impl Row {
    fn of(fields: &Fields) -> Self {
        Self {
            program: fields.get("pool").unwrap_or_default().to_owned(),
            label: fields.get("id").unwrap_or_default().to_owned(),
            enrolled: fields.get("enrolled") == Some("1"),
            condition: fields.get("condition").unwrap_or_default().to_owned(),
            five_hour: fields.number("pct5"),
            seven_day: fields.number("pct7"),
            week_reset_ms: survey::week_reset_ms(fields),
            walled_until_ms: survey::walled_until_ms(fields),
            read_at: fields.number("read_at"),
            launch: fields.get("launch").map(|word| word == "1"),
        }
    }

    /// Its seven-day figure at `at_ms`: `None` once its `week_reset=` has passed on the member's
    /// clock.
    pub(crate) fn seven_day_at(&self, at_ms: Option<u64>) -> Option<u64> {
        if self
            .week_reset_ms
            .zip(at_ms)
            .is_some_and(|(reset, at)| reset <= at)
        {
            return None;
        }
        self.seven_day
    }

    /// How a pick ranks its condition, the harness's own order (`Condition::fresh`): first a row
    /// read lately with room ([`Self::undated`] takes none). Then an `unseen` row, whose missing or
    /// old figures would otherwise rank it as the least used of all: at worst, on a member that
    /// says `read_at=`, its launch finds a wall, holds and stalls by the startup deadline, or on a
    /// lone row under rotation off, whose launch reads nothing, the worker meets the wall and the
    /// row's own rollouts show it from then on. A row no read can show a wall on says `launch=0`
    /// ([`Self::passed_over`]). A member
    /// older than the field has no such deadline, so a worker there can sit `working` at the wall
    /// for days: a pick reads no `read_at=` and `read_at=-` alike, so it takes such a member's
    /// `unseen` row as it does a current one's ([`Self::takes`]). A `low` row goes last, since a
    /// worker on one stops mid-task.
    fn rank(&self) -> u8 {
        match self.condition.as_str() {
            LOW => 2,
            UNSEEN => 1,
            _ => 0,
        }
    }

    /// Whether a figure of it reads its window full at `at_ms`: a worker sent there stops at once,
    /// whatever its condition word says, and a member older than the survey's own rule for this
    /// still sends `usable` or `low` for it.
    fn spent(&self, at_ms: Option<u64>) -> bool {
        self.seven_day_at(at_ms).is_some_and(|pct| pct >= 100)
            || self.five_hour.is_some_and(|pct| pct >= 100)
    }

    /// Whether it says `usable` and not when it was read. No aterm that says `read_at=` does: its
    /// survey keeps `usable` only with a read to show for it, and calls any other `unseen`. So
    /// this is an aterm older than the field, whose word may have stopped being true at any point
    /// since, and whose launcher, with no startup deadline on a hold, would keep a worker sent
    /// there `working` for days (m21 read `usable` at 1% while Codex stopped at a weekly wall).
    fn undated(&self) -> bool {
        self.condition == USABLE && self.read_at.is_none()
    }

    /// Whether its member says a launch there would not start on it: that launch would start on
    /// another row, on a login no row lists or none can show a wall on, or be refused. `launch=0`
    /// does not say which, and
    /// `aterm harness accounts` there does. A member older than the field says nothing, so none of
    /// its rows is passed over.
    fn passed_over(&self) -> bool {
        self.launch == Some(false)
    }

    /// Whether a worker of `program` can run on it now: a row that would ([`Self::would_take`]),
    /// not one whose word cannot be dated ([`Self::undated`]), and not one a launch there would
    /// pass over ([`Self::passed_over`]). An `unseen` row still takes one though it read nothing
    /// lately: rows go `unseen` after six hours and only a launch reads one again, so refusing them
    /// would leave an idle fleet with nowhere to start.
    fn takes(&self, program: &str, at_ms: Option<u64>) -> bool {
        self.would_take(program, at_ms) && !self.undated() && !self.passed_over()
    }

    /// A row of `program`, enrolled, no window of it full ([`Self::spent`]), in a condition that
    /// takes a session, or `limited` by a wall that ended by `at_ms` (the member's clock, so no
    /// clock between the two Macs is compared: its rows may be read up to a minute before it
    /// answers; a wall with no clock to judge it by stands).
    fn would_take(&self, program: &str, at_ms: Option<u64>) -> bool {
        let ended = || {
            self.walled_until_ms
                .zip(at_ms)
                .is_some_and(|(until, at)| until <= at)
        };
        self.program == program
            && self.enrolled
            && !self.spent(at_ms)
            && (RUNS.contains(&self.condition.as_str()) || (self.condition == LIMITED && ended()))
    }
}

/// One member a survey of the fleet answered, as a pick weighs it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Member {
    /// The node its book entry names, which the spawn dials it by (`--host <node>`).
    pub(crate) node: Option<String>,
    /// It proved the key this Mac binds that node to.
    pub(crate) proven: bool,
    /// How the read named it.
    pub(crate) label: String,
    /// Its host, as its own facts say it (`host=`).
    pub(crate) host: Option<String>,
    /// The address its answer came from.
    pub(crate) addr: Option<String>,
    /// It lets this Mac drive it (`drive=1` in its instance facts).
    pub(crate) drive: bool,
    /// Its fleet access (`access=` in its instance facts; an aterm older than the lock sends
    /// none and enforces none, which reads as full): what a driver it lets in may do there.
    pub(crate) access: aterm_types::fleet_access::FleetAccess,
    /// Its clock when it answered (`at_ms=`).
    pub(crate) at_ms: Option<u64>,
    /// Its account rows, `None` where its aterm says nothing of them (one older than the field).
    pub(crate) accounts: Option<Vec<Row>>,
    /// The pools whose rows it could not read (`accounts_unread=`).
    pub(crate) unread: Vec<String>,
    /// Its sessions at work (the survey's `busy`).
    pub(crate) busy: usize,
    /// Its home, absolute, as its own facts say it (`home=`); `None` from an aterm that says
    /// nothing of it.
    pub(crate) home: Option<String>,
    /// The free space on its home's volume, in MiB (`home_free_mb=`).
    pub(crate) free_mb: Option<u64>,
    /// Its home's checkouts as its last read found them; `None` where it says nothing of them,
    /// read none yet, or its reply's rows did not all come.
    pub(crate) repos: Option<Repos>,
}

/// The checkouts aterm's setup cloned into a member's home, as its last read found them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Repos {
    /// When the read ended, in Unix milliseconds on the member's clock (`repos_at=`); `None`
    /// where it did not say.
    pub(crate) read_ms: Option<u64>,
    /// The checkouts (`repo` lines).
    pub(crate) rows: Vec<Repo>,
    /// The clones past the most a read lists, not read (`repos_cut=`): a name the read does not
    /// list may be one of them.
    pub(crate) cut: u64,
}

/// One checkout of a member's home ([`Repos`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Repo {
    /// Its folder's name in the home (`name=`).
    pub(crate) name: String,
    /// The commit its `HEAD` names (`head=`); `None` where none could be read.
    pub(crate) head: Option<String>,
    /// Commits on its origin's main it lacks, as of its last fetch (`behind=`).
    pub(crate) behind: Option<u64>,
}

impl Repo {
    fn of(fields: &Fields) -> Self {
        Self {
            name: fields.get("name").unwrap_or_default().to_owned(),
            head: fields.get("head").map(str::to_owned),
            behind: fields.number("behind"),
        }
    }
}

/// The word a spawn names what its worker needs of the Mac by: `need=<need>[,<need>…]`.
pub(crate) const NEED: &str = "need=";

/// How long a member's read of its checkouts stands for a [`Need::Repo`], on its own clock: an
/// older one cannot say what its home holds now.
pub(crate) const REPOS_FRESH_MS: u64 = 30 * 60 * 1000;

/// What a worker needs of the Mac it goes to ([`needs_of`]), each judged by that Mac's own survey
/// facts ([`Member::judge`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Need {
    /// A checkout `~/<name>` aterm's setup cloned, whose `HEAD` its last read named (`repo:<name>`).
    Repo(String),
    /// At least `mb` MiB free on its home's volume, as the spawn said it (`free:<n>m|g|t`).
    Free {
        /// The MiB it needs.
        mb: u64,
        /// The words it was asked by (`free:150g`).
        said: String,
    },
    /// A home its aterm names, which resolves a `cwd=~/…` there (a spawn's `cwd=~/…` adds it).
    Home,
}

impl Need {
    /// How the need is written (`repo:trust`, `free:150g`, `home`).
    pub(crate) fn said(&self) -> String {
        match self {
            Self::Repo(name) => format!("repo:{name}"),
            Self::Free { said, .. } => said.clone(),
            Self::Home => "home".to_owned(),
        }
    }
}

/// What a member's facts say of one [`Need`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Judged {
    /// It meets it.
    Met,
    /// It does not.
    Lacks,
    /// Its facts cannot say, for this reason.
    Unknown(Unsure),
}

/// Why a member's facts cannot say whether it meets a [`Need`] ([`Judged::Unknown`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unsure {
    /// It says nothing of the fact: its aterm is too old to, it read no checkouts yet, or its
    /// reply's rows did not all come.
    Unsaid,
    /// Its read of its checkouts says no date, or one not within [`REPOS_FRESH_MS`] of its answer
    /// on its own clock.
    Stale,
    /// Its read lists the checkout with no `HEAD` read.
    NoHead,
    /// Its read does not list the checkout, and left clones unlisted or does not say.
    Cut,
}

/// The needs `words` name (each `need=<need>[,<need>…]`, [`NEED`]): `repo:<name>` (a checkout of
/// that name in the home, [`Need::Repo`]) and `free:<n>m|g|t` (that much free on the home's
/// volume, in MiB, GiB or TiB, [`Need::Free`]), each once.
/// # Errors
/// A word names no need, or a need this does not know, or one past what can be counted.
pub(crate) fn needs_of(words: &[String]) -> Result<Vec<Need>, String> {
    let usage = |what: &str, why: &str| {
        format!(
            "need= {why}: `{}` (need=repo:<name>[,free:<n>g]; free in m, g or t, binary units)",
            aterm_codec::tty::one_line(what, 48)
        )
    };
    let mut needs: Vec<Need> = Vec::new();
    for word in words {
        let list = word.strip_prefix(NEED).unwrap_or(word);
        if list.is_empty() {
            return Err(usage(word, "names no need"));
        }
        for item in list.split(',') {
            let need = if let Some(name) = item.strip_prefix("repo:") {
                let ok = (1..=100).contains(&name.len())
                    && !name.starts_with('.')
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
                if !ok {
                    return Err(usage(item, "names no repository a home holds"));
                }
                Need::Repo(name.to_owned())
            } else if let Some(size) = item.strip_prefix("free:") {
                if !size.is_ascii() {
                    return Err(usage(item, "says no size"));
                }
                let (digits, unit) = size.split_at(size.len().saturating_sub(1));
                let scale: u64 = match unit.to_ascii_lowercase().as_str() {
                    "m" => 1,
                    "g" => 1024,
                    "t" => 1024 * 1024,
                    _ => return Err(usage(item, "says no size")),
                };
                let mb = (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| digits.parse::<u64>().ok())
                    .flatten()
                    .and_then(|n| n.checked_mul(scale))
                    .ok_or_else(|| usage(item, "says no size"))?;
                Need::Free {
                    mb,
                    said: item.to_ascii_lowercase(),
                }
            } else {
                return Err(usage(item, "names no need this knows"));
            };
            if !needs.contains(&need) {
                needs.push(need);
            }
        }
    }
    Ok(needs)
}

impl Member {
    /// What a person reads it as: its own host, else the name the read reached it by.
    pub(crate) fn host(&self) -> &str {
        self.host.as_deref().unwrap_or(&self.label)
    }

    /// Whether a spawn may go to it at all: it proved its node, it lets this Mac drive it, and
    /// its fleet access is Full Access, the only level that admits a spawn.
    fn drivable(&self) -> bool {
        self.granted() && self.access == aterm_types::fleet_access::FleetAccess::Full
    }

    /// Whether it proved its node and its grants let this Mac drive it, whatever its fleet access
    /// admits of a drive.
    fn granted(&self) -> bool {
        self.proven && self.node.is_some() && self.drive
    }

    /// What its facts say of `need` ([`Judged`]): a checkout counts where its read, within
    /// [`REPOS_FRESH_MS`] of its answer on its own clock, lists it with a `HEAD`; free space where
    /// its home's volume has at least that much; a home where it names one. Facts it does not
    /// say, or a read too old, cannot say; so cannot a checkout listed with no `HEAD` read, or
    /// one not listed by a read that did not reach every clone.
    pub(crate) fn judge(&self, need: &Need) -> Judged {
        match need {
            Need::Repo(name) => {
                let Some(repos) = &self.repos else {
                    return Judged::Unknown(Unsure::Unsaid);
                };
                // A read its clock puts after its answer is no read it can date: its clock moved.
                let fresh = repos
                    .read_ms
                    .zip(self.at_ms)
                    .and_then(|(read, at)| at.checked_sub(read))
                    .is_some_and(|age| age <= REPOS_FRESH_MS);
                if !fresh {
                    return Judged::Unknown(Unsure::Stale);
                }
                match repos
                    .rows
                    .iter()
                    .find(|repo| repo.name.eq_ignore_ascii_case(name))
                {
                    Some(repo) if repo.head.is_some() => Judged::Met,
                    Some(_) => Judged::Unknown(Unsure::NoHead),
                    None if repos.cut > 0 => Judged::Unknown(Unsure::Cut),
                    None => Judged::Lacks,
                }
            }
            Need::Free { mb, .. } => match self.free_mb {
                Some(free) if free >= *mb => Judged::Met,
                Some(_) => Judged::Lacks,
                None => Judged::Unknown(Unsure::Unsaid),
            },
            Need::Home => {
                if self.home.is_some() {
                    Judged::Met
                } else {
                    Judged::Unknown(Unsure::Unsaid)
                }
            }
        }
    }

    /// How it falls short of `needs`, `None` where it meets them all: a checkout it lacks first,
    /// then too little free space, then the first need its facts cannot say.
    fn short_of(&self, needs: &[Need]) -> Option<Short> {
        let judged: Vec<(&Need, Judged)> = needs.iter().map(|n| (n, self.judge(n))).collect();
        let lacks = |repo: bool| {
            judged.iter().any(|(need, judged)| {
                *judged == Judged::Lacks && matches!(need, Need::Repo(_)) == repo
            })
        };
        if lacks(true) {
            Some(Short::Repo)
        } else if lacks(false) {
            Some(Short::Room)
        } else {
            judged.iter().find_map(|(_, judged)| match judged {
                Judged::Unknown(why) => Some(Short::Unknown(*why)),
                Judged::Met | Judged::Lacks => None,
            })
        }
    }

    /// The row of `program` it says a launch there would start on (`launch=1`).
    fn launch_row(&self, program: &str) -> Option<&Row> {
        self.accounts
            .iter()
            .flatten()
            .find(|row| row.program == program && row.launch == Some(true))
    }

    /// Whether a sign-in there is what a launch of `program` lacks: the row it says a launch would
    /// start on has no login, or where it names none (older than the field, or that launch is
    /// refused), a row of `program` is signed out.
    fn signed_out(&self, program: &str) -> bool {
        match self.launch_row(program) {
            Some(row) => !row.enrolled || row.condition == SIGNED_OUT,
            None => self
                .accounts
                .iter()
                .flatten()
                .any(|row| row.program == program && row.condition == SIGNED_OUT),
        }
    }

    /// Its row that a worker of `program` would best run on: of the rows that can take it
    /// ([`Row::takes`], which is at most the row its survey says a launch there starts on), the
    /// least used week, then the least used five hours ([`used`]), then by label.
    fn best(&self, program: &str) -> Option<&Row> {
        self.accounts
            .iter()
            .flatten()
            .filter(|row| row.takes(program, self.at_ms))
            .min_by(|a, b| {
                (
                    a.rank(),
                    used(a.seven_day_at(self.at_ms)),
                    used(a.five_hour),
                    &a.label,
                )
                    .cmp(&(
                        b.rank(),
                        used(b.seven_day_at(self.at_ms)),
                        used(b.five_hour),
                        &b.label,
                    ))
            })
    }
}

/// How a member falls short of a spawn's needs ([`Member::short_of`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Short {
    /// It lacks a checkout a need names.
    Repo,
    /// It has less free space than a need names.
    Room,
    /// Its facts cannot say whether it meets them, for this reason.
    Unknown(Unsure),
}

/// A window's use as a pick ranks it: a window with no figure is unused. A member sends none once
/// the window has reset, where it was never read (a row the harness reads as fresh), or for a
/// window of another length. A lone row's full window of any length reads `limited` while its wall
/// stands, where its records or rollouts show it. On a pool of several, a window only rollouts know
/// gives no figure and no wall until a launch reads it, so such a row ranks as unused
/// (`ATERM-KI-1105-1`).
fn used(pct: Option<u64>) -> u64 {
    pct.unwrap_or(0)
}

/// The members each answered survey of `peers` is, in their order; one that did not answer, or
/// answered no survey, is left out.
pub(crate) fn members_of(peers: &[PeerSurvey]) -> Vec<Member> {
    peers
        .iter()
        .filter_map(|peer| {
            let lines = peer.answer.as_ref().ok()?;
            let survey = survey::parse_reply(lines).ok()?;
            let header = &survey.header;
            Some(Member {
                node: peer.node.clone(),
                proven: !peer.unproven,
                label: peer.label.clone(),
                host: header.get("host").map(str::to_owned),
                addr: peer.addr.clone(),
                drive: header.get("drive") == Some("1"),
                access: aterm_types::fleet_access::FleetAccess::of_header(header.get("access")),
                at_ms: header.number("at_ms"),
                accounts: survey
                    .accounts
                    .as_ref()
                    .map(|rows| rows.iter().map(Row::of).collect()),
                unread: survey.unread.clone(),
                busy: survey::busy_sessions(&survey),
                home: header.get("home").map(str::to_owned),
                free_mb: header.number("home_free_mb"),
                repos: survey.repos.as_ref().map(|rows| Repos {
                    read_ms: header.number("repos_at"),
                    rows: rows.iter().map(Repo::of).collect(),
                    cut: header.number("repos_cut").unwrap_or(u64::MAX),
                }),
            })
        })
        .collect()
}

/// The member a pick chose, the row it ranked it by, and how many could take the worker.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Chosen<'a> {
    /// The member.
    pub(crate) member: &'a Member,
    /// Its best row ([`Member::best`]).
    pub(crate) row: &'a Row,
    /// The members that could take it, this one among them.
    pub(crate) able: usize,
}

/// Why a pick chose no member: how far the members it read got.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    /// Members that answered.
    pub(crate) answered: usize,
    /// Of those, the ones this Mac's book names no node for: peers named by hand, whom no drive
    /// reaches and no grant mends.
    pub(crate) nodeless: usize,
    /// Of those with a node, the ones that did not prove the key this Mac binds to it, which no
    /// grant mends.
    pub(crate) unproven: usize,
    /// Of those that proved their node, the ones that do not let this Mac drive them.
    pub(crate) ungranted: usize,
    /// Of those that proved their node and let this Mac drive them, the ones whose fleet access
    /// is below Full Access, so they take no spawn until their person raises it.
    pub(crate) locked: usize,
    /// Of those, the ones that proved their node, let this Mac drive them and are at Full Access.
    pub(crate) drivable: usize,
    /// Of those, the ones that lack a checkout the spawn needs.
    pub(crate) no_repo: usize,
    /// Of those, the ones with less free space than the spawn needs (and every checkout it
    /// needs).
    pub(crate) no_room: usize,
    /// Of those, the ones whose facts do not say what the spawn needs ([`Unsure::Unsaid`]).
    pub(crate) unsaid: usize,
    /// Of those, the ones whose read of their checkouts cannot be dated as fresh
    /// ([`Unsure::Stale`]).
    pub(crate) stale: usize,
    /// Of those, the ones that list a checkout with no `HEAD` read ([`Unsure::NoHead`]).
    pub(crate) no_head: usize,
    /// Of those, the ones whose read may not have reached a checkout ([`Unsure::Cut`]).
    pub(crate) cut: usize,
    /// Of those that meet its needs, the ones whose aterm says nothing of its accounts.
    pub(crate) silent: usize,
    /// Of those that meet its needs, the ones whose aterm could not read the program's pool
    /// (`accounts_unread=`).
    pub(crate) unread: usize,
    /// Of those that meet its needs, the ones with a row that would take the worker but for an
    /// aterm too old to say when it read that row ([`Row::undated`]).
    pub(crate) undated: usize,
    /// Of those that meet its needs, the ones a sign-in there would mend ([`Member::signed_out`]).
    pub(crate) signed_out: usize,
    /// Of those that meet its needs, the ones with a row that would take the worker but that a
    /// launch there would not start on ([`Row::passed_over`]).
    pub(crate) passed_over: usize,
}

impl Counts {
    /// The members whose facts cannot say whether they meet the spawn's needs, for any reason.
    fn unknown(&self) -> usize {
        self.unsaid + self.stale + self.no_head + self.cut
    }
}

/// The member of `members` a worker of `program` goes to: of those that let this Mac drive them,
/// meet every one of its `needs` by their own facts ([`Member::judge`]; a member whose facts
/// cannot say is never chosen) and have a row it can run on now, which on a member that says which
/// row a launch there starts on is that row alone, the one whose best row ([`Member::best`]) ranks
/// first ([`Row::rank`]), then has used the least of its week, then of its five hours, then the one
/// with the fewest sessions at work, then the first by host (and node). Never one that does not let
/// this Mac drive it.
/// # Errors
/// No member qualifies: what the members it read were.
pub(crate) fn pick<'a>(
    program: &str,
    needs: &[Need],
    members: &'a [Member],
) -> Result<Chosen<'a>, Box<Counts>> {
    let able: Vec<(&Member, &Row)> = members
        .iter()
        .filter(|member| member.drivable() && member.short_of(needs).is_none())
        .filter_map(|member| Some((member, member.best(program)?)))
        .collect();
    let chosen = able.iter().min_by(|(a, ra), (b, rb)| {
        (
            ra.rank(),
            used(ra.seven_day_at(a.at_ms)),
            used(ra.five_hour),
            a.busy,
            a.host().to_ascii_lowercase(),
            &a.node,
        )
            .cmp(&(
                rb.rank(),
                used(rb.seven_day_at(b.at_ms)),
                used(rb.five_hour),
                b.busy,
                b.host().to_ascii_lowercase(),
                &b.node,
            ))
    });
    match chosen {
        Some((member, row)) => Ok(Chosen {
            member,
            row,
            able: able.len(),
        }),
        None => {
            let drivable: Vec<&Member> = members.iter().filter(|m| m.drivable()).collect();
            let short = |of: Short| {
                drivable
                    .iter()
                    .filter(|m| m.short_of(needs) == Some(of))
                    .count()
            };
            let fit: Vec<&&Member> = drivable
                .iter()
                .filter(|m| m.short_of(needs).is_none())
                .collect();
            let count = |of: &dyn Fn(&Member) -> bool| fit.iter().filter(|m| of(m)).count();
            let any_row =
                |m: &Member, of: &dyn Fn(&Row) -> bool| m.accounts.iter().flatten().any(of);
            let of_members =
                |of: &dyn Fn(&Member) -> bool| members.iter().filter(|m| of(m)).count();
            Err(Box::new(Counts {
                answered: members.len(),
                nodeless: of_members(&|m| m.node.is_none()),
                unproven: of_members(&|m| m.node.is_some() && !m.proven),
                ungranted: of_members(&|m| m.node.is_some() && m.proven && !m.drive),
                locked: of_members(&|m| m.granted() && !m.drivable()),
                drivable: drivable.len(),
                no_repo: short(Short::Repo),
                no_room: short(Short::Room),
                unsaid: short(Short::Unknown(Unsure::Unsaid)),
                stale: short(Short::Unknown(Unsure::Stale)),
                no_head: short(Short::Unknown(Unsure::NoHead)),
                cut: short(Short::Unknown(Unsure::Cut)),
                silent: count(&|m| m.accounts.is_none()),
                unread: count(&|m| {
                    m.accounts.is_some() && m.unread.iter().any(|pool| pool == program)
                }),
                undated: count(&|m| {
                    any_row(m, &|row| row.would_take(program, m.at_ms) && row.undated())
                }),
                signed_out: count(&|m| m.signed_out(program)),
                passed_over: count(&|m| {
                    any_row(m, &|row| {
                        row.would_take(program, m.at_ms) && !row.undated() && row.passed_over()
                    })
                }),
            }))
        }
    }
}

/// What the chosen member has of each of `needs`, for [`why`]'s line: each checkout and how far
/// behind it is, and its free space (`trust 3 behind, 812 GiB free`).
pub(crate) fn needs_met(needs: &[Need], member: &Member) -> String {
    let said: Vec<String> = needs
        .iter()
        .filter_map(|need| match need {
            Need::Repo(name) => {
                let repo = member
                    .repos
                    .iter()
                    .flat_map(|repos| &repos.rows)
                    .find(|repo| repo.name.eq_ignore_ascii_case(name))?;
                let name = aterm_codec::tty::one_line(&repo.name, 32);
                Some(match repo.behind {
                    Some(behind) => format!("{name} {behind} behind"),
                    None => name,
                })
            }
            Need::Free { .. } => member
                .free_mb
                .map(|mb| format!("{} free", survey::size_shown(mb))),
            Need::Home => None,
        })
        .collect();
    said.join(", ")
}

/// Why the pick chose `chosen` for `program`: its row's figures, its busy sessions, what it has
/// of the spawn's `needs`, and of how many.
pub(crate) fn why(program: &str, needs: &[Need], chosen: &Chosen<'_>) -> String {
    let row = chosen.row;
    let mut figures = String::new();
    for (window, pct) in [
        ("7d", row.seven_day_at(chosen.member.at_ms)),
        ("5h", row.five_hour),
    ] {
        if let Some(pct) = pct {
            figures.push_str(&format!(" {window} {}% left", survey::left_pct(pct)));
        }
    }
    if figures.is_empty() {
        figures = if row.condition == LIMITED {
            " its wall ended".to_owned()
        } else {
            format!(" {}", aterm_codec::tty::one_line(&row.condition, 16))
        };
    } else if row.condition == UNSEEN || row.condition == LOW {
        // Figures an `unseen` row still carries are old ones, and a `low` one's are near its wall.
        figures.push_str(&format!(" ({})", row.condition));
    }
    let busy = match chosen.member.busy {
        1 => "1 busy session".to_owned(),
        n => format!("{n} busy sessions"),
    };
    let has = needs_met(needs, chosen.member);
    let has = if has.is_empty() {
        String::new()
    } else {
        format!("; {has}")
    };
    format!(
        "{program} account {}{figures}, {busy}{has}; the best of {} that can take it",
        aterm_codec::tty::one_line(&row.label, 32),
        chosen.able
    )
}

/// The refusal of a pick for `program` that chose no member, with how far the `counts` got, the
/// `alone` reason the read gave for reading no other machine, and the spawn's `needs`. Each clause
/// says only what those members' facts show, with what mends it or what says more. A survey says
/// that a launch passes a row over and not why, so that clause names no cause.
pub(crate) fn none_able(
    program: &str,
    needs: &[Need],
    counts: &Counts,
    alone: Option<&str>,
) -> String {
    let asked = asked(program, needs);
    if counts.answered == 0 {
        return format!(
            "--host any: no Mac can take {asked}: no fleet member answered{}",
            alone.map_or_else(String::new, |why| format!(" ({why})"))
        );
    }
    let drive = format!(
        "--host any: no Mac can take {asked}: {} members answered, {} of them can be driven from \
         this Mac{}",
        counts.answered,
        counts.drivable,
        undriven(counts)
    );
    if counts.drivable == 0 {
        return drive;
    }
    let accounts = accounts_short(program, counts);
    if needs.is_empty() {
        return format!(
            "{drive}, and none of those would start it on a {program} account with no known wall \
             or sign-out{accounts}"
        );
    }
    let said = |of: &dyn Fn(&Need) -> bool| {
        needs
            .iter()
            .filter(|need| of(need))
            .map(Need::said)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut short = Vec::new();
    if counts.no_repo > 0 {
        short.push(format!(
            "{} lack a checkout it needs ({})",
            counts.no_repo,
            said(&|need| matches!(need, Need::Repo(_)))
        ));
    }
    if counts.no_room > 0 {
        short.push(format!(
            "{} have less free disk than it needs ({})",
            counts.no_room,
            said(&|need| matches!(need, Need::Free { .. }))
        ));
    }
    if counts.unknown() > 0 {
        short.push(format!(
            "{} cannot say whether they meet its needs ({})",
            counts.unknown(),
            unsure(counts)
        ));
    }
    let short = if short.is_empty() {
        String::new()
    } else {
        format!("; of those, {}", short.join(", "))
    };
    let fit = counts
        .drivable
        .saturating_sub(counts.no_repo + counts.no_room + counts.unknown());
    let rest = if fit == 0 {
        ", so none meets them".to_owned()
    } else {
        format!(
            ", and none of the {fit} that meet them would start it on a {program} account with no \
             known wall or sign-out{accounts}"
        )
    };
    format!("{drive}{short}{rest}")
}

/// What keeps the members that cannot be driven from being driven, and what mends each, in a
/// parenthesis; nothing where every member can be.
fn undriven(counts: &Counts) -> String {
    let mut why = Vec::new();
    if counts.ungranted > 0 {
        why.push(format!(
            "{} with no grant: `aterm fleet allow drive --from <this Mac>` there",
            counts.ungranted
        ));
    }
    if counts.locked > 0 {
        why.push(format!(
            "{} locked: its person raises it at its menu-bar lock or with `aterm fleet access \
             full` there",
            counts.locked
        ));
    }
    if counts.unproven > 0 {
        why.push(format!(
            "{} with no proof of the node key this Mac's book binds, which no grant mends: `aterm \
             fleet peers` here reads the issuer's book, else update aterm there or join it again",
            counts.unproven
        ));
    }
    if counts.nodeless > 0 {
        why.push(format!(
            "{} named by hand, with no node a drive can go to: `aterm fleet join` there",
            counts.nodeless
        ));
    }
    if why.is_empty() {
        String::new()
    } else {
        format!(" (of the others, {})", why.join("; "))
    }
}

/// Why the members counted as unsure cannot say, each cause with what mends it.
fn unsure(counts: &Counts) -> String {
    let mut why = Vec::new();
    if counts.unsaid > 0 {
        why.push(format!(
            "{} saying nothing of their home, checkouts or disk, from an aterm too old to, no \
             read of its checkouts yet or a reply cut short: update it, or ask again",
            counts.unsaid
        ));
    }
    if counts.stale > 0 {
        why.push(format!(
            "{} with no read of its checkouts dated within {} min on its clock: ask again, which \
             starts one",
            counts.stale,
            REPOS_FRESH_MS / 60_000
        ));
    }
    if counts.no_head > 0 {
        why.push(format!(
            "{} listing the checkout with no HEAD read: mend that checkout there",
            counts.no_head
        ));
    }
    if counts.cut > 0 {
        why.push(format!(
            "{} not listing the checkout, in a read that stopped short of every clone: name that \
             Mac with `--host <node>`",
            counts.cut
        ));
    }
    why.join("; ")
}

/// Why the members that meet a spawn's needs would not start it on an account, each with what mends
/// it or what says more, in a parenthesis; nothing where nothing but the known walls and sign-outs
/// the sentence names holds.
fn accounts_short(program: &str, counts: &Counts) -> String {
    let report = if program == "claude" {
        "aterm harness accounts".to_owned()
    } else {
        format!("aterm harness accounts --cli {program}")
    };
    let mut why = Vec::new();
    if counts.silent > 0 {
        why.push(format!(
            "{} of those say nothing of their accounts, from an aterm too old to or a reply cut \
             short: update it, or name it with `--host <node>`",
            counts.silent
        ));
    }
    if counts.unread > 0 {
        why.push(format!(
            "{} of those could not read their {program} account records: `{report}` there says why",
            counts.unread
        ));
    }
    if counts.undated > 0 {
        why.push(format!(
            "{} of those run an aterm too old to say when it read its {program} account: update \
             it, or name it with `--host <node>`",
            counts.undated
        ));
    }
    if counts.signed_out > 0 {
        why.push(format!(
            "{} of those signed out of {program}: `{report}` there names the sign-in",
            counts.signed_out
        ));
    }
    if counts.passed_over > 0 {
        why.push(format!(
            "{} of those holding such an account, where a launch would start on another row, on a \
             login no row lists or none can show a wall on, or be refused: `{report}` there says \
             where it would run or why it is refused, and `--host <node>` still starts a worker \
             there unless that launch is refused",
            counts.passed_over
        ));
    }
    if why.is_empty() {
        String::new()
    } else {
        format!(" ({})", why.join("; "))
    }
}

/// What a spawn asked, as a refusal names it: `agent=<program>` and its needs
/// (`agent=claude need=repo:trust,free:150g`, and `cwd=~/…` where a home is needed).
fn asked(program: &str, needs: &[Need]) -> String {
    let listed: Vec<String> = needs
        .iter()
        .filter(|need| **need != Need::Home)
        .map(Need::said)
        .collect();
    let mut asked = format!("agent={program}");
    if !listed.is_empty() {
        asked.push_str(&format!(" {NEED}{}", listed.join(",")));
    }
    if needs.contains(&Need::Home) {
        asked.push_str(" cwd=~/\u{2026}");
    }
    asked
}

#[cfg(test)]
#[path = "any_host_tests.rs"]
mod tests;
