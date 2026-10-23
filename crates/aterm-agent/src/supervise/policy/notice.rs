// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OWNER'S NOTICES WHEN A SIGN-IN, A RENEWAL OR AN ACCOUNT MOVE CANNOT FINISH ON ITS OWN (the owner, 2026-10-19: *"I want to see the notice of that if the auto login and account rotation is working be FAILED, then I need to see that when clear and simple instructions on what to do about it or what to expect"*).

/// One notice's constant words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoticeText {
    /// The notice's identity: the journal's word.
    pub id: &'static str,
    /// Its short code: the tail of the host's key for its ask, which the message engine caps (a tab's id and a word would not fit).
    pub code: &'static str,
    /// The band's title, without the tab's place (the host fits that on).
    pub title: &'static str,
    /// THE TITLE'S SHORT FORM: two or three words that say what happened, with no `Claude` and no tab (the band's `Tab N` capsule names the tab), which the band paints in the title's place where the whole title does not fit (the review of the band, 2026-10-20: at the default window every notice's title read as a stub, `Claude's lo…`).
    pub short_title: &'static str,
    /// What happened, one sentence.
    pub happened: &'static str,
    /// What aterm already did about it, one sentence.
    pub did: &'static str,
    /// The one thing to do now, one imperative sentence.
    pub todo: &'static str,
    /// The step in its place where the session's login is a roster row ([`Self::fill`] given its label), for a notice whose step is a sign-in by hand: it must be that row's own account, or two rows of the roster become one account, and nothing at the browser hand-off of the owner's own `/login` says so ([`LOGIN_SIGN_IN_AS`] is aterm's `/login`'s).
    pub todo_as: Option<&'static str>,
    /// What to expect once it is done, one sentence.
    pub expect: &'static str,
    /// [`Self::expect`] where the usage limit's reset is known and aterm continues the tab past it (`resume_limits`): `{reset}` names when ([`NoticeText::fill_reset`]), so the owner knows what happens if they do nothing, and when.
    pub expect_reset: Option<&'static str>,
    /// HOW TO END WHAT THE STEP STARTS, where it outlasts the notice: one sentence, a line of Details after the expectation (the review of the login carry, r3h-3: `accounts use` pins the account for every session, and the notice that asked for it named no way to end the pin).
    pub undo: Option<&'static str>,
    /// Every clause the policy fills its `{why}` with (none where it names none): what the tests measure each filling at, and what a development build's `login-notice` verb fills it with — never a clause this notice is not raised for.
    pub whys: &'static [&'static str],
    /// THE WORDS ITS RECORD CLOSES FIXED IN once the tab runs on a working login again (the host's login-back note: the login back, or a move onto another account landed): [`BACK`] for a lost or renewed login, [`SWITCHED`] for a usage limit, where no login was lost.
    pub back: &'static str,
}

/// A notice with its placeholders filled: what an exception carries to the host ([`super::exception::Exception::notice`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// [`NoticeText::id`].
    pub id: &'static str,
    /// [`NoticeText::code`].
    pub code: &'static str,
    /// [`NoticeText::title`].
    pub title: &'static str,
    /// [`NoticeText::short_title`].
    pub short_title: &'static str,
    /// [`NoticeText::did`], filled.
    pub did: String,
    /// [`NoticeText::expect`], filled.
    pub expect: String,
    /// [`Filled::command`]: the command the step runs, whole, where the step points at Details for it.
    pub command: Option<String>,
    /// [`Filled::short`]: what the band shows in the step's place where it cannot show the step whole.
    pub short: Option<&'static str>,
    /// [`Filled::undo`]: how to end what the step starts, a line of Details after the expectation.
    pub undo: Option<String>,
    /// [`NoticeText::back`].
    pub back: &'static str,
}

/// What a lost login's notice says once its login is back: the host closes the notice's one row in Settings ▸ Messages fixed in these words (the tab's place fitted on), so the record says how the loss ended ([`NoticeText::back`]).
pub const BACK: &str = "Claude's login is back";

/// What a usage limit's notice says once the tab runs on another account (the move onto it landed, or the owner signed another account in): no login was lost, so its record never reads [`BACK`] (the review of the login carry, r8c-7: every usage limit's record closed as `Claude's login is back`).
pub const SWITCHED: &str = "Claude switched accounts";

/// The placeholder a roster label fills.
pub const LABEL: &str = "{label}";

/// The placeholder a short clause of the policy's fills.
pub const WHY: &str = "{why}";

/// The placeholder a usage limit's reset fills, as its notice says it (`on Oct 23 at 9am`: [`crate::supervise::limit::ResetClock::words`]).
pub const RESET: &str = "{reset}";

/// What opens a command a step runs: its label is never cut.
const COMMAND: &str = "aterm harness";

/// The step in place of one whose command, its label whole, would pass the engine's cap: the command, whole, is then a line of Details ([`Filled::command`]).
pub const COMMAND_UNDER_DETAILS: &str = "Run the command under Details.";

/// What Claude Code says, and what the owner sees, when its login is gone.
const EXPIRED: &str = "Claude Code's login expired, so it cannot answer prompts.";

/// The one thing that ends a lost login the harness could not carry.
const TYPE_LOGIN: &str = "Type /login in the tab and sign in again.";

/// [`TYPE_LOGIN`] where the session's login is the roster row `{label}`: the
/// sign-in must be that row's account, or two rows of the roster become one
/// account.
const TYPE_LOGIN_AS: &str = "Sign in to {label}: type /login in the tab.";

/// What follows a sign-in made in the tab: Claude Code sends the prompt the
/// login swallowed again by itself, and aterm presses the success Enter.
const CONTINUES: &str = "The session continues by itself once you are signed in.";

/// What closes a sign-in aterm opened that nobody needs any more: Esc IN THE
/// TAB the band's title names — never `this tab`, which on the window's band
/// reads as whichever tab is in front.
const ESC_SIGN_IN: &str = "Press Esc in the tab to close its sign-in.";

/// What follows a renewal: the relaunch onto the login renewed for the
/// roster row `{label}`, with the wall's continuation.
const MOVES: &str = "aterm then restarts the tab on the login renewed for {label}.";

/// What a `{why}` says where `/login` was not typed ([`LOGIN_NOT_RUN`]): the owner's `retry_api_errors = false` (named without the key, whose `retry` the message engine refuses in a reason).
pub const WHY_RETRY_OFF: &str = "aterm.toml leaves API errors to you";

/// …the hour's typing budget (`continue_per_hour`) spent: the cap that keeps aterm from typing into a tab in a loop.
pub const WHY_BUDGET: &str = "it already typed here as often as an hour allows";

/// …a tab with no task: nothing is typed into it.
pub const WHY_NO_TASK: &str = "this tab has no task for aterm to carry on";

/// …a task Land landed (Autopilot Land: the one end a task has): nothing more is typed into it until someone's turn.
pub const WHY_TASK_LANDED: &str = "the task in this tab is landed";

/// …a person's own exchange left to them: the reply is theirs to answer, and nothing is typed until the next turn ends.
pub const WHY_TURN_YOURS: &str = "the last reply in this tab is yours to answer";

/// Every clause [`LOGIN_NOT_RUN`]'s `{why}` is filled with.
pub const WHYS: &[&str] = &[
    WHY_RETRY_OFF,
    WHY_BUDGET,
    WHY_NO_TASK,
    WHY_TASK_LANDED,
    WHY_TURN_YOURS,
];

/// What holds a renewed login's relaunch (`super::turn_end::RenewStep::ByHand`), [`RENEWED_BY_HAND`]'s `{why}`: `relaunch` off, `resume_limits` off, no host that relaunches the agent.
pub const BY_HAND_WHYS: &[&str] = &[
    "relaunch is off in aterm.toml",
    "resume_limits is off in aterm.toml",
    "it cannot restart this Claude Code",
];

/// The `/login` aterm typed opened its browser sign-in (the hand-off read on the screen), the method picker answered with the method the session already uses: the sign-in in the browser is the one step left, and only the owner can take it.
pub const LOGIN_SIGN_IN: NoticeText = NoticeText {
    id: "login-sign-in",
    code: "si",
    title: "Claude's login expired",
    short_title: "Login expired",
    happened: EXPIRED,
    did: "aterm typed /login in the tab and opened the sign-in in your browser.",
    todo: "Finish the browser sign-in.",
    todo_as: None,
    expect: CONTINUES,
    expect_reset: None,
    undo: None,
    whys: &[],
    back: BACK,
};

/// [`LOGIN_SIGN_IN`] for a session whose login is a roster row (`{label}`: the renewable login it runs on, or the account a move put it on): the browser sign-in must be that row's own account, or two rows of the roster become one account.
pub const LOGIN_SIGN_IN_AS: NoticeText = NoticeText {
    id: "login-sign-in-as",
    code: "sa",
    title: "Claude's login expired",
    short_title: "Login expired",
    happened: EXPIRED,
    did: "aterm typed /login in the tab and opened the sign-in in your browser.",
    todo: "Sign in to {label} in your browser.",
    todo_as: None,
    expect: CONTINUES,
    expect_reset: None,
    undo: None,
    whys: &[],
    back: BACK,
};

/// The method picker of the `/login` aterm typed, which aterm does not answer: nothing says the session signs in with a subscription (the only method chosen for the owner), or the picker is not drawn as the one measured — or Console's own menu behind it (`How do you want to sign in?`, one of whose options mints an API key).
pub const LOGIN_CHOOSE: NoticeText = NoticeText {
    id: "login-choose",
    code: "ch",
    title: "Claude's login expired",
    short_title: "Login expired",
    happened: EXPIRED,
    did: "aterm typed /login and leaves the choice of how to sign in to you.",
    todo: "Pick a sign-in method in the tab and finish signing in.",
    todo_as: None,
    expect: CONTINUES,
    expect_reset: None,
    undo: None,
    whys: &[],
    back: BACK,
};

/// The `/login` aterm typed was left before it signed in (`Login interrupted`, or its dialog gone with no login back): aterm types it once per lost login, never again.
pub const LOGIN_CANCELLED: NoticeText = NoticeText {
    id: "login-cancelled",
    code: "ca",
    title: "Claude sign-in cancelled",
    short_title: "Sign-in cancelled",
    happened: "Claude Code's sign-in was cancelled before it finished.",
    did: "aterm typed /login once and does not type it again by itself.",
    todo: TYPE_LOGIN,
    todo_as: Some(TYPE_LOGIN_AS),
    expect: CONTINUES,
    expect_reset: None,
    undo: None,
    whys: &[],
    back: BACK,
};

/// THE LOGIN CAME BACK IN ANOTHER TAB while the dialog of the `/login` aterm typed here still stands (its picker, its browser hand-off, Console's menu or a message of its own): nothing is left to sign in, and nobody is asked to — a second sign-in is what one sign-in per login exists to prevent.
pub const LOGIN_BACK_DIALOG: NoticeText = NoticeText {
    id: "login-back-dialog",
    code: "bd",
    title: "Claude sign-in still open",
    short_title: "Sign-in still open",
    happened: "Claude is signed in again in another tab, but this one's sign-in is still open.",
    did: "aterm does not ask you to sign in twice, and leaves the open sign-in to you.",
    todo: ESC_SIGN_IN,
    todo_as: None,
    expect: "The session continues by itself once the sign-in is closed.",
    expect_reset: None,
    undo: None,
    whys: &[],
    back: BACK,
};

/// A RENEWAL MADE WHILE THE `/login` aterm TYPED HERE STILL STANDS (its picker, its browser hand-off, or a message of its own), the tab's relaunch onto the renewed login in reach: nothing is left to sign in, and nobody is asked to — a browser sign-in there would bring back the login the renewal replaced, and the tab moves onto the renewed one anyway.
pub const LOGIN_RENEWED_DIALOG: NoticeText = NoticeText {
    id: "login-renewed-dialog",
    code: "re",
    title: "Claude sign-in still open",
    short_title: "Sign-in still open",
    happened: "This tab's login was renewed, but the sign-in aterm opened here is still open.",
    did: "aterm does not ask you to sign in again, and leaves the open sign-in to you.",
    todo: ESC_SIGN_IN,
    todo_as: None,
    expect: MOVES,
    expect_reset: None,
    undo: None,
    whys: &[],
    back: BACK,
};

/// A person's own `/login` at a lost login, left before it signed in (`Login interrupted` with no `/login` of aterm's): their answer, which the harness does not override with a `/login` of its own.
pub const LOGIN_LEFT: NoticeText = NoticeText {
    id: "login-left",
    code: "lf",
    title: "Claude sign-in cancelled",
    short_title: "Sign-in cancelled",
    happened: "Claude Code's sign-in was cancelled before it finished.",
    did: "aterm does not type /login again after a sign-in you cancelled.",
    todo: TYPE_LOGIN,
    todo_as: Some(TYPE_LOGIN_AS),
    expect: CONTINUES,
    expect_reset: None,
    undo: None,
    whys: &[],
    back: BACK,
};

/// The dialog stopped short of a login on a state of its own (an OAuth error, an account on hold): aterm presses none of its keys, whoever ran `/login`.
pub const LOGIN_STOPPED: NoticeText = NoticeText {
    id: "login-stopped",
    code: "st",
    title: "Claude sign-in stopped",
    short_title: "Sign-in stopped",
    happened: "Claude Code's sign-in stopped and shows a message in the tab.",
    did: "aterm leaves that message to you and presses no keys in the sign-in.",
    todo: "Follow the message in the tab to sign in.",
    todo_as: None,
    expect: CONTINUES,
    expect_reset: None,
    undo: None,
    whys: &[],
    back: BACK,
};

/// `/login` not typed at all, and not about to be (`{why}`: one of [`WHYS`]).
pub const LOGIN_NOT_RUN: NoticeText = NoticeText {
    id: "login-not-run",
    code: "nr",
    title: "Claude's login expired",
    short_title: "Login expired",
    happened: EXPIRED,
    did: "aterm did not type /login, because {why}.",
    todo: TYPE_LOGIN,
    todo_as: Some(TYPE_LOGIN_AS),
    expect: CONTINUES,
    expect_reset: None,
    undo: None,
    whys: WHYS,
    back: BACK,
};

/// `/login` not typed because a draft stands in the composer: the wall would swallow `/login` with it, so the draft goes first.
pub const LOGIN_NOT_RUN_DRAFT: NoticeText = NoticeText {
    id: "login-not-run-draft",
    code: "dr",
    title: "Claude's login expired",
    short_title: "Login expired",
    happened: EXPIRED,
    did: "aterm did not type /login, because the tab has a draft you have not sent.",
    todo: "Clear the draft in the tab, then type /login and sign in.",
    todo_as: Some("Sign in to {label}: clear the draft, then type /login."),
    expect: CONTINUES,
    expect_reset: None,
    undo: None,
    whys: &[],
    back: BACK,
};

/// The login `{label}` was renewed, and only a draft in the composer holds the tab's move onto it.
pub const RENEWED_DRAFT: NoticeText = NoticeText {
    id: "renewed-draft",
    code: "rd",
    title: "Claude login renewed",
    short_title: "Login renewed",
    happened: "This tab's login was renewed, and the tab still runs on the old one.",
    did: "aterm does not restart the tab on the renewed login while it has a draft.",
    todo: "Send the draft in the tab.",
    todo_as: None,
    expect: MOVES,
    expect_reset: None,
    undo: None,
    whys: &[],
    back: BACK,
};

/// The login `{label}` was renewed, and nothing here can move the tab onto it (`{why}`: `relaunch` or `resume_limits` off, no host that relaunches).
pub const RENEWED_BY_HAND: NoticeText = NoticeText {
    id: "renewed-by-hand",
    code: "rh",
    title: "Claude login renewed",
    short_title: "Login renewed",
    happened: "This tab's login was renewed, and the tab still runs on the old one.",
    did: "aterm cannot switch the tab to the renewed login: {why}.",
    todo: TYPE_LOGIN,
    todo_as: Some(TYPE_LOGIN_AS),
    expect: CONTINUES,
    expect_reset: None,
    undo: None,
    whys: BY_HAND_WHYS,
    back: BACK,
};

/// A roster account is signed out: no session is moved onto it.
pub const ACCOUNT_SIGNED_OUT: NoticeText = NoticeText {
    id: "account-signed-out",
    code: "so",
    title: "Claude account signed out",
    short_title: "Account signed out",
    happened: "A Claude account listed in accounts.toml is signed out.",
    did: "aterm switches no session to an account that is signed out.",
    todo: "Run aterm harness accounts login {label}",
    todo_as: None,
    expect: "Once it is signed in, aterm can switch sessions to it again.",
    expect_reset: None,
    undo: None,
    whys: &[],
    back: BACK,
};

/// A renewal signed in an account other than its row's: nothing live changed.
pub const RENEWAL_WRONG_ACCOUNT: NoticeText = NoticeText {
    id: "renewal-wrong-account",
    code: "wa",
    title: "Claude login renewed with wrong account",
    short_title: "Wrong account",
    happened: "A login renewal signed in to a different Claude account from the one it renews.",
    did: "aterm threw that sign-in away, so the login of {label} did not change.",
    todo: "Run aterm harness accounts renew {label}",
    todo_as: None,
    expect: "Until it is renewed again, sessions on {label} keep their old login.",
    expect_reset: None,
    undo: None,
    whys: &[],
    back: BACK,
};

/// Claude Code hit a multi-day usage limit and no other account in the roster can take over: the owner can sign in another account in-place — only where no roster row is the session's login (`super::turn_end::RosterWord::Unlisted`, or no roster word at all).
pub const LIMIT_SIGN_IN: NoticeText = NoticeText {
    id: "limit-sign-in",
    code: "ls",
    title: "Claude usage limit reached",
    short_title: "Usage limit",
    happened: "Claude Code hit this account's usage limit, and no other account is ready to take over.",
    did: "aterm has no other Claude account it can switch this tab to.",
    todo: "Sign in to another account: type /login in the tab.",
    todo_as: None,
    expect: CONTINUES,
    expect_reset: Some("The tab continues once you sign in, else after the reset {reset}."),
    undo: None,
    whys: &[],
    back: SWITCHED,
};

/// What a weekly limit on a roster row asks of nobody: a sign-in in the tab,
/// which would sign another account into the row's own login (the review's
/// acct-1).
const LIMIT_HAPPENED_ROW: &str =
    "Claude Code hit this account's usage limit, and the next listed account is not signed in.";

/// What follows the sign-in of the roster row `{label}` by name: the waiting
/// tab is decided again at the accounts' news, and moves.
const LIMIT_MOVES: &str = "Once {label} is signed in, aterm switches this tab to it by itself.";

/// [`LIMIT_MOVES`] where the limit's reset is known: the tab continues past
/// it if nobody signs the row in (`it`: the row the line before names, so
/// the sentence holds the reset at any label's length).
const LIMIT_MOVES_RESET: &str =
    "The tab moves to it once signed in, else continues after the reset {reset}.";

/// What follows a usage limit where aterm cannot tell which account is next
/// ([`LIMIT_UNREAD`]), its reset known: aterm continues the tab past it
/// (`resume_limits`).
const LIMIT_RESUMES: &str = "aterm continues the tab by itself after the reset {reset}.";

/// [`LIMIT_MOVES`] where the roster holds the tab at its limit even once
/// `{label}` is ready (`super::turn_end::HeldBy`): no switch follows the
/// sign-in by itself, and the notice that follows it names the step that
/// moves the tab ([`LIMIT_USE`]).
const LIMIT_NEXT: &str = "Once {label} is signed in, aterm names the next step to move the tab.";

/// What follows a step after which aterm names another, the limit's reset
/// known (`resume_limits` on): BOTH OUTCOMES, EACH WITH ITS CONDITION — the
/// step taken, aterm names the next one; not taken, the tab resumes at the
/// reset.
const LIMIT_NEXT_RESET: &str =
    "If you do, aterm names the next step, and if not, the tab resumes {reset}.";

/// What follows a step after which aterm names the one that moves the tab to
/// another account, whichever that is: the account [`LIMIT_ROSTER`] asks to
/// be added (`accounts add` prints the line that signs it in, and once it is
/// ready the tab moves, or the notice that follows names what does), or the
/// lines of aterm.toml [`LIMIT_STAY_HELD`] asks to be set.
const LIMIT_NEXT_ANY: &str = "aterm then names the next step to move this tab to another account.";

/// What follows a usage limit's notice where aterm continues nothing
/// past the limit's reset: the owner's `resume_limits = false`.
const LIMIT_NOT_RESUMED: &str =
    "aterm does not continue this tab after the limit resets, as resume_limits is off.";

/// A multi-day usage limit on a roster row's login, and the row `{label}` (one with an email) is not signed in on this machine: its renewal signs it in as its own account.
pub const LIMIT_RENEW: NoticeText = NoticeText {
    id: "limit-renew",
    code: "lr",
    title: "Claude usage limit reached",
    short_title: "Usage limit",
    happened: LIMIT_HAPPENED_ROW,
    did: "aterm found {label} in accounts.toml, ready to take over once signed in.",
    todo: "Run aterm harness accounts renew {label}",
    todo_as: None,
    expect: LIMIT_MOVES,
    expect_reset: Some(LIMIT_MOVES_RESET),
    undo: None,
    whys: &[],
    back: SWITCHED,
};

/// [`LIMIT_RENEW`] for a row `{label}` with no email: it signs in through `accounts login`.
pub const LIMIT_LOGIN: NoticeText = NoticeText {
    id: "limit-login",
    code: "lg",
    title: "Claude usage limit reached",
    short_title: "Usage limit",
    happened: LIMIT_HAPPENED_ROW,
    did: "aterm found {label} in accounts.toml, ready to take over once signed in.",
    todo: "Run aterm harness accounts login {label}",
    todo_as: None,
    expect: LIMIT_MOVES,
    expect_reset: Some(LIMIT_MOVES_RESET),
    undo: None,
    whys: &[],
    back: SWITCHED,
};

/// [`LIMIT_RENEW`] where the roster holds the tab at this limit even once `{label}` is signed in (`super::turn_end::RosterWord::Row`'s `held`: a limit from before this Claude Code started under `accounts = "wall"`, the owner's pin, a limit reset kept): no switch follows the sign-in by itself, so none is promised — the notice that follows names the step that moves the tab ([`LIMIT_USE`]; the review of the login carry, r4h-4).
pub const LIMIT_RENEW_HELD: NoticeText = NoticeText {
    id: "limit-renew-held",
    code: "ln",
    expect: LIMIT_NEXT,
    expect_reset: Some(LIMIT_NEXT_RESET),
    ..LIMIT_RENEW
};

/// [`LIMIT_LOGIN`] where the roster holds the tab at this limit even once `{label}` is signed in, as [`LIMIT_RENEW_HELD`].
pub const LIMIT_LOGIN_HELD: NoticeText = NoticeText {
    id: "limit-login-held",
    code: "lj",
    expect: LIMIT_NEXT,
    expect_reset: Some(LIMIT_NEXT_RESET),
    ..LIMIT_LOGIN
};

/// What [`LIMIT_USE`]'s `{why}` names (`super::turn_end::HeldBy`): the limit stands from before this Claude Code started, and the roster rotates at a limit the process itself hit.
pub const WHY_USE_EARLIER: &str = "this Claude Code started after the limit";

/// …the owner's pin on the tab's account (`accounts use`).
pub const WHY_USE_PIN: &str = "you pinned this account";

/// …a limit reset the account holds, expiring within a day, which aterm runs the account to its own wall to use.
pub const WHY_USE_RESET: &str = "this account has a limit reset to use";

/// …the relaunch onto `{label}` that aterm asked for and could not make (`super::turn_end::TurnEndState::account_refused`): the rotation itself failed, and the step's news ends the refusal, so the move is asked again (the review of the login carry, r5h-3: told the limit from before this Claude Code, which this process had hit itself).
pub const WHY_USE_UNMADE: &str = "it could not restart this Claude Code";

/// Every clause [`LIMIT_USE`]'s `{why}` is filled with.
pub const USE_WHYS: &[&str] = &[WHY_USE_EARLIER, WHY_USE_PIN, WHY_USE_RESET, WHY_USE_UNMADE];

/// A multi-day usage limit on a roster row's login that aterm does not move the tab off by itself, and another row `{label}` is ready to run now: the owner's `accounts use` makes that row the one every session follows, this tab included.
pub const LIMIT_USE: NoticeText = NoticeText {
    id: "limit-use",
    code: "lw",
    title: "Claude usage limit reached",
    short_title: "Usage limit",
    happened: "Claude Code hit this account's usage limit, and aterm does not switch this tab by itself.",
    did: "aterm found {label} ready, but {why}.",
    todo: "Run aterm harness accounts use {label}",
    todo_as: None,
    expect: "aterm then moves every Claude session to {label}, this tab included.",
    expect_reset: Some("Every session then moves, else the tab continues after the reset {reset}."),
    undo: Some(
        "aterm harness accounts use --auto ends the pin, and aterm then moves sessions between accounts by itself again.",
    ),
    whys: USE_WHYS,
    back: SWITCHED,
};

/// A multi-day usage limit on a roster row's login, another row `{label}` ready that nothing of the roster's holds the tab off, and only a draft in the tab's prompt keeps aterm from asking the switch: the relaunch needs an empty prompt, and a draft is never typed over (the review of the login carry, r6h-1: told nothing, the tab waited for the reset, days away, beside a ready account).
pub const LIMIT_DRAFT: NoticeText = NoticeText {
    id: "limit-draft",
    code: "ld",
    title: "Claude usage limit reached",
    short_title: "Usage limit",
    happened: "Claude Code hit this account's usage limit, and the tab has a draft in its prompt.",
    did: "aterm does not switch the tab to {label} while it has an unsent draft.",
    todo: "Send the draft in the tab.",
    todo_as: None,
    expect: "aterm then switches the tab to {label} by itself.",
    expect_reset: Some("aterm then switches the tab, else its limit resets {reset}."),
    undo: None,
    whys: &[],
    back: SWITCHED,
};

/// [`LIMIT_DRAFT`] where the words in the prompt are aterm's own (`super::turn_end::Composer::Own`: its upgrade's notice left part-typed), which aterm never sends and asks a person to clear (`super::turn_end::OWN_WORDS_STAND`): sent, they would be the owner's prompt to the model, carried into the conversation the switch resumes (the review of the login carry, r7h-4: told to send them).
pub const LIMIT_DRAFT_OWN: NoticeText = NoticeText {
    id: "limit-draft-own",
    code: "lc",
    happened: "Claude Code hit this account's usage limit, and its prompt holds aterm's own words.",
    did: "aterm does not switch the tab to {label} while its prompt holds words.",
    todo: "Clear aterm's words from the prompt in the tab.",
    ..LIMIT_DRAFT
};

/// [`LIMIT_DRAFT`] where the draft stands under Claude Code's open completion menu (`super::turn_end::Composer::Diverted`): Enter there runs or completes the menu's highlighted entry (`/clear` for a `/cl` left standing), not the draft (`super::turn_end::MENU_DIVERTS`; r7h-4: told to send it).
pub const LIMIT_DRAFT_MENU: NoticeText = NoticeText {
    id: "limit-draft-menu",
    code: "lm",
    happened: "Claude Code hit this account's usage limit, and the tab has a draft under an open menu.",
    todo: "Close the menu in the tab, then send the draft.",
    ..LIMIT_DRAFT
};

/// The notices a draft in the prompt alone raises, whose step is the draft's going: the point each stands at is decided again the moment the prompt reads otherwise, sent or cleared (the review of the login carry, r7h-3: cleared, the tab waited for the reset under a notice whose step was done).
pub const DRAFT_HOLDS: &[&str] = &[
    LIMIT_DRAFT.id,
    LIMIT_DRAFT_OWN.id,
    LIMIT_DRAFT_MENU.id,
    RENEWED_DRAFT.id,
];

/// What [`LIMIT_UNMADE`]'s `{why}` names where the tab's own program is Claude Code (`refused:not-a-shell-job`): ended, it would come back in no shell, so aterm never ends it to restart it.
pub const WHY_UNMADE_NO_SHELL: &str = "the tab runs Claude Code with no shell";

/// Every clause [`LIMIT_UNMADE`]'s `{why}` is filled with: the tab with no shell, and any other refusal or failure of this launch's restart ([`WHY_USE_UNMADE`]).
pub const UNMADE_WHYS: &[&str] = &[WHY_UNMADE_NO_SHELL, WHY_USE_UNMADE];

/// A multi-day usage limit on a roster row's login, another row `{label}` ready, and the restart onto it refused for THIS LAUNCH (`super::turn_end::Unmade::Launch`: `refused:*`, `failed:*`): asked again, it is refused the same way, so the owner's `accounts use` would pin every other session and leave this tab where it is (the review of the login carry, r6h-2: [`LIMIT_USE`] promised "this tab included", the step ended the latch, the move was refused again and the same notice came back).
pub const LIMIT_UNMADE: NoticeText = NoticeText {
    id: "limit-unmade",
    code: "lx",
    title: "Claude usage limit reached",
    short_title: "Usage limit",
    happened: "Claude Code hit this account's usage limit, and aterm cannot move it to another account.",
    did: "aterm found {label} ready, but {why}.",
    todo: "Quit Claude Code, then resume it in a shell tab.",
    todo_as: None,
    expect: "aterm can then switch it to {label} when it meets the limit again.",
    expect_reset: Some("aterm can then switch it, else continues the tab after the reset {reset}."),
    undo: None,
    whys: UNMADE_WHYS,
    back: SWITCHED,
};

/// What [`LIMIT_STAY`]'s `{why}` names: the lines of aterm.toml's `[harness]` table that let aterm switch the tab, for EVERY setting that keeps it from switching — `accounts = "off"` (`wall`, the least that switches a tab at its limit), `relaunch = false`, `resume_limits = false` — in that order, together ([`stay_why`]: one setting named, the owner set it, and the next notice named another — the review of the login carry, r4h-2).
pub const STAY_WHYS: &[&str] = &[
    "accounts = \"wall\"",
    "relaunch = true",
    "accounts = \"wall\" and relaunch = true",
    "resume_limits = true",
    "accounts = \"wall\" and resume_limits = true",
    "relaunch = true and resume_limits = true",
    "accounts = \"wall\", relaunch = true and resume_limits = true",
];

/// [`LIMIT_STAY`]'s `{why}` for the settings that keep aterm from switching a tab at its limit — the rotation off, `relaunch` off, `resume_limits` off — every one of them named ([`STAY_WHYS`]); `None` where none does.
#[must_use]
pub fn stay_why(accounts_off: bool, relaunch_off: bool, resume_off: bool) -> Option<&'static str> {
    let bits =
        usize::from(accounts_off) | usize::from(relaunch_off) << 1 | usize::from(resume_off) << 2;
    bits.checked_sub(1).map(|at| STAY_WHYS[at])
}

/// A multi-day usage limit on a roster row's login, another row `{label}` ready to run now that nothing of the roster's holds the tab off, and the owner's aterm.toml keeps aterm from switching the tab to it (the rotation off, or the relaunch it would take: `{why}` names every line that lets it); a row still to sign in, or one the roster holds the tab off, is [`LIMIT_STAY_HELD`]'s, whose next step is not a switch.
pub const LIMIT_STAY: NoticeText = NoticeText {
    id: "limit-stay",
    code: "lk",
    title: "Claude usage limit reached",
    short_title: "Usage limit",
    happened: "Claude Code hit this account's usage limit; aterm.toml's [harness] keeps this tab on it.",
    did: "aterm found {label} in accounts.toml but leaves this tab as it is.",
    todo: "Set {why}.",
    todo_as: None,
    expect: "aterm then switches this tab once another account is ready.",
    expect_reset: Some("aterm then switches the tab, else continues it after the reset {reset}."),
    undo: None,
    whys: STAY_WHYS,
    back: SWITCHED,
};

/// [`LIMIT_STAY`] where, with those lines set, the roster still holds the tab at this limit (`super::turn_end::RosterWord::Row`'s `held`, under the mode the step puts in force), or no row is ready yet and `{label}` is the one still to sign in: no switch follows by itself, so none is promised — the notice that follows names the next step (the review of the login carry, r4h-4; r5h-2: the sign-in followed a promised switch).
pub const LIMIT_STAY_HELD: NoticeText = NoticeText {
    id: "limit-stay-held",
    code: "lh",
    expect: LIMIT_NEXT_ANY,
    expect_reset: Some(LIMIT_NEXT_RESET),
    ..LIMIT_STAY
};

/// A multi-day usage limit on a roster row's login, and no other row is ready or left to sign in: the tab stays on its own account (a sign-in in the tab would sign another account into the row's login).
pub const LIMIT_ROSTER: NoticeText = NoticeText {
    id: "limit-roster",
    code: "lo",
    title: "Claude usage limit reached",
    short_title: "Usage limit",
    happened: "Claude Code hit this account's usage limit, and no other listed account can take over.",
    did: "aterm keeps this tab on its own account, so no other account replaces its login.",
    todo: "Run aterm harness accounts add <label>",
    todo_as: None,
    expect: LIMIT_NEXT_ANY,
    expect_reset: Some(LIMIT_NEXT_RESET),
    undo: None,
    whys: &[],
    back: SWITCHED,
};

/// A multi-day usage limit where the roster is refused, the accounts' state could not be read, or the login the tab's process runs on could not be (the review's acct-7; acct3-1): whose the login is, or which row could take over, is not known, so no sign-in in the tab is asked for, and the roster report says what is wrong (its refusal, or the state it could not read, ahead of anything else it prints).
pub const LIMIT_UNREAD: NoticeText = NoticeText {
    id: "limit-unread",
    code: "lu",
    title: "Claude usage limit reached",
    short_title: "Usage limit",
    happened: "Claude Code hit this account's usage limit, and aterm cannot tell which account is next.",
    did: "aterm keeps this tab on its own account, so no other account replaces its login.",
    todo: "Run aterm harness accounts",
    todo_as: None,
    expect: "The session continues by itself once an account is ready.",
    expect_reset: Some(LIMIT_RESUMES),
    undo: None,
    whys: &[],
    back: SWITCHED,
};

/// What follows [`LIMIT_ROSTER_KEPT`]'s step: `accounts add` prints the line
/// that signs the account in, and once it is signed in the point is decided
/// again on the accounts' news (`crate::supervise::run`'s `account_edge`,
/// heard where the decision read the roster and did nothing), whose notice
/// names the line of aterm.toml that lets the tab switch ([`LIMIT_STAY`],
/// [`LIMIT_STAY_HELD`]); and nothing resumes the tab at the reset.
const LIMIT_NEXT_KEPT: &str =
    "aterm then names the next step, and with resume_limits off, no reset resumes the tab.";

/// [`LIMIT_ROSTER`] where aterm continues nothing past the limit's reset (`resume_limits = false`, at a person's `/login` left at the limit): the account added does not continue the tab by itself either (the notice that follows its sign-in names the line that lets it, [`LIMIT_STAY`]), so the expectation says so (the review of the login carry, r4h-3: it said the session continues by itself once an account is ready) — and says that aterm names that next step (r9h-1: it said only that nothing resumes, and nothing decided the point again once the account was signed in, so the notice kept asking for a step already taken).
pub const LIMIT_ROSTER_KEPT: NoticeText = NoticeText {
    id: "limit-roster-kept",
    code: "lp",
    expect: LIMIT_NEXT_KEPT,
    expect_reset: None,
    ..LIMIT_ROSTER
};

/// [`LIMIT_UNREAD`] where aterm continues nothing past the limit's reset, as [`LIMIT_ROSTER_KEPT`].
pub const LIMIT_UNREAD_KEPT: NoticeText = NoticeText {
    id: "limit-unread-kept",
    code: "lq",
    expect: LIMIT_NOT_RESUMED,
    expect_reset: None,
    ..LIMIT_UNREAD
};

/// Every notice, for the tests that hold them to their shape.
pub const ALL: &[NoticeText] = &[
    LOGIN_SIGN_IN,
    LOGIN_SIGN_IN_AS,
    LOGIN_CHOOSE,
    LOGIN_CANCELLED,
    LOGIN_BACK_DIALOG,
    LOGIN_RENEWED_DIALOG,
    LOGIN_LEFT,
    LOGIN_STOPPED,
    LOGIN_NOT_RUN,
    LOGIN_NOT_RUN_DRAFT,
    RENEWED_DRAFT,
    RENEWED_BY_HAND,
    ACCOUNT_SIGNED_OUT,
    RENEWAL_WRONG_ACCOUNT,
    LIMIT_SIGN_IN,
    LIMIT_RENEW,
    LIMIT_RENEW_HELD,
    LIMIT_LOGIN,
    LIMIT_LOGIN_HELD,
    LIMIT_USE,
    LIMIT_DRAFT,
    LIMIT_DRAFT_OWN,
    LIMIT_DRAFT_MENU,
    LIMIT_UNMADE,
    LIMIT_STAY,
    LIMIT_STAY_HELD,
    LIMIT_ROSTER,
    LIMIT_ROSTER_KEPT,
    LIMIT_UNREAD,
    LIMIT_UNREAD_KEPT,
];

/// The characters of a roster label a notice's sentences name whole: a longer one is shown clipped there (it is a name, not a command), and a command it would push past the engine's cap is a line of Details in the step's place ([`COMMAND_UNDER_DETAILS`], [`Filled::command`]).
pub const LABEL_ROOM: usize = 24;

/// `text` with `label` and `why` in their placeholders.
fn fill_one(text: &str, label: Option<&str>, why: Option<&str>) -> String {
    let mut out = text.to_string();
    if let Some(label) = label {
        out = out.replace(LABEL, label);
    }
    if let Some(why) = why {
        out = out.replace(WHY, why);
    }
    out
}

/// The four parts filled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filled {
    pub happened: String,
    pub did: String,
    pub todo: String,
    pub expect: String,
    /// The command the step runs, whole, where it would not fit the step with its label whole: the step is then [`COMMAND_UNDER_DETAILS`], and whoever shows the notice puts this on a line of its Details.
    pub command: Option<String>,
    /// What the band shows in the step's place where it cannot show the step whole: [`COMMAND_UNDER_DETAILS`] for a step that runs a command (the band shows a command whole or not at all, and Details shows it whole); `None` for a sentence, which the band cuts at its end.
    pub short: Option<&'static str>,
    /// [`NoticeText::undo`], filled: how to end what the step starts.
    pub undo: Option<String>,
}

impl NoticeText {
    /// Every part with `label` and `why` put in their placeholders (a part with none is its constant), the step [`Self::todo_as`] where a label is given and the notice has one.
    #[must_use]
    pub fn fill(&self, label: Option<&str>, why: Option<&str>) -> Filled {
        self.fill_reset(label, why, None)
    }

    /// [`Self::fill`], and where `reset` names a usage limit's reset (`on Oct 23 at 9am`) and the notice has one, the expectation [`Self::expect_reset`] naming it.
    #[must_use]
    pub fn fill_reset(
        &self,
        label: Option<&str>,
        why: Option<&str>,
        reset: Option<&str>,
    ) -> Filled {
        let named = label.map(|l| {
            if l.chars().count() <= LABEL_ROOM {
                l.to_string()
            } else {
                let mut cut: String = l.chars().take(LABEL_ROOM).collect();
                cut.push('\u{2026}');
                cut
            }
        });
        let named = named.as_deref();
        let step = self
            .todo_as
            .filter(|_| label.is_some())
            .unwrap_or(self.todo);
        let mut todo = fill_one(step, label, why);
        let mut command = None;
        if todo.chars().count() > aterm_messages::need::STEP_CAP {
            match todo.find(COMMAND) {
                Some(at) => {
                    command = Some(todo[at..].to_string());
                    todo = COMMAND_UNDER_DETAILS.to_string();
                }
                None => todo = fill_one(step, named, why),
            }
        }
        let expect = match (self.expect_reset, reset) {
            (Some(text), Some(reset)) => fill_one(text, named, why).replace(RESET, reset),
            _ => fill_one(self.expect, named, why),
        };
        let short = todo.contains(COMMAND).then_some(COMMAND_UNDER_DETAILS);
        Filled {
            happened: fill_one(self.happened, named, why),
            did: fill_one(self.did, named, why),
            todo,
            expect,
            command,
            short,
            undo: self.undo.map(|undo| fill_one(undo, named, why)),
        }
    }

    /// The parts in the order a reader reads them.
    #[must_use]
    pub const fn parts(&self) -> [&'static str; 4] {
        [self.happened, self.did, self.todo, self.expect]
    }

    /// Whether a label given is named: a part, or [`Self::todo_as`], says [`LABEL`].
    #[must_use]
    pub fn takes_label(&self) -> bool {
        self.todo_as.is_some() || self.parts().iter().any(|p| p.contains(LABEL))
    }
}

#[cfg(test)]
#[path = "notice_tests.rs"]
mod tests;
