// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure state and health classification for each AI CLI from its [`Facts`].

use super::connector::Connector;
use super::facts::{Facts, Kept};
use super::store::Ended;

/// Maximum age in seconds for a recent use to classify as [`State::Ok`].
pub const OK_FRESH_S: i64 = 6 * 3600;

/// One CLI's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// A login exists and was used within [`OK_FRESH_S`].
    Ok,
    /// A login exists and nothing has used it lately.
    Idle,
    /// The usage limit is reached until this time.
    Limited(i64),
    /// Claude usage forecast ends by this time.
    Due(i64),
    /// It has run here, and nothing says whether a login exists.
    Unproven,
    /// No login exists, or its login has ended.
    SignedOut,
    /// Wanted and served here, and not installed.
    NotInstalled,
    /// Not to be used here.
    Off,
}

impl State {
    /// Its word.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            State::Ok => "ok",
            State::Idle => "idle",
            State::Limited(_) => "limited",
            State::Due(_) => "due",
            State::Unproven => "unproven",
            State::SignedOut => "signed-out",
            State::NotInstalled => "not-installed",
            State::Off => "off",
        }
    }

    /// Whether a review may be asked of it.
    #[inline]
    #[must_use]
    pub const fn usable(self) -> bool {
        matches!(
            self,
            State::Ok | State::Idle | State::Due(_) | State::Unproven
        )
    }

    /// Whether a wanted CLI has a definite install or sign-in action, aterm's own or a
    /// person's ([`Row::actor`] says whose).
    #[inline]
    #[must_use]
    pub const fn needs_act(self, wanted: bool) -> bool {
        wanted && matches!(self, State::NotInstalled | State::SignedOut | State::Due(_))
    }
}

/// Who makes a row's fix ([`Row::fix`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    /// aterm itself, at a terminal or none and under an agent too: an install (`aterm pkg
    /// install`), or a sign-in or renewal through accounts v2's or the CLI's own pool's sign-in
    /// ladder, of a row with an email that nothing bars from running unattended ([`bar`]).
    Aterm,
    /// A person at this Mac: the CLI's own sign-in, which only a person's approval in a browser
    /// completes, an email only a person knows, a roster only a person can mend, or the step a
    /// [`Bar`] names (a row's own browser profile signed in once, a sign-in while aterm's are
    /// switched off).
    Person,
}

impl Actor {
    /// Its word in `ai doctor --json`.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Actor::Aterm => "aterm",
            Actor::Person => "person",
        }
    }
}

/// Who signs `f`'s CLI in: aterm where accounts v2 renews Claude Code's row or the row a sign-in
/// moves to (each has an email), or the CLI's own pool has a row to renew whose page its renewal
/// approves by itself ([`super::facts::CliAccounts::sign_in_target`] with `renew`,
/// [`crate::harness::accounts::Cli::renews_headlessly`]); a person otherwise, as
/// [`super::login::sign_in`] does it: the CLI's own sign-in (Codex's for the row on this
/// machine's own login on a Codex build that keeps no row's login its own,
/// [`super::login::default_home_sign_in`]), a pool's `accounts login` (Grok Build's, whose page a
/// headless browser is refused), or Claude's adoption of the default login asking its email.
#[must_use]
pub fn sign_in_actor(f: &Facts) -> Actor {
    if ladder(f) && bar(f).is_none() {
        Actor::Aterm
    } else {
        Actor::Person
    }
}

/// Whether `f`'s sign-in is a ladder's renewal ([`sign_in_actor`]): accounts v2's of a Claude row
/// with an email, or the CLI's own pool's `renew` of its row.
fn ladder(f: &Facts) -> bool {
    if f.connector.is_claude() {
        f.accounts
            .as_ref()
            .is_some_and(|a| a.refused.is_none() && (a.renews || a.next_sign_in.is_some()))
    } else {
        f.pool.as_ref().is_some_and(|p| {
            p.refused.is_none()
                && crate::harness::accounts::Cli::parse(f.connector.id)
                    .and_then(|cli| p.sign_in_target(cli))
                    .is_some_and(|(_, verb)| verb == "renew")
        })
    }
}

/// What keeps aterm's own sign-in of a row from running with nobody at this Mac ([`bar`]), and
/// the one act a person makes instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bar {
    /// Why aterm's own sign-in does not run unattended.
    pub why: String,
    /// The command that starts the person's act.
    pub step: String,
}

/// What keeps aterm's own sign-in of `f`'s row, a ladder's renewal (accounts v2's of a Claude row
/// with an email, a pool's `renew` of its row), from running with nobody at this Mac, where
/// something does, so the
/// sign-in is a person's ([`sign_in_actor`]): a switch that keeps aterm's sign-ins off here,
/// for which the step is the person's own `aterm ai login <id>`; a Claude row no route signs in
/// unattended ([`super::facts::ClaudeAccounts::unattended`]), for which it is the row's own
/// browser profile signed in once (`aterm harness accounts browser <label>`), after which its
/// renewals run headless; or a pool row whose last renewal stopped for a person
/// ([`super::facts::CliAccounts::person_task`]), for which it is the step that stop named.
#[must_use]
pub fn bar(f: &Facts) -> Option<Bar> {
    if !ladder(f) {
        return None;
    }
    let id = f.connector.id;
    let off = f
        .accounts
        .as_ref()
        .and_then(|a| a.off)
        .or_else(|| f.pool.as_ref().and_then(|p| p.off));
    if let Some(switch) = off {
        return Some(Bar {
            why: format!("{switch} keeps aterm's own sign-ins off here"),
            step: format!("aterm ai login {id}"),
        });
    }
    if f.connector.is_claude() {
        let a = f.accounts.as_ref()?;
        if a.unattended {
            return None;
        }
        let label = a.next_sign_in.as_deref().unwrap_or(&a.label);
        return Some(Bar {
            why: format!(
                "no route signs `{label}` in with nobody at this Mac: no mailbox answers its \
                 email, and no browser profile of its own is signed in for a headless browser"
            ),
            step: format!("aterm harness accounts browser {label}"),
        });
    }
    let p = f.pool.as_ref()?;
    let cli = crate::harness::accounts::Cli::parse(id)?;
    let (label, _) = p.sign_in_target(cli)?;
    let step = match p.person_task? {
        crate::harness::accounts::SignInAction::Browser if cli.renews_headlessly() => {
            format!("aterm harness accounts browser {label} --cli {id}")
        }
        _ => format!("aterm harness accounts login {label} --cli {id}"),
    };
    Some(Bar {
        why: format!("`{label}`'s last renewal stopped for a person"),
        step,
    })
}

/// Why aterm's own sign-in of `f`'s row is left to the window's upkeep now, where it is: a run
/// with nobody at this Mac starts none, so an agent's or a script's run, however often it is
/// repeated, loads no vendor sign-in page the upkeep holds back (design R-13). That is a
/// failed sign-in of the row on record, which the upkeep's ladder retries on its own backoff and
/// whenever what it reads changes, or Cloudflare's bot check met within the upkeep's hold
/// ([`crate::harness::accounts_sync::PERSON_BACKOFF_SECS`]).
#[must_use]
pub fn held(f: &Facts, now: i64) -> Option<String> {
    let hold = crate::harness::accounts_sync::PERSON_BACKOFF_SECS;
    if let Some(at) = f
        .pool
        .as_ref()
        .and_then(|p| p.bot_check_at)
        .filter(|at| now.saturating_sub(*at) < hold)
    {
        return Some(format!(
            "its last sign-in met Cloudflare's bot check {} ago, which aterm never tries to \
             pass; the window's upkeep tries again after {}",
            span(now.saturating_sub(at)),
            when(at.saturating_add(hold))
        ));
    }
    let failed = f
        .accounts
        .as_ref()
        .and_then(|a| a.tried.as_ref())
        .or_else(|| f.pool.as_ref().and_then(|p| p.tried.as_ref()))
        .is_some_and(|t| t.outcome == "failed");
    if !failed {
        return None;
    }
    tried_words(f, now)
        .map(|words| format!("{words}; the window's upkeep retries it on its own backoff"))
}

/// What aterm's own sign-in of `f`'s row last came to, where it failed and stands on record:
/// `aterm's sign-in found no route 2 h ago: tried profile(none) mailbox(none configured) …`.
fn tried_words(f: &Facts, now: i64) -> Option<String> {
    let tried = f
        .accounts
        .as_ref()
        .and_then(|a| a.tried.as_ref())
        .or_else(|| f.pool.as_ref().and_then(|p| p.tried.as_ref()))?;
    let how = match tried.outcome {
        "no-route" => "found no route",
        _ => "failed",
    };
    Some(format!(
        "aterm's sign-in {how} {} ago: tried {}",
        span(now.saturating_sub(tried.since)),
        crate::harness::one_line(&tried.routes, 160)
    ))
}

/// How much a row matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mark {
    /// Working.
    Ok,
    /// Worth knowing; nothing to do.
    Note,
    /// Something to do soon.
    Warn,
    /// A person is needed.
    Fail,
}

impl Mark {
    /// Its word, as `aterm doctor` prints it.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Mark::Ok => "ok",
            Mark::Note => "note",
            Mark::Warn => "warn",
            Mark::Fail => "FAIL",
        }
    }
}

/// One CLI, judged.
#[derive(Debug, Clone)]
pub struct Row {
    /// The row.
    pub connector: &'static Connector,
    /// Whether it is wanted here.
    pub wanted: bool,
    /// Its state.
    pub state: State,
    /// How much it matters.
    pub mark: Mark,
    /// What is known, in one line.
    pub fact: String,
    /// The command that starts the one act that fixes it; `None` where no command does: nothing
    /// here signs it in, or a person must do something else first ([`Row::mend`]).
    pub fix: Option<String>,
    /// What a person does instead where its sign-in starts nothing until then
    /// ([`super::login::claude_mend`]: Claude Code's home is not the default one, accounts.toml
    /// is refused, accounts v2's records cannot be read, or this machine's own login is listed
    /// with no email), said alone: `mend accounts.toml (…), then sign claude in`. `fix` is then
    /// `None`, so nothing offers a command that only says why it starts nothing.
    pub mend: Option<String>,
    /// Who makes `fix` or `mend`, where there is one.
    pub actor: Option<Actor>,
    /// The installed version.
    pub version: Option<String>,
    /// Claude's plan.
    pub plan: Option<String>,
    /// The newest use, unix seconds, and how.
    pub used: Option<(i64, &'static str)>,
    /// Claude's forecast end.
    pub renew_by: Option<i64>,
    /// When the usage limit lifts.
    pub limited_until: Option<i64>,
}

/// `n` seconds as a short span: `35 min`, `5 h`, `3 d`.
#[must_use]
pub fn span(n: i64) -> String {
    let n = n.max(0);
    if n < 3600 {
        format!("{} min", (n / 60).max(1))
    } else if n < 2 * 86_400 {
        format!("{} h", n / 3600)
    } else {
        format!("{} d", n / 86_400)
    }
}

/// Unix seconds as a UTC date and time, `2026-11-02 14:05 UTC`.
#[must_use]
pub fn when(t: i64) -> String {
    match u64::try_from(t) {
        Ok(secs) => {
            let s = aterm_types::rfc3339::format_rfc3339(secs).replace('T', " ");
            format!("{} UTC", s.chars().take(16).collect::<String>())
        }
        Err(_) => t.to_string(),
    }
}

const fn login_seen(f: &Facts) -> Option<bool> {
    if let Some(a) = &f.accounts
        && a.login.is_some()
    {
        return Some(a.made);
    }
    if f.connector.is_claude() {
        return Some(f.profile_signed_in || f.login_file.is_some());
    }
    if f.hidden_store || f.connector.stores.is_empty() {
        return None;
    }
    Some(f.login_file.is_some())
}

/// Judges one CLI.
#[must_use]
pub fn assess(f: &Facts, wanted: bool, now: i64) -> Row {
    let c = f.connector;
    let version = f.managed.as_ref().and_then(|m| m.version.clone());
    let unmeasured = if c.measured {
        ""
    } else {
        " (not measured in this build)"
    };
    let mut row = Row {
        connector: c,
        wanted,
        state: State::Off,
        mark: Mark::Note,
        fact: String::new(),
        fix: None,
        mend: None,
        actor: None,
        version,
        plan: f.plan.clone(),
        used: None,
        renew_by: None,
        limited_until: None,
    };
    let off = |row: &mut Row, why: &str| {
        row.state = State::Off;
        row.mark = Mark::Note;
        row.fact = why.to_string();
    };
    if !wanted {
        off(
            &mut row,
            &format!("not used on this machine ([ai.{}] use = false)", c.id),
        );
        return row;
    }
    match f.kept {
        Some(Kept::Unavailable) => {
            off(&mut row, "no build for this platform");
            return row;
        }
        Some(Kept::Excluded) => {
            off(&mut row, "[packages] exclude names it");
            return row;
        }
        Some(Kept::Removed) => {
            off(
                &mut row,
                &format!(
                    "uninstalled here on purpose (`aterm pkg install {}` brings it back)",
                    c.id
                ),
            );
            return row;
        }
        None => {}
    }
    if f.managed.is_none() {
        row.state = State::NotInstalled;
        row.mark = Mark::Fail;
        row.fact = match &f.foreign {
            Some(p) => format!(
                "not installed by aterm (a copy it does not manage is at {}){unmeasured}",
                p.display()
            ),
            None => format!("not installed{unmeasured}"),
        };
        row.fix = Some(format!("aterm pkg install {}", c.id));
        row.actor = Some(Actor::Aterm);
        return row;
    }
    if let Err(value) = &f.home {
        row.state = State::Unproven;
        row.mark = Mark::Note;
        row.fact = if value.is_empty() {
            "no home directory is known (HOME is not set)".to_string()
        } else {
            format!(
                "{} is set to a relative path ({}), which aterm cannot place",
                c.home.var.unwrap_or("its home variable"),
                crate::harness::one_line(value, 60)
            )
        };
        return row;
    }
    let served_at = f
        .accounts
        .as_ref()
        .and_then(|a| a.served_at)
        .or_else(|| f.pool.as_ref().and_then(|p| p.served_at));
    let renewed_at = f
        .accounts
        .as_ref()
        .and_then(|a| a.renewed_at)
        .or_else(|| f.pool.as_ref().and_then(|p| p.renewed_at));
    let refreshed = f.login_file.as_ref().and_then(|(_, w)| *w);
    let newest_use = [f.record.used_at, served_at, refreshed, renewed_at]
        .into_iter()
        .flatten()
        .max();
    let ended = f
        .record
        .ended_at
        .filter(|end| newest_use.is_none_or(|u| *end >= u))
        .map(|at| {
            let why = f.record.ended_why.map_or("it ended", Ended::says);
            (at, why)
        });
    let claude_expired = f.accounts.as_ref().and_then(|a| a.state) == Some("expired");
    let pool_expired = f.pool.as_ref().and_then(|p| p.state) == Some("expired");
    row.used = [
        f.record
            .used_at
            .zip(f.record.used_how.map(crate::ai::store::How::word)),
        served_at.map(|t| (t, "served")),
        renewed_at.map(|t| (t, "renewed")),
        refreshed.map(|t| (t, "refreshed")),
    ]
    .into_iter()
    .flatten()
    .max_by_key(|(t, _)| *t);
    let pool = pool_words(f);
    let actor = sign_in_actor(f);
    let tried = tried_words(f, now);
    let bar = bar(f);
    // A person's step where something keeps aterm's own sign-in from running unattended, else
    // the connection's (on a Codex build that keeps no row's login its own, Codex's own sign-in
    // for the row on this machine's own login).
    let fix = bar
        .as_ref()
        .map(|b| b.step.clone())
        .unwrap_or_else(|| format!("aterm ai login {}", c.id));
    let barred = bar
        .as_ref()
        .filter(|_| tried.is_none())
        .map(|b| b.why.clone());
    // Where Claude Code's sign-in starts nothing until a person does something else, that is
    // the row's step, never a command that would only say why it starts nothing.
    let mend = super::login::claude_mend(f);
    let give_fix = |row: &mut Row| {
        match &mend {
            Some(m) => row.mend = Some(m.step.clone()),
            None => row.fix = Some(fix.clone()),
        }
        row.actor = Some(actor);
    };
    // On a Codex build that keeps no row's login its own, the row on this machine's own login
    // is the only one that runs, and while the pool cannot take it here (another Mac's warm set
    // holds it, another Mac's claim does, or this Mac cannot record its claims and is not its
    // home) `aterm ai login` signs nothing in.
    let held_default = f.pool.as_ref().is_some_and(|p| {
        p.unread
            && matches!(
                p.default_row,
                Some(crate::harness::codex_alab::DefaultRow::Other { .. })
            )
    });
    let signed_out = |row: &mut Row, fact: String| {
        row.state = State::SignedOut;
        row.mark = Mark::Fail;
        row.fact = [
            Some(format!("{fact}{unmeasured}")),
            mend.as_ref().map(|m| m.why.clone()),
            tried.clone(),
            barred.clone(),
            pool.clone(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" \u{b7} ");
        if !held_default {
            give_fix(row);
        }
    };
    // A launch aterm makes of a pool's CLI that is refused as the records stand runs nothing (no
    // worker starts, `aterm ai review` runs no reviewer of it), in its words.
    let launch_refused = f.pool.as_ref().and_then(|p| {
        p.launch_refused.as_deref().map(|why| {
            why.strip_suffix(crate::harness::accounts_pool::NO_WORKER)
                .unwrap_or(why)
        })
    });
    // Where the owner's pin holds every launch to a row not signed in here, the CLI is signed out
    // as aterm runs it, whatever the records of the row the pool would serve past the pin say,
    // and the sign-in that ends it is that row's: the pool's next sign-in signs it in
    // ([`super::facts::CliAccounts::next`]), as aterm's own or a person's ([`sign_in_actor`]).
    if let (Some(p), Some(why)) = (f.pool.as_ref(), launch_refused)
        && let Some(super::facts::LaunchEnd::SignIn(label)) = &p.launch_end
        && p.next.as_deref() == Some(label.as_str())
    {
        signed_out(
            &mut row,
            format!(
                "a launch aterm makes of it now is refused: {why} (the owner's pin holds every \
                 launch to `{label}`)"
            ),
        );
        return row;
    }
    // On a Codex build that keeps no row's login its own, the row on this machine's own login is
    // the only one it runs, under the owner's pin on another row too: while that row is not
    // signed in here, the CLI is signed out as aterm runs it, whatever the pinned row's records
    // and login file say, and the way back is two steps, that row's sign-in and then its pin.
    if let Some((label, stored)) = sign_in_then_pin(f) {
        use crate::harness::codex_alab::{DEFAULT_SIGN_IN, signed_out_says, unread_fix};
        let refused = launch_refused
            .map(|why| format!(" \u{b7} a launch aterm makes of it now is refused: {why}"))
            .unwrap_or_default();
        signed_out(
            &mut row,
            format!(
                "`{label}`, the only row this Codex build runs, is {}{refused}",
                signed_out_says(stored).0
            ),
        );
        row.fix = None;
        row.mend = Some(format!(
            "sign `{label}` in at a terminal (`{DEFAULT_SIGN_IN}`), then pin it (`{}`)",
            unread_fix(Some(label))
        ));
        row.actor = Some(Actor::Person);
        return row;
    }
    if claude_expired || pool_expired {
        // Accounts v2 and a CLI's own pool each write one word for a wall, a displacement by the
        // vendor's own sign-in and a check that found no login, so it is quoted as recorded, with
        // when.
        let (by, at) = if claude_expired {
            (
                "accounts v2",
                f.accounts.as_ref().and_then(|a| a.signed_out_at),
            )
        } else {
            (
                "its account pool",
                f.pool.as_ref().and_then(|p| p.signed_out_at),
            )
        };
        let fact = match at {
            Some(at) => format!(
                "its login has ended ({by} recorded it signed out {} ago)",
                span(now.saturating_sub(at))
            ),
            None => format!("its login has ended ({by} recorded it signed out)"),
        };
        signed_out(&mut row, fact);
        return row;
    }
    if let Some((at, why)) = ended {
        signed_out(
            &mut row,
            format!(
                "its login has ended: {why} {} ago",
                span(now.saturating_sub(at))
            ),
        );
        return row;
    }
    match login_seen(f) {
        Some(false) if !f.home_exists => {
            signed_out(&mut row, "never signed in here".to_string());
            return row;
        }
        Some(false) => {
            signed_out(&mut row, "no login is stored".to_string());
            return row;
        }
        None if newest_use.is_none() => {
            row.state = State::Unproven;
            row.mark = Mark::Note;
            row.fact = if f.hidden_store {
                "it keeps its login in the OS keyring, which aterm does not read".to_string()
            } else if f.home_exists {
                format!("it has run here; nothing has shown a login yet{unmeasured}")
            } else {
                format!("its home is not there; nothing has shown a login yet{unmeasured}")
            };
            return row;
        }
        _ => {}
    }
    let mut words = vec![match &f.plan {
        Some(plan) => format!("signed in ({plan})"),
        None => "signed in".to_string(),
    }];
    if let Some((t, how)) = row.used {
        words.push(format!("{how} {} ago", span(now.saturating_sub(t))));
    }
    let renew_by = f
        .accounts
        .as_ref()
        .and_then(|a| a.renew_by)
        .or_else(|| f.pool.as_ref().and_then(|p| p.renew_by));
    let limited = [f.limited_until, f.record.limited_until]
        .into_iter()
        .flatten()
        .filter(|t| *t > now)
        .max();
    row.limited_until = limited;
    row.renew_by = renew_by;
    let next_sign_in = f
        .accounts
        .as_ref()
        .and_then(|a| a.next_sign_in.as_deref())
        .or_else(|| f.pool.as_ref().and_then(|p| p.next_sign_in.as_deref()));
    if let Some(by) = renew_by {
        words.push(format!("renew by {}", when(by)));
        row.state = State::Due(by);
        row.mark = Mark::Warn;
        // On a Codex build that keeps no row's login its own, only Codex's own sign-in renews
        // the row on this machine's own login, and `aterm ai login` runs it only once that login
        // has ended, since it signs the login out first.
        if !f.pool.as_ref().is_some_and(serves_default_row) {
            give_fix(&mut row);
        }
    } else if let Some(until) = limited {
        if let Some(next) = next_sign_in {
            words.push(format!(
                "usage limit until {} (next account `{next}` needs sign-in)",
                when(until)
            ));
            row.state = State::Due(until);
            row.mark = Mark::Warn;
            give_fix(&mut row);
        } else {
            words.push(format!("usage limit until {}", when(until)));
            row.state = State::Limited(until);
            row.mark = Mark::Note;
        }
    } else if f.accounts.as_ref().and_then(|a| a.state) == Some("ok")
        || f.pool.as_ref().and_then(|p| p.state) == Some("ok")
        || newest_use.is_some_and(|t| now.saturating_sub(t) <= OK_FRESH_S)
    {
        row.state = State::Ok;
        row.mark = Mark::Ok;
    } else {
        row.state = State::Idle;
        row.mark = Mark::Ok;
    }
    if let Some(pool) = pool {
        words.push(pool);
    }
    // Any other refused launch: the row says why, and where nothing else on it asks for an act,
    // whose act ends it ([`super::facts::LaunchEnd`]). A change to the owner's pin or roster, or
    // the sign-in of a pinned row the pool cannot sign in, is a person's: a warning whose step is
    // the commands that end it where they are known, else what to do. The pool's own records (a
    // lease, a warm set) end it by themselves, so that is a note that asks nobody for anything.
    if let (Some(p), Some(why)) = (f.pool.as_ref(), launch_refused) {
        use super::facts::LaunchEnd;
        words.push(format!("a launch aterm makes of it now is refused: {why}"));
        if matches!(row.state, State::Ok | State::Idle) {
            let id = c.id;
            let mend = match (&p.launch_end, &p.launch_step) {
                (Some(LaunchEnd::Pool) | None, _) => None,
                (_, Some(step)) => Some(format!(
                    "re-pin {id}'s account pool so its launches run: {step}"
                )),
                (Some(LaunchEnd::SignIn(label)), None) => Some(format!(
                    "sign in `{label}`, the row the owner's pin holds {id}'s launches to, or pin \
                     another row (`aterm harness accounts --cli {id}` says each row's state)"
                )),
                (Some(LaunchEnd::Owner), None) => Some(format!(
                    "mend the pin or roster that refuses {id}'s launches (`aterm harness accounts \
                     --cli {id}` says each row's state)"
                )),
            };
            match mend {
                Some(mend) => {
                    row.mark = Mark::Warn;
                    row.fix = None;
                    row.mend = Some(mend);
                    row.actor = Some(Actor::Person);
                }
                None => row.mark = Mark::Note,
            }
        }
    }
    if !f.env_credentials.is_empty() {
        words.push(format!(
            "{} set in this environment would be used instead of the stored login",
            f.env_credentials.join(", ")
        ));
    }
    row.fact = format!("{}{unmeasured}", words.join(" · "));
    row
}

/// What `f`'s own accounts pool says in its row's fact ([`Facts::pool`]): how many accounts its
/// roster lists and how many are signed in here (a login made here that the pool has not
/// recorded signed out), the rows due for renewal, or why the roster is
/// refused; `None` for a CLI with no pool, Claude Code's included (its view is
/// [`Facts::accounts`]).
fn pool_words(f: &Facts) -> Option<String> {
    use crate::harness::codex_alab::DefaultRow;
    let pool = f.pool.as_ref()?;
    let file = pool.roster.file_name().map_or_else(
        || pool.roster.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    if let Some(why) = &pool.refused {
        return Some(format!(
            "its pool's roster ({file}) is refused: {}",
            crate::harness::one_line(why, 120)
        ));
    }
    let mut words = format!(
        "pool: {} {} ({} signed in here)",
        pool.rows,
        if pool.rows == 1 {
            "account"
        } else {
            "accounts"
        },
        pool.signed_in
    );
    if !pool.due.is_empty() {
        words.push_str(&format!("; renewal due: {}", pool.due.join(", ")));
    }
    if pool.default_only
        && let Some(label) = &pool.default_label
    {
        words.push_str(&format!(
            "; serves `{label}` alone, since this Codex build keeps no other account's login its \
             own"
        ));
        if let Some(DefaultRow::SignedOut { stored, .. }) = &pool.default_row {
            let (state, sign_in) = crate::harness::codex_alab::signed_out_says(*stored);
            words.push_str(&format!(
                ", and `{label}` is {state}: Codex's own sign-in in this machine's default home \
                 (`{}`) signs that login {sign_in} and keeps `{label}` on it",
                crate::harness::codex_alab::DEFAULT_SIGN_IN
            ));
        }
    }
    if pool.unread
        && let Some(DefaultRow::Other { label, why }) = &pool.default_row
    {
        words.push_str(&format!(
            "; `{label}`, the row on this machine's own login, cannot take a launch here now \
             ({why}), and this Codex build runs no other account, so nothing here signs it in"
        ));
    }
    // Codex's own sign-in renews the row on this machine's own login once its login has ended
    // only where that login still works: a signed-out row is signed in now, and one the pool
    // cannot take here now is signed in by nothing here.
    if pool.unread
        && !matches!(
            pool.default_row,
            Some(DefaultRow::SignedOut { .. } | DefaultRow::Other { .. })
        )
        && let Some(label) = pool.default_label.as_ref().filter(|l| pool.due.contains(l))
    {
        words.push_str(&format!(
            "; on this Codex build only Codex's own sign-in in this machine's default home \
             renews `{label}`'s login, which `{}` runs once that login has ended (it signs the \
             login out first)",
            crate::harness::codex_alab::DEFAULT_SIGN_IN
        ));
    }
    Some(words)
}

/// Whether `pool`'s serving row is the row on this machine's own login on a Codex program that
/// keeps no row's login its own ([`crate::ai::facts::CliAccounts::unread`]), where only Codex's
/// own sign-in renews it.
fn serves_default_row(pool: &crate::ai::facts::CliAccounts) -> bool {
    pool.unread && pool.serving_label.is_some() && pool.serving_label == pool.default_label
}

/// The row on this machine's own login, and whether a login of it is stored here, where `f`'s
/// way back is that row's sign-in and then its pin: on a Codex build that keeps no row's login its
/// own ([`super::facts::CliAccounts::unread`]) that row is the only one the build runs, it is not
/// signed in here ([`crate::harness::codex_alab::DefaultRow::SignedOut`]), and the owner's pin
/// holds every launch to another row (one of its own, or a label the roster does not list), so a
/// launch is refused. Its sign-in ([`crate::harness::codex_alab::DEFAULT_SIGN_IN`], a person's at
/// a terminal) leaves that pin where it is, and its pin
/// ([`crate::harness::codex_alab::unread_fix`]) ends it. `None` elsewhere.
#[must_use]
pub fn sign_in_then_pin(f: &Facts) -> Option<(&str, bool)> {
    use crate::harness::codex_alab::DefaultRow;
    let p = f.pool.as_ref()?;
    let Some(DefaultRow::SignedOut { label, stored }) = &p.default_row else {
        return None;
    };
    (p.unread
        && p.refused.is_none()
        && p.launch_refused.is_some()
        && p.pinned.as_deref().is_some_and(|pin| pin != label))
    .then_some((label.as_str(), *stored))
}

/// The doctor's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    /// `OK`, `WARN` or `FAIL`.
    pub word: &'static str,
    /// The ids whose rows are FAIL.
    pub failing: Vec<&'static str>,
    /// The ids whose rows are warn.
    pub warning: Vec<&'static str>,
    /// The ids whose rows are notes.
    pub noted: Vec<&'static str>,
    /// The FAIL and warn rows whose fix aterm makes itself ([`Actor::Aterm`]), by id.
    pub by_aterm: Vec<&'static str>,
    /// The FAIL and warn rows whose fix only a person makes ([`Actor::Person`]) by a command, by
    /// id: not those whose step is something else ([`Health::mends`]).
    pub by_person: Vec<&'static str>,
    /// The command that starts each of those fixes ([`Row::fix`]), in the same order.
    pub person_steps: Vec<String>,
    /// What a person does for each FAIL and warn row whose sign-in starts nothing until then
    /// ([`Row::mend`]), in its words.
    pub mends: Vec<String>,
}

/// The verdict over every row.
#[must_use]
pub fn health(rows: &[Row]) -> Health {
    let ids = |mark: Mark| {
        rows.iter()
            .filter(|r| r.mark == mark)
            .map(|r| r.connector.id)
            .collect::<Vec<_>>()
    };
    let failing = ids(Mark::Fail);
    let warning = ids(Mark::Warn);
    let noted = ids(Mark::Note);
    let by = |actor: Actor| {
        rows.iter()
            .filter(|r| r.mark >= Mark::Warn && r.actor == Some(actor) && r.mend.is_none())
            .map(|r| r.connector.id)
            .collect::<Vec<_>>()
    };
    let word = if !failing.is_empty() {
        "FAIL"
    } else if !warning.is_empty() {
        "WARN"
    } else {
        "OK"
    };
    Health {
        word,
        failing,
        warning,
        noted,
        by_aterm: by(Actor::Aterm),
        by_person: by(Actor::Person),
        person_steps: rows
            .iter()
            .filter(|r| r.mark >= Mark::Warn && r.actor == Some(Actor::Person) && r.mend.is_none())
            .map(|r| {
                r.fix
                    .clone()
                    .unwrap_or_else(|| format!("aterm ai login {}", r.connector.id))
            })
            .collect(),
        mends: rows
            .iter()
            .filter(|r| r.mark >= Mark::Warn)
            .filter_map(|r| r.mend.clone())
            .collect(),
    }
}

/// `ids` as words: `claude`, `muse and grok`, `codex, muse and grok`.
fn and_list(ids: &[&str]) -> String {
    match ids {
        [] => String::new(),
        [one] => (*one).to_string(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

impl Health {
    /// What comes next for the rows that need an act, never a command handed to a person that
    /// aterm runs itself: `aterm ai doctor`, which makes aterm's own fixes itself, at a terminal or
    /// none and under an agent too (its installs, and its sign-ins through accounts v2's or a
    /// pool's ladder), then the sign-ins no route of aterm's makes, each with the verb a person
    /// starts it by, then what a person does where a sign-in starts nothing until then
    /// ([`Health::mends`]); `None` where no row needs an act.
    #[must_use]
    pub fn next(&self) -> Option<String> {
        let aterm = (!self.by_aterm.is_empty()).then(|| {
            format!(
                "`aterm ai doctor` fixes {} itself",
                and_list(&self.by_aterm)
            )
        });
        let person = (!self.by_person.is_empty()).then(|| {
            format!(
                "a person signs {} in once at this Mac ({}): no route of aterm's does",
                and_list(&self.by_person),
                self.logins()
            )
        });
        let mends = (!self.mends.is_empty())
            .then(|| format!("a person at this Mac: {}", self.mends.join("; ")));
        let acts: Vec<String> = [aterm, person, mends].into_iter().flatten().collect();
        (!acts.is_empty()).then(|| acts.join("; "))
    }

    /// The `NEEDS YOU:` line for what only a person can give ([`Health::by_person`],
    /// [`Health::mends`]): each sign-in no route of aterm's makes, once, and what a person does
    /// where a sign-in starts nothing until then; `None` where aterm makes every fix itself.
    #[must_use]
    pub fn needs_you(&self) -> Option<String> {
        let signs = (!self.by_person.is_empty()).then(|| {
            format!(
                "sign {} in once at this Mac: {}",
                and_list(&self.by_person),
                self.logins()
            )
        });
        let acts: Vec<String> = signs
            .into_iter()
            .chain(self.mends.iter().cloned())
            .collect();
        (!acts.is_empty()).then(|| format!("NEEDS YOU: {}", acts.join("; ")))
    }

    /// The command that starts each sign-in only a person makes ([`Health::person_steps`]):
    /// `` `aterm ai login muse` ``, `` `aterm harness accounts browser m2-2` ``.
    fn logins(&self) -> String {
        self.person_steps
            .iter()
            .map(|step| format!("`{step}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The `health:` line's words after the colon.
    #[must_use]
    pub fn line(&self, next: &str) -> String {
        let plural = |n: usize, one: &str, many: &str| {
            if n == 1 {
                one.to_string()
            } else {
                format!("{n} {many}")
            }
        };
        match self.word {
            "FAIL" => format!(
                "FAIL — {} not working ({}); next: {next}",
                plural(self.failing.len(), "1 CLI is", "CLIs are"),
                self.failing.join(", ")
            ),
            "WARN" => format!(
                "WARN — {} ({}); next: {next}",
                plural(self.warning.len(), "1 warning", "warnings"),
                self.warning.join(", ")
            ),
            _ if self.noted.is_empty() => "OK".to_string(),
            _ => format!(
                "OK — {} ({})",
                plural(self.noted.len(), "1 note", "notes"),
                self.noted.join(", ")
            ),
        }
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
