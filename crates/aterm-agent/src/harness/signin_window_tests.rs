// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::headless_oauth::GoogleGrant;

const EMAIL: &str = "one@example.com";
const AUTHORIZE: &str = "https://claude.ai/oauth/authorize?client_id=claude&redirect_uri=http%3A%2F%2Flocalhost%3A4321%2Fcallback&state=state";
const CALLBACK: &str = "http://localhost:4321/callback?code=secret&state=state";
const ANSWERED: &str = "https://platform.claude.com/oauth/code/success?app=claude-code";
const HOSTED_AUTHORIZE: &str = "https://claude.com/cai/oauth/authorize?client_id=claude&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&state=state";
const HOSTED_CALLBACK: &str =
    "https://platform.claude.com/oauth/code/callback?code=secret&state=state";
const LOGIN: &str = "https://claude.ai/login";
const CONSENT: &str = "https://claude.ai/oauth/authorize?client_id=claude";
const MAGIC: &str = "https://claude.ai/magic-link#token";

/// What a fake desk was asked to do, in order.
#[derive(Default)]
struct Log {
    opened: Vec<String>,
    reads: usize,
    pressed: Vec<Button>,
    filled: Vec<(Field, String)>,
    went: Vec<String>,
    /// The sign-in link read aside, once per read of its page.
    links: Vec<String>,
    /// The address each press and each set was told its document is at.
    acted_at: Vec<String>,
    forgot: usize,
    closed: usize,
    noted: usize,
    polls: usize,
    said: Vec<String>,
}

type PageOf = Box<dyn FnMut(&Log) -> Result<Facts, DeskError>>;
/// The fake mailbox: what the `n`th read gives.
type MailOf = Box<dyn FnMut(usize) -> Result<MailboxCredential, &'static str>>;

/// A desk whose page is a function of what was done to it, on a clock only `pause` moves.
struct Fake {
    log: Log,
    page: PageOf,
    now: Instant,
    open: Result<(), DeskError>,
    /// How long opening the window takes.
    open_takes: Duration,
    mail: Option<MailOf>,
    /// What the mailbox gave before: as the real reader, it gives each code or link once.
    given: Vec<MailboxCredential>,
    /// How each set of the code field ends, in order; the last repeats.
    code_fills: Vec<Filled>,
    /// How each set of the email field ends, in order; the last repeats.
    email_fills: Vec<Filled>,
    /// Whether an earlier sign-in of this flow's signed the window's store in as the account.
    store_is_the_accounts: bool,
    /// What each read of a sign-in link's page, aside, finds, in order; the last repeats.
    linked: Vec<Linked>,
    /// How many presses and sets fail before one is carried out.
    act_errors: usize,
    /// Where the page the last read gave is: every act is told that address.
    last_at: String,
    absent: Vec<Button>,
    cancel_after: Option<usize>,
}

/// The login code `code` as a sign-in link's page shows it.
fn shown(code: &str) -> Linked {
    Linked::Code(zeroize::Zeroizing::new(code.to_owned()))
}

impl Fake {
    fn new(page: impl FnMut(&Log) -> Result<Facts, DeskError> + 'static) -> Fake {
        Fake {
            log: Log::default(),
            page: Box::new(page),
            now: Instant::now(),
            open: Ok(()),
            open_takes: Duration::ZERO,
            mail: None,
            given: Vec::new(),
            code_fills: vec![Filled::Kept],
            email_fills: vec![Filled::Kept],
            store_is_the_accounts: false,
            linked: vec![Linked::Waiting],
            act_errors: 0,
            last_at: String::new(),
            absent: Vec::new(),
            cancel_after: None,
        }
    }

    fn with_mail(
        mut self,
        mail: impl FnMut(usize) -> Result<MailboxCredential, &'static str> + 'static,
    ) -> Fake {
        self.mail = Some(Box::new(mail));
        self
    }

    /// An act in the document at `at`: it is the one the read before gave, and one of the
    /// sign-in's own. The first [`Fake::act_errors`] fail.
    fn act(&mut self, at: &str) -> Result<(), DeskError> {
        assert_eq!(at, self.last_at, "an act names the document the read gave");
        assert!(at.starts_with("https://claude."), "{at}");
        self.log.acted_at.push(at.to_owned());
        if self.act_errors > 0 {
            self.act_errors -= 1;
            return Err(DeskError::Unread("the page was busy".into()));
        }
        Ok(())
    }
}

impl Desk for Fake {
    fn open(&mut self, url: &str, within: Duration) -> Result<(), DeskError> {
        self.open.clone()?;
        assert!(self.open_takes <= within, "the open outran the sign-in");
        self.now += self.open_takes;
        self.log.opened.push(url.to_owned());
        Ok(())
    }
    fn facts(&mut self) -> Result<Facts, DeskError> {
        self.log.reads += 1;
        let facts = (self.page)(&self.log)?;
        self.last_at.clone_from(&facts.url);
        Ok(facts)
    }
    fn press(&mut self, button: Button, email: &str, at: &str) -> Result<bool, DeskError> {
        assert_eq!(email, EMAIL, "a press is made for the account's own email");
        self.act(at)?;
        if self.absent.contains(&button) {
            return Ok(false);
        }
        self.log.pressed.push(button);
        Ok(true)
    }
    fn fill(&mut self, field: Field, value: &str, at: &str) -> Result<Filled, DeskError> {
        self.act(at)?;
        let nth = self.log.filled.iter().filter(|(f, _)| *f == field).count();
        self.log.filled.push((field, value.to_owned()));
        let ends = match field {
            Field::Code => &self.code_fills,
            Field::Email => &self.email_fills,
        };
        Ok(*ends.get(nth).or(ends.last()).expect("a fill's end"))
    }
    fn go(&mut self, url: &str) -> Result<(), DeskError> {
        // A sign-in link is spent by the first page that asks for it: the window's page is
        // never sent to one.
        assert!(
            !self
                .given
                .contains(&MailboxCredential::MagicLink(url.to_owned())),
            "the window's page was sent to the sign-in link"
        );
        self.log.went.push(url.to_owned());
        Ok(())
    }
    fn link_code(&mut self, link: &str, within: Duration) -> Result<Linked, DeskError> {
        assert_eq!(
            within.is_zero(),
            !self.log.links.is_empty(),
            "a link's page is given time at its first read, and one look after"
        );
        let nth = self.log.links.len();
        self.log.links.push(link.to_owned());
        Ok(self
            .linked
            .get(nth)
            .or(self.linked.last())
            .expect("a link's read")
            .clone())
    }
    fn forget(&mut self) {
        self.log.forgot += 1;
    }
    fn close(&mut self) {
        self.log.closed += 1;
    }
    fn store_is_the_accounts(&self) -> bool {
        self.store_is_the_accounts
    }
    fn note_signed_in(&mut self) {
        self.log.noted += 1;
    }
    fn has_mailbox(&self) -> bool {
        self.mail.is_some()
    }
    fn mailbox(&mut self, _within: Duration) -> Result<MailboxCredential, &'static str> {
        self.log.polls += 1;
        let polls = self.log.polls;
        let credential = self.mail.as_mut().expect("a mailbox")(polls)?;
        if self.given.contains(&credential) {
            return Err(MAILBOX_TIMEOUT);
        }
        self.given.push(credential.clone());
        Ok(credential)
    }
    fn now(&self) -> Instant {
        self.now
    }
    fn pause(&mut self, wait: Duration) {
        self.now += wait;
    }
    fn cancelled(&self) -> bool {
        self.cancel_after
            .is_some_and(|reads| self.log.reads >= reads)
    }
    fn say(&mut self, line: &str) {
        self.log.said.push(line.to_owned());
    }
}

fn at(url: &str) -> Facts {
    Facts {
        url: url.to_owned(),
        ..Facts::default()
    }
}

fn with_buttons(url: &str, buttons: &[&str]) -> Facts {
    Facts {
        buttons: buttons.iter().map(|title| (*title).to_owned()).collect(),
        ..at(url)
    }
}

fn login_page() -> Facts {
    with_buttons(LOGIN, &["Continue with Google", "Continue with email"])
}

fn code_page(filled: bool) -> Facts {
    Facts {
        code: Some(filled),
        ..with_buttons(LOGIN, &["Verify email address"])
    }
}

fn consent_page(text: &str) -> Facts {
    Facts {
        text: text.to_owned(),
        ..with_buttons(CONSENT, &["Decline", "Authorize"])
    }
}

fn bot_check_page() -> Facts {
    Facts {
        title: "Just a moment...".into(),
        text: "Verify you are human by completing the action below.".into(),
        markers: vec![Marker::ChallengeHost],
        // A check's page is read with its controls unread; a button named like the flow's own
        // shows that nothing on it is pressed.
        ..with_buttons(LOGIN, &["Continue with email", "Authorize"])
    }
}

fn run(authorize: &str, desk: &mut Fake) -> Result<Done, Stop> {
    let flow = MailboxFlow::new(authorize).unwrap();
    drive(
        &SignIn {
            authorize_url: authorize,
            flow: &flow,
            email: EMAIL,
            within: WITHIN,
        },
        desk,
    )
}

/// The whole sign-in as the page answers each act: the login page until `Continue with email`
/// is pressed, the code page until `Verify email address` is, the consent page until
/// `Authorize` is, then the page the waiting sign-in's listener sends the window on to.
fn whole_flow(log: &Log) -> Result<Facts, DeskError> {
    Ok(if log.pressed.contains(&Button::Authorize) {
        at(ANSWERED)
    } else if log.pressed.contains(&Button::VerifyEmailAddress) || !log.went.is_empty() {
        consent_page("Claude Code would like to connect. Logged in as one@example.com")
    } else if log.pressed.contains(&Button::ContinueWithEmail) {
        code_page(codes(log) > 0)
    } else {
        login_page()
    })
}

/// How many times the code field was set.
fn codes(log: &Log) -> usize {
    log.filled
        .iter()
        .filter(|(field, _)| *field == Field::Code)
        .count()
}

fn filled(values: &[(Field, &str)]) -> Vec<(Field, String)> {
    values
        .iter()
        .map(|(field, value)| (*field, (*value).to_owned()))
        .collect()
}

/// THE SIGN-IN FINISHES BY ITSELF: the window opens the exact authorization page once, the account's
/// email is set in the sign-in page's field before `Continue with email` is pressed, each of the
/// three exact buttons is pressed once and in order, the mailbox's code is set in the field
/// before `Verify email address` is pressed, and the flow ends when the waiting sign-in's
/// listener has answered the callback the page itself requested: nothing is left to deliver. The
/// window is closed once, and the store is recorded as the account's.
#[test]
fn a_sign_in_with_a_mailbox_code_finishes_by_itself_and_the_page_delivers_the_callback() {
    let mut desk = Fake::new(whole_flow).with_mail(|_| Ok(MailboxCredential::Otp("123456".into())));
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
    assert_eq!(desk.log.opened, [AUTHORIZE]);
    assert_eq!(
        desk.log.pressed,
        [
            Button::ContinueWithEmail,
            Button::VerifyEmailAddress,
            Button::Authorize
        ]
    );
    assert_eq!(
        desk.log.filled,
        filled(&[(Field::Email, EMAIL), (Field::Code, "123456")])
    );
    assert_eq!(desk.log.polls, 1);
    assert!(desk.log.went.is_empty());
    assert_eq!(desk.log.closed, 1);
    assert_eq!(desk.log.noted, 1);
    for line in &desk.log.said {
        assert!(
            !line.contains("123456") && !line.contains("secret"),
            "{line}"
        );
    }
}

/// A SIGN-IN LINK IS NEVER OPENED IN THE WINDOW'S PAGE: it is read aside, again and again,
/// and one that went on by itself (it signed its store in) has the window's page ask for the
/// authorization again, once; the code field is left alone. A hosted flow's callback stays in
/// the page's address and is returned for its one delivery. A link whose page shows no code
/// and goes nowhere is read until the deadline, and the window's page stands where it was.
/// CONTROL: a link that is not this sign-in's is never opened: the mailbox that gave it is
/// dropped, and the code is a person's to type until the deadline, which names why.
#[test]
fn a_sign_in_link_is_read_aside_and_never_opened_in_the_windows_page() {
    let hosted = |log: &Log| {
        Ok(if log.pressed.contains(&Button::Authorize) {
            at(HOSTED_CALLBACK)
        } else {
            whole_flow(log)?
        })
    };
    let mut desk =
        Fake::new(hosted).with_mail(|_| Ok(MailboxCredential::MagicLink(MAGIC.to_owned())));
    desk.linked = vec![Linked::Waiting, Linked::Waiting, Linked::Moved];
    assert_eq!(
        run(HOSTED_AUTHORIZE, &mut desk),
        Ok(Done::Callback(HOSTED_CALLBACK.to_owned()))
    );
    assert_eq!(desk.log.links, [MAGIC, MAGIC, MAGIC]);
    assert_eq!(desk.log.went, [HOSTED_AUTHORIZE]);
    assert_eq!(desk.log.polls, 1, "the mailbox gives a link once");
    assert_eq!(codes(&desk.log), 0);
    assert_eq!(
        desk.log.pressed,
        [Button::ContinueWithEmail, Button::Authorize]
    );
    assert_eq!(desk.log.closed, 1);
    let said = desk.log.said.join("\n");
    assert!(said.contains("shows no login code yet"), "{said}");
    assert!(said.contains("signed in by itself"), "{said}");

    // The link's page never shows a code and never moves: the window's page goes nowhere.
    let mut desk =
        Fake::new(whole_flow).with_mail(|_| Ok(MailboxCredential::MagicLink(MAGIC.to_owned())));
    assert_eq!(
        run(AUTHORIZE, &mut desk),
        Err(Stop::TimedOut(Phase::LinkWait))
    );
    assert!(desk.log.went.is_empty(), "{:?}", desk.log.went);
    assert!(desk.log.links.len() > 2);
    assert_eq!(desk.log.polls, 1);
    assert_eq!(desk.log.forgot, 0);

    let mut desk = Fake::new(whole_flow).with_mail(|_| {
        Ok(MailboxCredential::MagicLink(
            "https://example.invalid/magic-link#token".into(),
        ))
    });
    assert_eq!(
        run(AUTHORIZE, &mut desk),
        Err(Stop::MailboxDropped("its link is not this sign-in's"))
    );
    assert!(desk.log.went.is_empty() && desk.log.links.is_empty());
    assert_eq!(desk.log.polls, 1, "a dropped mailbox is read no more");
    assert_eq!(desk.log.closed, 1);
}

/// A SIGN-IN LINK THAT SHOWS A LOGIN CODE LEAVES THE PAGE AS IT STANDS: the link is opened
/// aside, the code its page shows is set in the field of the page that asked for it, and
/// `Verify email address` is pressed; the window's own page goes nowhere. CONTROL: a link's
/// code that is no code is refused, none is set, and the mailbox is dropped.
#[test]
fn a_sign_in_links_code_is_set_in_the_page_that_asked_for_it() {
    let mut desk =
        Fake::new(whole_flow).with_mail(|_| Ok(MailboxCredential::MagicLink(MAGIC.to_owned())));
    // The code shows at the second read of the link's page.
    desk.linked = vec![Linked::Waiting, shown("246810")];
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
    assert_eq!(desk.log.links, [MAGIC, MAGIC]);
    assert!(desk.log.went.is_empty(), "{:?}", desk.log.went);
    assert_eq!(
        desk.log
            .filled
            .iter()
            .filter(|(field, _)| *field == Field::Code)
            .map(|(_, value)| value.as_str())
            .collect::<Vec<_>>(),
        ["246810"]
    );
    assert_eq!(
        desk.log.pressed,
        [
            Button::ContinueWithEmail,
            Button::VerifyEmailAddress,
            Button::Authorize
        ]
    );
    assert!(
        desk.log
            .said
            .iter()
            .any(|line| line == "read the login code the sign-in email's link shows"),
        "{:?}",
        desk.log.said
    );
    assert!(
        desk.log.said.iter().all(|line| !line.contains("246810")),
        "{:?}",
        desk.log.said
    );

    let mut desk =
        Fake::new(whole_flow).with_mail(|_| Ok(MailboxCredential::MagicLink(MAGIC.to_owned())));
    desk.linked = vec![shown("not a code!")];
    assert_eq!(
        run(AUTHORIZE, &mut desk),
        Err(Stop::MailboxDropped(
            "its link shows a code that is not one a sign-in page takes"
        ))
    );
    assert_eq!(codes(&desk.log), 0);
    assert!(desk.log.went.is_empty());
    assert_eq!(desk.log.polls, 1);
}

/// THE LINK PAGE'S SCRIPT ANSWERS ITS CODE, THE SHAPE OF ITS TEXT AND ITS OWN ADDRESS, and
/// reads nothing in a document that is not the flow's own; an answer that is no answer holds
/// none of them.
#[test]
fn a_link_pages_answer_holds_its_code_its_texts_shape_and_its_address() {
    let script = link_code_script(PINNED).unwrap();
    assert!(script.contains("challenged()"), "{script}");
    assert!(!script.contains(".click("), "the link's page is only read");
    let foreign = script
        .find(r#"if (!own) { out.shape = "foreign";"#)
        .expect("its stop");
    assert!(foreign < script.find("document.body").expect("its read"));
    let read = parse_link_code(
        r#"{"at":"https://claude.ai/magic-link","code":"246810","shape":"5 4 <6> 4"}"#,
    );
    assert_eq!(read.code.as_deref().map(String::as_str), Some("246810"));
    assert_eq!(read.shape, "5 4 <6> 4");
    assert_eq!(read.at, "https://claude.ai/magic-link");
    let read = parse_link_code(r#"{"at":"https://claude.ai/x","code":null,"shape":"challenge"}"#);
    assert_eq!(read.code, None);
    assert_eq!(read.shape, "challenge");
    let read = parse_link_code("no json");
    assert!(read.code.is_none() && read.shape.is_empty() && read.at.is_empty());
}

/// THE MAILBOX IS READ AGAIN WHILE IT HOLDS NO SIGN-IN EMAIL YET, no faster than every two
/// seconds; a code that is no code is refused, none is set, and the mailbox is dropped at
/// once. A MAILBOX THAT CANNOT BE READ IS ASKED AGAIN A FEW TIMES AND THEN DROPPED, NOT THE
/// SIGN-IN: one connection that failed is not the row's end, a refused password is not asked
/// for four minutes, and the code page waits for a person to type the code until the
/// deadline, whose words say nobody typed one and why the mailbox was dropped, so the row's
/// record names what to repair. The line says once why, in the mailbox's own words.
#[test]
fn the_mailbox_is_read_again_until_its_email_arrives_and_an_unreadable_one_is_dropped() {
    let mut desk = Fake::new(whole_flow).with_mail(|poll| {
        if poll < 3 {
            Err(MAILBOX_TIMEOUT)
        } else {
            Ok(MailboxCredential::Otp("654321".into()))
        }
    });
    let began = desk.now;
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
    assert_eq!(desk.log.polls, 3);
    assert_eq!(codes(&desk.log), 1);
    assert!(desk.now - began >= MAILBOX_EVERY * 2);

    let mut desk =
        Fake::new(whole_flow).with_mail(|_| Ok(MailboxCredential::Otp("12 34\" & quit".into())));
    let began = desk.now;
    assert_eq!(
        run(AUTHORIZE, &mut desk),
        Err(Stop::MailboxDropped(
            "its code is not one a sign-in page takes"
        ))
    );
    assert_eq!(codes(&desk.log), 0);
    assert_eq!(desk.log.polls, 1, "a dropped mailbox is read no more");
    assert!(
        desk.now - began >= WITHIN,
        "the window's own deadline governs"
    );
    assert!(
        desk.log.said.iter().all(|line| !line.contains("12 34")),
        "{:?}",
        desk.log.said
    );

    for why in [
        "webmail inbox requires headless browser",
        "the mailbox refused its sign-in",
        "swarm roster busy or unread; sign-in deferred",
    ] {
        let mut desk = Fake::new(whole_flow).with_mail(move |_| Err(why));
        let began = desk.now;
        let stop = run(AUTHORIZE, &mut desk).unwrap_err();
        assert_eq!(stop, Stop::MailboxDropped(why), "{why}");
        assert_eq!(desk.log.polls, MAX_MAILBOX_ERRORS, "{why}");
        assert!(
            desk.now - began >= WITHIN,
            "the window's own deadline governs: {why}"
        );
        assert_eq!(desk.log.closed, 1);
        assert_eq!(desk.log.pressed, [Button::ContinueWithEmail], "{why}");
        assert_eq!(codes(&desk.log), 0, "{why}");
        let words = stop.to_string();
        assert!(words.contains("nobody typed the login code"), "{words}");
        assert!(words.contains(why), "the record names why: {words}");
        let dropped: Vec<_> = desk
            .log
            .said
            .iter()
            .filter(|line| line.contains("the window no longer reads it"))
            .collect();
        assert_eq!(dropped.len(), 1, "said once: {:?}", desk.log.said);
        assert!(dropped[0].contains(why), "{}", dropped[0]);
        // An ordinary failure: another try may read the mailbox.
        assert!(matches!(VisibleStop::from(stop), VisibleStop::Failed(_)));
    }

    // One read that failed, then the code: the sign-in goes on.
    let mut desk = Fake::new(whole_flow).with_mail(|poll| {
        if poll == 1 {
            Err("connection failed")
        } else {
            Ok(MailboxCredential::Otp("654321".into()))
        }
    });
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
    assert_eq!(desk.log.polls, 2);
}

/// A SIGN-IN WHOSE MAILBOX CANNOT BE READ, OR WHOSE MAILBOX ANSWERS WHAT NO PAGE TAKES, IS A
/// PERSON'S TO FINISH: the mailbox fails its three reads, or gives a code no sign-in page takes
/// or a link that is not this sign-in's at its first, and is read no more; the person at this
/// Mac types the emailed code in the window and presses `Verify email address` themselves
/// (aterm sets no code and presses nothing over theirs), and the sign-in goes on to the consent
/// page, which aterm authorizes, and its callback.
#[test]
fn a_sign_in_whose_mailbox_failed_is_finished_by_the_code_a_person_types() {
    type Mail = fn(usize) -> Result<MailboxCredential, &'static str>;
    let cases: [(Mail, usize); 3] = [
        (
            |_| Err("the mailbox refused its sign-in"),
            MAX_MAILBOX_ERRORS,
        ),
        (|_| Ok(MailboxCredential::Otp("12 34\" & quit".into())), 1),
        (
            |_| {
                Ok(MailboxCredential::MagicLink(
                    "https://tracking.example.invalid/c/magic".into(),
                ))
            },
            1,
        ),
    ];
    for (mail, reads) in cases {
        let mut dropped_at = None;
        let page = move |log: &Log| {
            Ok(if log.pressed.contains(&Button::Authorize) {
                at(ANSWERED)
            } else if !log.pressed.contains(&Button::ContinueWithEmail) {
                login_page()
            } else if log.polls < reads {
                code_page(false)
            } else {
                // From the read after the mailbox was dropped: a few reads before the person
                // types, a few while their code stands in the field, then their own press of
                // `Verify email address` has taken the page on to the consent page.
                let first = *dropped_at.get_or_insert(log.reads);
                match log.reads - first {
                    0..=2 => code_page(false),
                    3..=6 => code_page(true),
                    _ => consent_page(
                        "Claude Code would like to connect. Logged in as one@example.com",
                    ),
                }
            })
        };
        let mut desk = Fake::new(page).with_mail(mail);
        assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered), "{reads}");
        assert_eq!(desk.log.polls, reads, "a dropped mailbox is read no more");
        assert_eq!(codes(&desk.log), 0, "aterm set no code");
        assert!(desk.log.links.is_empty(), "a foreign link is never opened");
        assert_eq!(
            desk.log.pressed,
            [Button::ContinueWithEmail, Button::Authorize],
            "Verify email address over a typed code is the person's"
        );
        let said = desk.log.said.join("\n");
        assert!(said.contains("the window no longer reads it"), "{said}");
        assert!(
            said.contains("a person may type it in the window"),
            "{said}"
        );
        assert!(said.contains("a value aterm did not set"), "{said}");
        assert_eq!(desk.log.closed, 1);
    }
}

/// A CODE THE FIELD DID NOT KEEP IS SET AGAIN FROM THE ONE READ THAT GAVE IT: the mailbox gives
/// each code once, so the flow holds it until the page takes it and never asks the mailbox for
/// it again. A set that did not happen because the window was not aterm's at that moment is
/// tried again and is not counted against the code.
#[test]
fn a_code_that_did_not_take_is_set_again_without_reading_the_mailbox_again() {
    let mut desk = Fake::new(whole_flow).with_mail(|_| Ok(MailboxCredential::Otp("123456".into())));
    desk.code_fills = vec![Filled::NotKept, Filled::NotKept, Filled::Kept];
    // The code page shows its field filled only once the third set kept it.
    desk.page = Box::new(|log: &Log| {
        Ok(if log.pressed.contains(&Button::Authorize) {
            at(ANSWERED)
        } else if log.pressed.contains(&Button::VerifyEmailAddress) {
            consent_page("Logged in as one@example.com")
        } else if log.pressed.contains(&Button::ContinueWithEmail) {
            code_page(codes(log) >= 3)
        } else {
            login_page()
        })
    });
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
    assert_eq!(desk.log.polls, 1, "the mailbox gives a code once");
    assert_eq!(codes(&desk.log), 3);
    assert!(desk.log.pressed.contains(&Button::VerifyEmailAddress));
}

/// THE ACCOUNT'S EMAIL IS IN THE FIELD BEFORE `Continue with email` IS PRESSED: it is set and
/// read back, so the login code is mailed to the account and to no address a page filled in by
/// itself. A field that does not keep it, or a page with no email field, ends the sign-in with
/// that reason and the button unpressed; an email that cannot be a field's value opens nothing.
#[test]
fn continue_with_email_is_pressed_only_over_the_accounts_own_email() {
    let mut desk = Fake::new(|_: &Log| Ok(login_page()));
    desk.email_fills = vec![Filled::NotKept];
    let stop = run(AUTHORIZE, &mut desk).unwrap_err();
    assert_eq!(stop, Stop::Email("it did not keep the account's email"));
    assert!(desk.log.pressed.is_empty());
    assert_eq!(desk.log.filled.len(), MAX_FILLS);
    assert!(!stop.to_string().contains(EMAIL));

    let mut desk = Fake::new(|_: &Log| Ok(login_page()));
    desk.email_fills = vec![Filled::NoField];
    assert_eq!(
        run(AUTHORIZE, &mut desk),
        Err(Stop::Email(
            "the page shows Continue with email and no email field"
        ))
    );
    assert!(desk.log.pressed.is_empty());

    // A set that did not take is tried again, and the press follows the one kept.
    let mut desk = Fake::new(whole_flow).with_mail(|_| Ok(MailboxCredential::Otp("123456".into())));
    desk.email_fills = vec![Filled::NotKept, Filled::NotKept, Filled::Kept];
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
    assert_eq!(desk.log.filled[2], (Field::Email, EMAIL.to_owned()));
    assert_eq!(desk.log.pressed[0], Button::ContinueWithEmail);

    for unusable in [
        "",
        "no-at-sign",
        "a b@example.com",
        "a\"b@example.com",
        "a@b",
    ] {
        let flow = MailboxFlow::new(AUTHORIZE).unwrap();
        let mut desk = Fake::new(|_: &Log| Ok(login_page()));
        let stop = drive(
            &SignIn {
                authorize_url: AUTHORIZE,
                flow: &flow,
                email: unusable,
                within: WITHIN,
            },
            &mut desk,
        )
        .unwrap_err();
        assert!(matches!(stop, Stop::Email(_)), "{unusable:?}");
        assert!(desk.log.opened.is_empty(), "{unusable:?}");
    }
}

/// `Authorize` IS PRESSED ONLY FOR AN ACCOUNT THAT IS ESTABLISHED. A consent page reached with
/// no sign-in of this flow's own is a standing session: one that names another address is
/// another account, and one that names no address in a store nothing recorded as the account's
/// is nobody's this flow can say; both end the sign-in with nothing pressed and nothing signed
/// out, and neither grant is ever issued. CONTROLS: a page that names the account is
/// authorized, and so is a page that names no address once this flow signed the store in
/// itself, in this run or in an earlier one it recorded (the account's token).
#[test]
fn a_consent_page_is_authorized_only_for_an_established_account() {
    let page = |text: &'static str| {
        move |log: &Log| {
            Ok(if log.pressed.contains(&Button::Authorize) {
                at(ANSWERED)
            } else {
                consent_page(text)
            })
        }
    };
    let mut desk = Fake::new(page("Logged in as other@example.com. Not you?"));
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::WrongAccount));
    assert!(desk.log.pressed.is_empty());
    assert_eq!(desk.log.closed, 1);
    assert!(Stop::WrongAccount.to_string().contains("another account"));

    // Another address is another account whatever words stand beside it.
    let mut desk = Fake::new(page("Claude Code would like to connect. two@example.com"));
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::WrongAccount));
    assert!(desk.log.pressed.is_empty());

    // A standing session whose page names nobody.
    let mut desk = Fake::new(page(
        "Claude Code would like to connect to your Claude account",
    ));
    let stop = run(AUTHORIZE, &mut desk).unwrap_err();
    assert_eq!(stop, Stop::UnknownSession);
    assert!(desk.log.pressed.is_empty());
    assert_eq!(desk.log.closed, 1);
    let words = stop.to_string();
    assert!(words.contains("names no account"), "{words}");

    // The same page in a store an earlier sign-in of this flow's recorded as the account's.
    let mut desk = Fake::new(page(
        "Claude Code would like to connect to your Claude account",
    ));
    desk.store_is_the_accounts = true;
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
    assert_eq!(desk.log.pressed, [Button::Authorize]);
    assert_eq!(desk.log.noted, 0, "a consent alone signs no store in");
    // Even there, a page that names another address is another account.
    let mut desk = Fake::new(page("Logged in as other@example.com"));
    desk.store_is_the_accounts = true;
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::WrongAccount));
    assert!(desk.log.pressed.is_empty());

    let mut desk = Fake::new(page("Logged in as One@Example.com"));
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
    assert_eq!(desk.log.pressed, [Button::Authorize]);

    // This flow signed the store in: its own consent page may name nobody.
    let signed_in_here = |log: &Log| {
        Ok(if log.pressed.contains(&Button::Authorize) {
            at(ANSWERED)
        } else if log.pressed.contains(&Button::VerifyEmailAddress) {
            consent_page("Claude Code would like to connect to your Claude account")
        } else {
            whole_flow(log)?
        })
    };
    let mut desk =
        Fake::new(signed_in_here).with_mail(|_| Ok(MailboxCredential::Otp("123456".into())));
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
    assert_eq!(desk.log.pressed.last(), Some(&Button::Authorize));

    // Even then, a page that names another address is another account.
    let other_after = |log: &Log| {
        Ok(if log.pressed.contains(&Button::VerifyEmailAddress) {
            consent_page("Logged in as two@example.com")
        } else {
            whole_flow(log)?
        })
    };
    let mut desk =
        Fake::new(other_after).with_mail(|_| Ok(MailboxCredential::Otp("123456".into())));
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::WrongAccount));
    assert!(!desk.log.pressed.contains(&Button::Authorize));
}

/// CLOUDFLARE'S CHECK IS WAITED ON AND NEVER OPERATED: while it stands nothing is pressed,
/// filled or opened, whatever its page shows, and the progress words say aterm is waiting and
/// that a person may complete it; once it clears the sign-in goes on.
#[test]
fn a_bot_check_that_clears_is_waited_on_with_nothing_pressed() {
    let pressed_during = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = pressed_during.clone();
    let mut desk = Fake::new(move |log: &Log| {
        if log.reads <= 20 {
            seen.set(log.pressed.len() + log.filled.len() + log.went.len());
            Ok(bot_check_page())
        } else {
            whole_flow(log)
        }
    })
    .with_mail(|_| Ok(MailboxCredential::Otp("123456".into())));
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
    assert_eq!(pressed_during.get(), 0);
    assert_eq!(desk.log.pressed.len(), 3);
    let said = desk.log.said.join("\n");
    assert!(said.contains("Cloudflare's check"), "{said}");
    assert!(said.contains("never operates it"), "{said}");
    assert!(
        said.contains("a person at this Mac may complete it"),
        "{said}"
    );
}

/// A CHECK THAT NEVER CLEARS ENDS THE SIGN-IN NAMING CLOUDFLARE, at the deadline, with nothing
/// pressed and the window closed. The interstitial's title alone is enough to wait on it.
#[test]
fn a_bot_check_that_never_clears_ends_naming_cloudflare() {
    for page in [
        bot_check_page(),
        Facts {
            title: "Just a moment...".into(),
            ..with_buttons(LOGIN, &["Continue with email"])
        },
    ] {
        let mut desk = Fake::new(move |_: &Log| Ok(page.clone()));
        let began = desk.now;
        let stop = run(AUTHORIZE, &mut desk).unwrap_err();
        assert_eq!(stop, Stop::BotCheck(Some("claude.ai".into())));
        assert!(desk.now - began >= WITHIN);
        assert!(desk.log.pressed.is_empty() && desk.log.filled.is_empty());
        assert_eq!(desk.log.closed, 1);
        let words = stop.to_string();
        assert!(words.contains("Cloudflare"), "{words}");
        assert_eq!(
            VisibleStop::from(stop),
            VisibleStop::BotCheck(Some("claude.ai".into()))
        );
    }
    // Its words in a help page, with none of its marks, are no check.
    let mut desk = Fake::new(|_: &Log| {
        Ok(Facts {
            text: "If you see Verify you are human, wait.".into(),
            ..login_page()
        })
    });
    assert!(matches!(
        run(AUTHORIZE, &mut desk),
        Err(Stop::TimedOut(Phase::EmailSent))
    ));
    assert_eq!(desk.log.pressed, [Button::ContinueWithEmail]);
}

/// A consent page whose `Authorize` a human check holds: the button is there and disabled.
fn held_consent_page(text: &str) -> Facts {
    Facts {
        text: text.to_owned(),
        disabled: vec!["Authorize".into()],
        check: Some("hCaptcha".into()),
        ..with_buttons(CONSENT, &["Decline"])
    }
}

/// THE CONSENT PAGE'S HUMAN CHECK IS WAITED ON AND NEVER OPERATED: while it holds `Authorize`
/// nothing is pressed or set, and one that stands until the deadline ends the sign-in naming
/// it, for a person at this Mac. Once the page itself enables `Authorize` (a person completed
/// the check) it is pressed, once. A held page that names another account ends at once.
#[test]
fn a_human_check_on_the_consent_page_is_waited_on_and_never_operated() {
    const NAMED: &str = "Claude Code would like to connect. Logged in as one@example.com";
    let mut desk = Fake::new(|_: &Log| Ok(held_consent_page(NAMED)));
    let began = desk.now;
    let stop = run(AUTHORIZE, &mut desk).unwrap_err();
    assert_eq!(stop, Stop::HumanCheck(Some("claude.ai".into())));
    assert!(desk.now - began >= WITHIN);
    assert!(desk.log.pressed.is_empty() && desk.log.filled.is_empty());
    assert!(desk.log.went.is_empty() && desk.log.links.is_empty());
    assert_eq!(desk.log.closed, 1);
    let words = stop.to_string();
    assert!(
        words.contains("human check") && words.contains("never operates"),
        "{words}"
    );
    assert_eq!(
        desk.log
            .said
            .iter()
            .filter(|line| line.contains("human check"))
            .count(),
        1,
        "{:?}",
        desk.log.said
    );
    assert_eq!(
        VisibleStop::from(stop),
        VisibleStop::HumanCheck(Some("claude.ai".into()))
    );

    // A person completes the check at the fortieth read: the page enables Authorize.
    let mut desk = Fake::new(|log: &Log| {
        Ok(if log.pressed.contains(&Button::Authorize) {
            at(ANSWERED)
        } else if log.reads < 40 {
            held_consent_page(NAMED)
        } else {
            consent_page(NAMED)
        })
    });
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
    assert_eq!(desk.log.pressed, [Button::Authorize]);

    let mut desk = Fake::new(|_: &Log| {
        Ok(held_consent_page(
            "Claude Code would like to connect. Logged in as other@example.com",
        ))
    });
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::WrongAccount));
    assert!(desk.log.pressed.is_empty());
    assert_eq!(desk.log.reads, 1);

    // A disabled Authorize beside no check is an ordinary page that is not ready yet.
    let mut desk = Fake::new(|_: &Log| {
        Ok(Facts {
            check: None,
            ..held_consent_page(NAMED)
        })
    });
    assert_eq!(
        run(AUTHORIZE, &mut desk),
        Err(Stop::TimedOut(Phase::Waiting))
    );
}

/// CLOUDFLARE'S BLOCK PAGE ENDS IT AT ONCE, named.
#[test]
fn a_block_page_ends_the_sign_in_at_once() {
    let mut desk = Fake::new(|_: &Log| {
        Ok(Facts {
            text: "Sorry, you have been blocked. Cloudflare Ray ID".into(),
            ..at(LOGIN)
        })
    });
    let stop = run(AUTHORIZE, &mut desk).unwrap_err();
    assert_eq!(stop, Stop::BlockPage(Some("claude.ai".into())));
    assert_eq!(desk.log.reads, 1);
    assert_eq!(desk.log.closed, 1);
    assert_eq!(
        VisibleStop::from(stop),
        VisibleStop::BlockPage(Some("claude.ai".into()))
    );
}

/// A SESSION WITH NO DESKTOP OPENS NO WINDOW AND SAYS SO: the window's host reports it at the
/// open, the sign-in ends with that word for the opener's record, and nothing is read or
/// pressed.
#[test]
fn a_session_with_no_desktop_ends_with_its_own_word() {
    let mut desk = Fake::new(|_: &Log| Ok(login_page()));
    desk.open = Err(DeskError::NoDesktop);
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::NoDesktop));
    assert_eq!(desk.log.reads, 0);
    assert!(desk.log.pressed.is_empty());
    assert_eq!(desk.log.closed, 1);
    assert_eq!(VisibleStop::from(Stop::NoDesktop), VisibleStop::NoDesktop);
    assert_eq!(
        Stop::NoDesktop.to_string(),
        super::super::headless_oauth::WINDOW_NO_DESKTOP
    );
    assert!(Stop::NoDesktop.to_string().contains("no desktop"));
    // Cloudflare's pages keep their host for the record; any other stop is its words.
    assert_eq!(
        VisibleStop::from(Stop::BotCheck(Some("claude.ai".into()))),
        VisibleStop::BotCheck(Some("claude.ai".into()))
    );
    // A window a person closed and another account's consent page are their own words.
    assert_eq!(VisibleStop::from(Stop::Closed), VisibleStop::Closed);
    assert_eq!(
        VisibleStop::from(Stop::WrongAccount),
        VisibleStop::WrongAccount
    );
    assert_eq!(
        Stop::Closed.to_string(),
        super::super::headless_oauth::WINDOW_CLOSED
    );
    // A window that never opened is an ordinary failure, with why, and no window a person
    // closed.
    let mut desk = Fake::new(|_: &Log| Ok(login_page()));
    desk.open = Err(DeskError::NoWindow("its host could not be started".into()));
    let stop = run(AUTHORIZE, &mut desk).unwrap_err();
    assert_eq!(stop, Stop::NoWindow("its host could not be started".into()));
    let VisibleStop::Failed(why) = VisibleStop::from(stop) else {
        panic!("an ordinary failure");
    };
    assert_eq!(
        why,
        "the sign-in window was not opened: its host could not be started"
    );
}

/// A SIGN-IN THAT RUNS OUT OF TIME SAYS WHERE IT STOOD, in aterm's own words and none of the
/// page's (its button titles are the page's text), and no button is pressed twice: a page that
/// keeps `Continue with email` after it was pressed is left alone. THE ONE DEADLINE COVERS THE
/// OPENING OF THE WINDOW: a window that took two minutes to open leaves the sign-in two more.
#[test]
fn a_timeout_names_where_it_stood_and_no_button_is_pressed_twice() {
    let mut desk = Fake::new(|_: &Log| Ok(with_buttons(LOGIN, &["Log in with SSO", "Help"])));
    desk.open_takes = Duration::from_secs(120);
    let began = desk.now;
    let stop = run(AUTHORIZE, &mut desk).unwrap_err();
    assert_eq!(stop, Stop::TimedOut(Phase::Waiting));
    assert!(desk.now - began >= WITHIN && desk.now - began < WITHIN + TICK * 2);
    let words = stop.to_string();
    assert!(words.contains("not completed in 240 s"), "{words}");
    assert!(!words.contains("SSO") && !words.contains("Help"), "{words}");
    assert_eq!(desk.log.closed, 1);
    assert_eq!(
        VisibleStop::from(stop),
        VisibleStop::Failed(words),
        "an ordinary failure carries its words"
    );

    let mut desk = Fake::new(|_: &Log| Ok(login_page()));
    assert_eq!(
        run(AUTHORIZE, &mut desk),
        Err(Stop::TimedOut(Phase::EmailSent))
    );
    assert_eq!(desk.log.pressed, [Button::ContinueWithEmail]);

    // A button that is not there to press is not counted as pressed, and is tried again.
    let mut desk = Fake::new(|_: &Log| Ok(login_page()));
    desk.absent = vec![Button::ContinueWithEmail];
    assert_eq!(
        run(AUTHORIZE, &mut desk),
        Err(Stop::TimedOut(Phase::Waiting))
    );
    assert!(desk.log.pressed.is_empty());
}

/// THE LOOPBACK CALLBACK IS THE PAGE'S OWN TO DELIVER: a page seen at the callback is waited on
/// until the listener's answer moves it on, and is never returned for a second delivery. One
/// that stays there was not answered. A local address that is not this flow's callback is
/// refused. A PAGE THAT LEFT THE SIGN-IN'S PAGES ENDS IT WITH NO PLACE KEPT: neither its host
/// nor its path reaches a report, and the window, which is aterm's own, is closed.
#[test]
fn a_loopback_callback_is_never_delivered_twice() {
    let mut desk = Fake::new(|log: &Log| {
        Ok(if log.reads < 4 {
            at(CALLBACK)
        } else {
            at("https://platform.claude.com/settings")
        })
    });
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));

    let mut desk = Fake::new(|_: &Log| Ok(at(CALLBACK)));
    let began = desk.now;
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::ListenerSilent));
    assert!(desk.now - began >= CALLBACK_SETTLE && desk.now - began < WITHIN);

    let mut desk = Fake::new(|_: &Log| Ok(at("http://localhost:9999/callback?code=x&state=state")));
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::InvalidCallback));

    let mut desk = Fake::new(|_: &Log| Ok(at("https://example.invalid/login?code=secret")));
    let stop = run(AUTHORIZE, &mut desk).unwrap_err();
    assert_eq!(stop, Stop::LeftFlow);
    let words = stop.to_string();
    for kept in ["secret", "example", "login"] {
        assert!(!words.contains(kept), "{words}");
    }
    assert_eq!(desk.log.closed, 1);
    let VisibleStop::Failed(why) = VisibleStop::from(stop) else {
        panic!("an ordinary failure");
    };
    assert_eq!(why, words);

    // The listener's host with no callback seen before it is no answer.
    let mut desk = Fake::new(|_: &Log| Ok(at("https://platform.claude.com/settings")));
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::LeftFlow));
}

/// A CLOSED WINDOW, A CANCELLED SCOPE AND A DESK THAT STOPS ANSWERING each end the sign-in with
/// their own reason; a few failed reads in a row do not.
#[test]
fn a_closed_window_a_cancellation_and_an_unreadable_desk_end_the_sign_in() {
    let mut desk = Fake::new(|_: &Log| Err(DeskError::Gone));
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::Closed));
    // A host that ended without saying its window closed failed: nobody closed anything.
    let mut desk = Fake::new(|_: &Log| Err(DeskError::Ended));
    let stop = run(AUTHORIZE, &mut desk).unwrap_err();
    assert_eq!(stop, Stop::Desk("its host ended".into()));
    assert!(matches!(VisibleStop::from(stop), VisibleStop::Failed(_)));

    let mut desk = Fake::new(|_: &Log| Ok(at(LOGIN)));
    desk.cancel_after = Some(3);
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::Cancelled));
    assert_eq!(desk.log.closed, 1);
    let mut desk = Fake::new(|_: &Log| Ok(at(LOGIN)));
    desk.cancel_after = Some(0);
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::Cancelled));
    assert!(desk.log.opened.is_empty());

    let mut desk = Fake::new(|log: &Log| {
        if log.reads < MAX_UNREAD {
            Err(DeskError::Unread("the page was busy".into()))
        } else {
            Ok(at(ANSWERED))
        }
    });
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
    let mut desk = Fake::new(|_: &Log| Err(DeskError::Unread("the page was busy".into())));
    assert_eq!(
        run(AUTHORIZE, &mut desk),
        Err(Stop::Desk("the page was busy".into()))
    );
    assert_eq!(desk.log.reads, MAX_UNREAD);
}

/// THE CODE IS VERIFIED ONLY ONCE IT IS IN THE FIELD: a code that did not take is set again a
/// bounded number of times and `Verify email address` is never pressed over an empty field; a
/// code page with no field for a code ends the sign-in; with no mailbox configured the page is
/// left for a person, whose own code is then verified.
#[test]
fn a_code_is_verified_only_once_the_field_holds_it() {
    let mut desk = Fake::new(|log: &Log| {
        Ok(if log.pressed.contains(&Button::ContinueWithEmail) {
            code_page(false)
        } else {
            login_page()
        })
    })
    .with_mail(|_| Ok(MailboxCredential::Otp("123456".into())));
    desk.code_fills = vec![Filled::NotKept];
    assert_eq!(
        run(AUTHORIZE, &mut desk),
        Err(Stop::TimedOut(Phase::CodeNotKept))
    );
    assert_eq!(codes(&desk.log), MAX_FILLS);
    assert_eq!(desk.log.polls, 1);
    assert_eq!(desk.log.pressed, [Button::ContinueWithEmail]);

    let mut desk = Fake::new(|_: &Log| Ok(with_buttons(LOGIN, &["Verify email address"])))
        .with_mail(|_| Ok(MailboxCredential::Otp("123456".into())));
    desk.code_fills = vec![Filled::NoField];
    assert!(matches!(run(AUTHORIZE, &mut desk), Err(Stop::Desk(_))));
    assert!(desk.log.pressed.is_empty());

    // A person types the code: the field holds a value aterm did not set, and `Verify email
    // address` is theirs to press. aterm presses nothing over it, and goes on when the page
    // does.
    let typed = |log: &Log| {
        Ok(if log.reads > 12 {
            at(ANSWERED)
        } else {
            code_page(log.reads > 6)
        })
    };
    let mut desk = Fake::new(typed);
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
    assert!(desk.log.pressed.is_empty(), "{:?}", desk.log.pressed);
    assert_eq!(desk.log.polls, 0);
    let said = desk.log.said.join("\n");
    assert!(
        said.contains("this account has no mailbox the window can read"),
        "{said}"
    );
    assert!(said.contains("a value aterm did not set"), "{said}");

    // The same with a mailbox: a first digit typed before aterm's code came is not verified.
    let mut desk = Fake::new(|_: &Log| Ok(code_page(true)))
        .with_mail(|_| Ok(MailboxCredential::Otp("123456".into())));
    assert_eq!(
        run(AUTHORIZE, &mut desk),
        Err(Stop::TimedOut(Phase::CodeTyped))
    );
    assert!(desk.log.pressed.is_empty());
    assert_eq!(desk.log.polls, 0);
}

/// A PAGE READ IS PARSED WHOLE OR REFUSED: the script's one JSON answer gives the document's
/// own address, the title, the text, Cloudflare's marks (an unknown word is no mark), the
/// buttons and the code field's state; the address the web view gave is kept apart, and no
/// decision about the page's content reads it. A document that is not the flow's own answers
/// its address alone. Anything else is an unread page, never a page with defaults.
#[test]
fn a_page_read_is_parsed_whole_or_refused() {
    let answer = r#"{"at":"https://claude.ai/login","title":"Sign in - Claude","text":"Welcome","markers":["cf-turnstile","not-a-marker"],"buttons":["Continue with email","Continue with Google"],"code":"empty"}"#;
    assert_eq!(
        parse_facts(answer, CONSENT),
        Ok(Facts {
            url: LOGIN.into(),
            active: CONSENT.into(),
            load_failed: None,
            title: "Sign in - Claude".into(),
            text: "Welcome".into(),
            markers: vec![Marker::Turnstile],
            buttons: vec!["Continue with email".into(), "Continue with Google".into()],
            disabled: Vec::new(),
            check: None,
            code: Some(false),
        })
    );
    // A consent page's read: its disabled buttons and the human check it carries, by name.
    let held = r#"{"at":"https://claude.ai/oauth/authorize?client_id=claude","title":"Claude","text":"Authorize Decline","markers":[],"buttons":["Decline"],"disabled":["Authorize"],"check":"hCaptcha","code":null}"#;
    let held = parse_facts(held, CONSENT).unwrap();
    assert_eq!(held.disabled, ["Authorize"]);
    assert_eq!(held.check.as_deref(), Some("hCaptcha"));
    assert_eq!(held.url, CONSENT);
    let filled = r#"{"at":"https://claude.ai/login","title":"","text":"","markers":[],"buttons":[],"code":"filled"}"#;
    assert_eq!(parse_facts(filled, LOGIN).unwrap().code, Some(true));
    // The empty page a web view starts on has no address.
    let none = r#"{"at":"about:blank","title":"","text":"","markers":[],"buttons":[],"code":null}"#;
    assert_eq!(parse_facts(none, "").unwrap(), Facts::default());
    // A document that is not the flow's own: its address, and nothing of its content, even
    // where the web view says it is at a page of the flow's.
    let foreign = r#"{"at":"https://example.invalid/login","foreign":true,"title":"Sign in","buttons":["Authorize"]}"#;
    assert_eq!(
        parse_facts(foreign, LOGIN),
        Ok(Facts {
            url: "https://example.invalid/login".into(),
            active: LOGIN.into(),
            ..Facts::default()
        })
    );
    for unread in [
        "",
        "pressed",
        "{}",
        r#"{"title":"x","text":"y","markers":[],"buttons":[],"code":null}"#,
        r#"{"at":"https://claude.ai/","title":"x","text":"y","markers":[],"buttons":[7],"code":null}"#,
        r#"{"at":"https://claude.ai/","title":"x","text":"y","markers":"cf-turnstile","buttons":[],"code":null}"#,
        r#"{"at":"https://claude.ai/","title":"x","markers":[],"buttons":[],"code":null}"#,
    ] {
        assert!(
            matches!(parse_facts(unread, LOGIN), Err(DeskError::Unread(_))),
            "{unread}"
        );
    }
}

/// WHAT A SCRIPT IS GIVEN IS A STRING LITERAL, never script text: a value with a quote, a
/// backslash or a line end stays inside its literal. THE READ SCRIPT ACTS ON NOTHING: it holds
/// no click and no value set. EVERY SCRIPT TESTS ITS OWN DOCUMENT FIRST: one that is not an
/// https page of the flow's hosts is answered with its address and nothing read, pressed or
/// set, and a press or a set made for another document than the one that runs it does
/// nothing. EVERY SCRIPT STOPS AT CLOUDFLARE'S PAGE before it reads a control.
#[test]
fn script_values_are_literals_and_every_script_acts_only_in_its_own_document() {
    assert_eq!(
        literal("a\"b\\c\n</script>").unwrap(),
        r#""a\"b\\c\n</script>""#
    );
    let fill = fill_script(Field::Code, "12\"; alert(1); \"", LOGIN, PINNED).unwrap();
    assert!(
        fill.contains(r#"const value = "12\"; alert(1); \"";"#),
        "{fill}"
    );
    // The hosts and the expected address are literals too.
    let odd = press_script(
        Button::Authorize,
        EMAIL,
        "https://claude.ai/\"; x(); \"",
        PINNED,
    )
    .unwrap();
    assert!(
        odd.contains(r#"if (here !== "https://claude.ai/\"; x(); \"") return answer("moved");"#),
        "{odd}"
    );
    assert!(
        odd.contains(r#"const pinned = ["claude.ai","claude.com"];"#),
        "{odd}"
    );
    assert!(
        odd.contains(
            r#"const own = location.protocol === "https:" && pinned.includes(String(location.hostname).toLowerCase());"#
        ),
        "{odd}"
    );

    let read = facts_script(PINNED).unwrap();
    assert!(!read.contains(".click("), "the read presses nothing");
    assert!(!read.contains("setValue(field") && !read.contains("setValue(fields"));
    let foreign = read
        .find("if (!own) return JSON.stringify({ at: here, foreign: true });")
        .expect("the read's own-document test");
    assert!(foreign < read.find("document.title").expect("the read's title"));
    let at_check = read
        .find("if (challenged()) return")
        .expect("the read's stop");
    assert!(foreign < at_check);
    assert!(
        at_check
            < read
                .find("enabledButtons().map")
                .expect("the read's buttons")
    );

    for button in [
        Button::ContinueWithEmail,
        Button::VerifyEmailAddress,
        Button::Authorize,
    ] {
        let press = press_script(button, EMAIL, LOGIN, PINNED).unwrap();
        let foreign = press
            .find(r#"if (!own) return answer("foreign");"#)
            .expect("its own-document test");
        let moved = press
            .find(r#"if (here !== "https://claude.ai/login") return answer("moved");"#)
            .expect("its same-document test");
        let stop = press
            .find(r#"if (challenged()) return answer("unready");"#)
            .expect("its stop");
        let click = press.find("hit.click()").expect("its press");
        assert!(
            foreign < moved && moved < stop && stop < click,
            "{button:?}"
        );
        // The whole text, lowercase: never a part of a longer button's.
        assert!(press.contains(&format!(
            "const title = \"{}\"",
            button.title().to_ascii_lowercase()
        )));
        assert!(press.contains("labelOf(b).toLowerCase() === title"));
        assert_eq!(press.matches(".click()").count(), 1);
    }
    // `Continue with email` is pressed only over the account's own email, read at the press.
    let press = press_script(Button::ContinueWithEmail, "One@Example.com", LOGIN, PINNED).unwrap();
    assert!(press.contains(r#"email = "one@example.com""#), "{press}");
    assert!(
        press_script(Button::Authorize, EMAIL, CONSENT, PINNED)
            .unwrap()
            .contains("email = null")
    );
    assert!(
        press_script(Button::VerifyEmailAddress, EMAIL, LOGIN, PINNED)
            .unwrap()
            .contains("code = true")
    );
    assert!(
        press_script(Button::Authorize, EMAIL, CONSENT, PINNED)
            .unwrap()
            .contains("code = false")
    );
    for field in [Field::Email, Field::Code] {
        let fill = fill_script(field, "123456", LOGIN, PINNED).unwrap();
        let foreign = fill
            .find(r#"if (!own) return answer("foreign");"#)
            .expect("its own-document test");
        let moved = fill
            .find(r#"if (here !== "https://claude.ai/login") return answer("moved");"#)
            .expect("its same-document test");
        let stop = fill
            .find(r#"if (challenged()) return answer("unready");"#)
            .expect("its stop");
        assert!(foreign < moved && moved < stop);
        // Every set follows the stop.
        let uses: Vec<usize> = fill.match_indices("setValue(").map(|(at, _)| at).collect();
        assert!(
            !uses.is_empty() && uses.iter().all(|at| *at > stop),
            "{field:?}"
        );
        assert!(!fill.contains(".click("), "a fill presses nothing");
    }
    // What an act answers: what it did, and where the document that ran it is.
    assert_eq!(
        parse_act(r#"{"at":"https://claude.ai/login","did":"pressed"}"#),
        Some(("pressed".to_owned(), LOGIN.to_owned()))
    );
    assert_eq!(parse_act(r#"{"did":"pressed"}"#), None);
    assert_eq!(parse_act("pressed"), None);
}

/// The hosts a loopback sign-in's documents are read and acted in.
const PINNED: &[&str] = &["claude.ai", "claude.com"];

/// A BUTTON IS ITS WHOLE TEXT: white space and letter case apart, never a part of a longer
/// one's.
#[test]
fn a_button_is_matched_by_its_whole_text() {
    assert!(Button::ContinueWithEmail.is("Continue with email"));
    assert!(Button::ContinueWithEmail.is("  CONTINUE   WITH\nEMAIL "));
    assert!(!Button::ContinueWithEmail.is("Continue with email or Google"));
    assert!(!Button::Authorize.is("Authorize all"));
    assert!(!Button::Authorize.is("Don't authorize"));
    assert!(Button::VerifyEmailAddress.is("Verify Email Address"));
}

/// AN ACCOUNT'S STORE IS ITS OWN AND THE SAME EVERY TIME: the identifier is a UUID made of the
/// account's name alone, letter case apart, and two accounts never share one.
#[test]
fn an_accounts_store_is_a_uuid_of_its_own() {
    let one = store_id("claude:one@example.com");
    assert_eq!(one, store_id("claude:One@Example.com "));
    assert_ne!(one, store_id("claude:two@example.com"));
    let groups: Vec<&str> = one.split('-').collect();
    assert_eq!(
        groups.iter().map(|group| group.len()).collect::<Vec<_>>(),
        [8, 4, 4, 4, 12]
    );
    assert!(one.bytes().all(|b| b == b'-' || b.is_ascii_hexdigit()));
    assert!(groups[2].starts_with('8'), "{one}");
    assert!(
        matches!(groups[3].as_bytes()[0], b'8' | b'9' | b'A' | b'B'),
        "{one}"
    );
}

/// THE STORE IS THE ACCOUNT'S ONLY ONCE A SIGN-IN OF THIS FLOW'S SAYS SO, in a private record
/// beside the accounts store that names the account: none before, the account after, and
/// another account's desk over the same record is not it.
#[test]
fn a_store_is_recorded_as_the_accounts_only_by_a_sign_in() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let exe = std::path::PathBuf::from("/nonexistent/aterm");
    let mut desk = Window::new(exe.clone(), Some(dir.path()), None, EMAIL, None);
    assert!(!desk.store_is_the_accounts());
    desk.note_signed_in();
    assert!(desk.store_is_the_accounts());
    let marker = desk.marker.clone().unwrap();
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), EMAIL);
    let other = Window::new(exe.clone(), Some(dir.path()), None, "two@example.com", None);
    assert_ne!(other.marker, desk.marker);
    assert!(!other.store_is_the_accounts());
    // A desk with no records folder has no record, and says no.
    let mut bare = Window::new(exe, None, None, EMAIL, None);
    bare.note_signed_in();
    assert!(!bare.store_is_the_accounts());
    assert_eq!(desk.title, "aterm \u{b7} signing in Claude one@example.com");
}

/// A STORE WHOSE SIGN-IN TURNED OUT TO BE ANOTHER ACCOUNT'S IS NO LONGER TAKEN FOR THE
/// ACCOUNT'S: a renewal that found another account's login takes the record back
/// ([`disown_store`]), so the next consent page there is authorized only where it names the
/// account. Another account's record stands.
#[test]
fn a_store_that_signed_another_account_in_is_no_longer_recorded_as_the_accounts() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let exe = std::path::PathBuf::from("/nonexistent/aterm");
    let mut desk = Window::new(exe.clone(), Some(dir.path()), None, EMAIL, None);
    let mut other = Window::new(exe, Some(dir.path()), None, "two@example.com", None);
    desk.note_signed_in();
    other.note_signed_in();
    disown_store(dir.path(), " One@Example.com ");
    assert!(!desk.store_is_the_accounts());
    assert!(other.store_is_the_accounts(), "another account's stands");
    // With no record there is nothing to take back.
    disown_store(dir.path(), EMAIL);
    assert!(!desk.store_is_the_accounts());
}

/// WHAT THE TRACE KEEPS OF A PAGE names no address.
#[test]
fn a_traced_line_names_no_address() {
    assert_eq!(
        traced("Logged in as one@example.com  now"),
        "Logged in as <address> now"
    );
    assert_eq!(
        traced_text("Signed in as one@example.com\ncode 246810 or 24-68 ok"),
        "Signed in as <address> code <n> or <n> ok"
    );
}

/// A mailbox only a headless browser reads is none of this desk's: its webmail page opens in no
/// read made here, so the flow says the code is the person's to type instead of asking the
/// mailbox for four minutes. CONTROLS: a local folder, an IMAP mailbox and Proton's mailbox
/// with its password are read here.
#[test]
fn a_webmail_only_mailbox_is_not_one_the_desk_reads() {
    let webmail = MailboxConfig {
        webmail_url: Some("https://mail.example.com/inbox".into()),
        user: Some(EMAIL.into()),
        password: Some("word".into()),
        ..MailboxConfig::default()
    };
    assert!(!webmail.reads_without_browser(Some(EMAIL), &no_grant));
    let exe = std::path::PathBuf::from("/nonexistent/aterm");
    assert!(!Window::new(exe.clone(), None, Some(&webmail), EMAIL, None).has_mailbox());
    let folder = MailboxConfig {
        dir: Some(std::path::PathBuf::from("/nonexistent/mailbox")),
        ..MailboxConfig::default()
    };
    assert!(folder.reads_without_browser(Some(EMAIL), &no_grant));
    assert!(Window::new(exe, None, Some(&folder), EMAIL, None).has_mailbox());
    let imap = MailboxConfig {
        imap_url: Some("imaps://one%40example.com:word@imap.example.com".into()),
        ..MailboxConfig::default()
    };
    assert!(imap.reads_without_browser(Some(EMAIL), &no_grant)); // Proton's mailbox is read through its own API, with no browser, where it has a login
    // and a password; without the password only its page could be signed in to.
    let proton = MailboxConfig {
        webmail_url: Some("https://mail.proton.me/u/0/inbox".into()),
        user: Some(EMAIL.into()),
        password: Some("word".into()),
        ..MailboxConfig::default()
    };
    #[cfg(unix)]
    assert!(proton.reads_without_browser(Some(EMAIL), &no_grant));
    let no_password = MailboxConfig {
        password: None,
        ..proton
    };
    assert!(!no_password.reads_without_browser(Some(EMAIL), &no_grant));
}

/// An account's saved Google mail grant, as a test's loader answers it: none is saved.
fn no_grant(_: &str) -> GoogleGrant {
    GoogleGrant::Absent
}

/// A GMAIL ROUTE IS A MAILBOX THE WINDOW READS ONLY WHERE ITS ACCOUNT'S SAVED GOOGLE GRANT
/// LOADS ON THIS MAC: route admission is no proof that the poll reads it. A Gmail address with
/// no grant (its route holds the account's own password for the webmail page, which the poll
/// does not read) is configured and is no mailbox of the window's, so a sign-in nobody watches
/// opens no window for it and the desk says the code is a person's to type; with the
/// account's grant (a stand-in loader, asked for that account alone) it is one. Another
/// account's grant is not this one's. CONTROL: a Gmail IMAP address with an app password in it
/// reads with no grant at all.
#[cfg(unix)]
#[test]
fn a_gmail_route_is_one_the_window_reads_only_where_its_saved_grant_loads() {
    let gmail = MailboxConfig::from_toml_str(
        "[[mailboxes]]\nlogin='fixture.person@gmail.com'\npassword='a fixture word'\n\
         webmail_url='https://mail.google.com/'\nreceives=['fixture.person@gmail.com']\n",
        None,
    )
    .unwrap();
    assert!(gmail.has_native_google_route(Some(GMAIL)));
    assert!(gmail.is_configured_for(Some(GMAIL)));
    assert!(!gmail.reads_without_browser(Some(GMAIL), &no_grant));
    assert!(!gmail.window_reads_with(Some(GMAIL), &no_grant));

    let asked = std::cell::RefCell::new(Vec::new());
    let saved = |account: &str| {
        asked.borrow_mut().push(account.to_owned());
        if account == GMAIL {
            GoogleGrant::Loads
        } else {
            GoogleGrant::Absent
        }
    };
    assert!(gmail.reads_without_browser(Some(GMAIL), &saved));
    assert!(gmail.window_reads_with(Some(GMAIL), &saved));
    assert!(
        asked.borrow().iter().all(|account| account == GMAIL),
        "{:?}",
        asked.borrow()
    );
    let another = |account: &str| {
        if account == "someone.else@gmail.com" {
            GoogleGrant::Loads
        } else {
            GoogleGrant::Absent
        }
    };
    assert!(!gmail.window_reads_with(Some(GMAIL), &another));

    assert!(app_password().window_reads_with(Some(GMAIL), &no_grant));
}

/// The row these tests' Gmail mailboxes are for.
#[cfg(unix)]
const GMAIL: &str = "fixture.person@gmail.com";

/// A Gmail IMAP address with an app password in it, for [`GMAIL`].
#[cfg(unix)]
fn app_password() -> MailboxConfig {
    MailboxConfig {
        imap_url: Some("imaps://fixture.person%40gmail.com:apppw@imap.gmail.com/INBOX".into()),
        ..MailboxConfig::default()
    }
}

/// A SAVED GOOGLE GRANT THAT DOES NOT LOAD READS NOTHING, WHATEVER STANDS BESIDE IT: the poll
/// ends every read on it before it reaches IMAP (`native_mail::plan`'s `Refused`), so an app
/// password beside it does not make the mailbox one the window reads, and a sign-in nobody
/// watches opens no window for it. A Gmail address with neither a grant nor a password (an
/// IMAP address with no password, the OAuth-only shape) reads nothing either. CONTROLS: the
/// same app password with no grant saved reads, and a folder beside a broken grant is read
/// before the Google route, as the poll reads it.
#[cfg(unix)]
#[test]
fn a_gmail_mailbox_the_poll_refuses_is_not_one_the_window_reads() {
    let broken = |_: &str| GoogleGrant::Broken;
    let with_app_password = app_password();
    assert!(with_app_password.has_native_google_route(Some(GMAIL)));
    assert!(!with_app_password.reads_without_browser(Some(GMAIL), &broken));
    assert!(!with_app_password.window_reads_with(Some(GMAIL), &broken));
    assert!(with_app_password.window_reads_with(Some(GMAIL), &no_grant));

    for imap_url in [
        "imaps://imap.gmail.com/INBOX",
        "imaps://fixture.person%40gmail.com@imap.gmail.com/INBOX",
    ] {
        let oauth_only = MailboxConfig {
            imap_url: Some(imap_url.into()),
            ..MailboxConfig::default()
        };
        assert!(
            oauth_only.has_native_google_route(Some(GMAIL)),
            "{imap_url}"
        );
        assert!(oauth_only.is_configured_for(Some(GMAIL)), "{imap_url}");
        assert!(
            oauth_only.effective_imap_url(Some(GMAIL)).is_some(),
            "{imap_url}"
        );
        assert!(
            !oauth_only.window_reads_with(Some(GMAIL), &no_grant),
            "{imap_url}"
        );
        assert!(
            !oauth_only.window_reads_with(Some(GMAIL), &broken),
            "{imap_url}"
        );
        let loads = |_: &str| GoogleGrant::Loads;
        assert!(
            oauth_only.window_reads_with(Some(GMAIL), &loads),
            "{imap_url}"
        );
    }

    let folder_too = MailboxConfig {
        dir: Some(std::path::PathBuf::from("/nonexistent/mailbox")),
        ..app_password()
    };
    assert!(folder_too.window_reads_with(Some(GMAIL), &broken));
}

/// A MAILBOX LOGIN THIS MAC HOLDS AS REFUSED IS NOT ONE THE WINDOW READS UNTIL ITS WAIT ENDS:
/// the poll does not try it (a Gmail route holding the Google account's own password, which
/// Gmail refuses over IMAP, is refused once and then held), so a sign-in nobody watches opens
/// no window for it again and again. The hold is the login's on its source: a saved grant's
/// refusal holds the grant, an IMAP refusal the IMAP login. CONTROLS: before the refusal the
/// route reads; a refusal of another login holds nothing here.
#[cfg(unix)]
#[test]
fn a_mailbox_login_held_as_refused_is_not_one_the_window_reads() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let record = dir
        .path()
        .join(super::super::mailbox_routes::LOGIN_RECORD_FILE);
    let mut account_password = MailboxConfig::from_toml_str(
        "[[mailboxes]]\nlogin='fixture.person@gmail.com'\npassword='a fixture word'\n\
         receives=['fixture.person@gmail.com']\n",
        None,
    )
    .unwrap();
    account_password.login_record = Some(record.clone());
    let source = super::super::mailbox_routes::login_source(
        &account_password
            .routed_for(Some(GMAIL))
            .effective_imap_url(Some(GMAIL))
            .expect("the discovered Gmail address"),
    );
    assert!(account_password.window_reads_with(Some(GMAIL), &no_grant));
    let now = super::super::mailbox_routes::unix_now();
    super::super::mailbox_routes::note_login_refused(
        Some(&record),
        &source,
        "someone.else@gmail.com",
        now,
    );
    assert!(account_password.window_reads_with(Some(GMAIL), &no_grant));
    super::super::mailbox_routes::note_login_refused(Some(&record), &source, GMAIL, now);
    assert!(!account_password.window_reads_with(Some(GMAIL), &no_grant));

    let loads = |_: &str| GoogleGrant::Loads;
    assert!(account_password.window_reads_with(Some(GMAIL), &loads));
    super::super::mailbox_routes::note_login_refused(
        Some(&record),
        "google-mail-oauth",
        GMAIL,
        now,
    );
    assert!(!account_password.window_reads_with(Some(GMAIL), &loads));

    let mut other_imap = MailboxConfig {
        imap_url: Some("imaps://one%40example.com:word@imap.example.com".into()),
        login_record: Some(record.clone()),
        ..MailboxConfig::default()
    };
    assert!(other_imap.window_reads_with(Some(EMAIL), &no_grant));
    super::super::mailbox_routes::note_login_refused(
        Some(&record),
        "imaps://imap.example.com",
        EMAIL,
        now,
    );
    assert!(!other_imap.window_reads_with(Some(EMAIL), &no_grant));
    other_imap.dir = Some(std::path::PathBuf::from("/nonexistent/mailbox"));
    assert!(other_imap.window_reads_with(Some(EMAIL), &no_grant));
}

/// A MAILBOX'S SAVED GOOGLE GRANT IS LOOKED FOR IN THE HOME IT WAS DISCOVERED IN, never the
/// process's: a row's window answer for a scratch home depends on that home alone. A grant
/// file there that does not load (no machine key in that home) makes the row's Gmail mailbox,
/// app password and all, one the window does not read, through the row's own entry point;
/// with no grant file there the app password reads.
#[cfg(unix)]
#[test]
fn a_rows_google_grant_is_read_in_the_home_its_mailbox_was_discovered_in() {
    let home = aterm_tempfile::TempDir::new().unwrap();
    let store_dir = home.path().join("state/accounts");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = super::super::accounts::Store::at(&store_dir);
    let var = |name: &str| {
        (name == super::super::headless_oauth::IMAP_URL_ENV).then(|| {
            std::ffi::OsString::from(
                "imaps://fixture.person%40gmail.com:apppw@imap.gmail.com/INBOX",
            )
        })
    };
    let ask = || {
        super::super::accounts_sync::window_reads_mailbox_for_row_with(
            &var,
            Some(home.path()),
            &store,
            None,
            Some(GMAIL),
        )
    };
    let discovered = super::super::headless_oauth::discover_mailbox_config_with_env(
        Some(home.path()),
        Some(&store_dir),
        var,
    )
    .expect("the IMAP address configures a mailbox");
    assert_eq!(discovered.mail_home.as_deref(), Some(home.path()));
    assert!(ask(), "no grant in this home: the app password reads");

    let google = home.path().join(".config/aterm/mail/google");
    std::fs::create_dir_all(&google).unwrap();
    let name: String = aterm_digest::Sha256::digest(GMAIL.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let grant = google.join(format!("{name}.age"));
    std::fs::write(&grant, b"not a grant").unwrap();
    std::fs::set_permissions(&grant, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    assert_eq!(
        super::super::headless_oauth::google_grant_in(Some(home.path()), GMAIL),
        GoogleGrant::Broken
    );
    assert!(
        !ask(),
        "a grant in this home that does not load reads nothing"
    );
}

/// THE FLEET'S MAIL RELAY IS NO MAILBOX OF THE WINDOW'S: it asks another Mac for the code,
/// with the account's address, and a sign-in in the window never does. A relay-only
/// configuration is configured, is not one the window reads, and leaves its desk with no
/// mailbox; a configuration that holds the relay beside a mailbox read here has the relay
/// turned off before the desk reads anything. CONTROL: the relay does count as configured.
#[test]
fn the_fleets_mail_relay_is_never_the_windows_mailbox() {
    let exe = std::path::PathBuf::from("/nonexistent/aterm");
    let relay_only = MailboxConfig {
        relay_enabled: true,
        relay_home: Some(std::path::PathBuf::from("/nonexistent/home")),
        relay_state_dir: Some(std::path::PathBuf::from("/nonexistent/state")),
        ..MailboxConfig::default()
    };
    assert!(relay_only.is_configured_for(Some(EMAIL)));
    assert!(relay_only.reads_relay_for(Some(EMAIL)));
    assert!(!relay_only.reads_without_browser(Some(EMAIL), &no_grant));
    assert!(!relay_only.window_reads(Some(EMAIL)));
    assert!(!Window::new(exe.clone(), None, Some(&relay_only), EMAIL, None).has_mailbox());

    let mut both = MailboxConfig {
        dir: Some(std::path::PathBuf::from("/nonexistent/mailbox")),
        ..relay_only
    };
    assert!(both.window_reads(Some(EMAIL)));
    both.read_on_this_mac_alone();
    assert!(!both.relay_enabled);
    assert_eq!(both.relay_home, None);
    assert_eq!(both.relay_state_dir, None);
    assert!(!both.reads_relay_for(Some(EMAIL)));
    assert!(both.window_reads(Some(EMAIL)));
    assert!(Window::new(exe, None, Some(&both), EMAIL, None).has_mailbox());
}

/// An address on the consent page that is not the account's is another account, whatever
/// stands beside it; only a page with no address names none.
#[test]
fn the_account_a_consent_page_names_is_read_from_its_addresses() {
    assert_eq!(
        emails_in("Logged in as One@Example.com. (two@example.org), help@anthropic.com x@y"),
        ["one@example.com", "two@example.org"]
    );
    for (text, named) in [
        ("Logged in as one@example.com", Account::Named),
        ("one@example.com and two@example.com", Account::Named),
        ("Logged in as two@example.com", Account::Other),
        ("two@example.com", Account::Other),
        ("Questions? Write to support@example.com", Account::Other),
        ("Logged in. Not you?", Account::Unnamed),
        ("Write to help@anthropic.com", Account::Unnamed),
    ] {
        assert_eq!(account_named(text, EMAIL), named, "{text}");
    }
}

/// A HUMAN CHECK THAT HELD AN EARLIER PAGE IS NOT WHY A LATER ONE RAN OUT OF TIME: each read
/// says anew whether the check holds the page, so a sign-in whose consent page gave way to a
/// page that never loaded ends on that page, and one whose check still stands ends naming it.
#[test]
fn a_human_check_names_the_end_only_while_it_stands() {
    const NAMED: &str = "Claude Code would like to connect. Logged in as one@example.com";
    let mut desk = Fake::new(|log: &Log| {
        Ok(if log.reads < 10 {
            held_consent_page(NAMED)
        } else {
            at("")
        })
    });
    assert_eq!(
        run(AUTHORIZE, &mut desk),
        Err(Stop::TimedOut(Phase::Opening))
    );
    // The same for the callback's branch: the page left the check for the callback, which
    // the listener never answered.
    let mut desk = Fake::new(|log: &Log| {
        Ok(if log.reads < 10 {
            held_consent_page(NAMED)
        } else {
            at(CALLBACK)
        })
    });
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::ListenerSilent));
    // And for a page with none of the flow's buttons.
    let mut desk = Fake::new(|log: &Log| {
        Ok(if log.reads < 10 {
            held_consent_page(NAMED)
        } else {
            with_buttons(LOGIN, &["Help"])
        })
    });
    assert_eq!(
        run(AUTHORIZE, &mut desk),
        Err(Stop::TimedOut(Phase::Waiting))
    );
}

/// A PAGE THAT DID NOT LOAD ENDS THE SIGN-IN AT ONCE, as an ordinary failure with the
/// navigation's code: a Mac that is offline is not held for four minutes and is not a row's
/// six-hour stop. After `Authorize` on a loopback flow the navigation that failed is the
/// callback's, which the waiting sign-in's listener did not answer. CONTROL: a failure
/// beside a page that stands (a navigation the page itself gave up) ends nothing.
#[test]
fn a_page_that_did_not_load_ends_the_sign_in_as_an_ordinary_failure() {
    let mut desk = Fake::new(|_: &Log| {
        Ok(Facts {
            load_failed: Some(-1009),
            ..at("")
        })
    });
    let stop = run(AUTHORIZE, &mut desk).unwrap_err();
    assert_eq!(stop, Stop::Load(-1009));
    assert_eq!(desk.log.reads, 1);
    assert_eq!(desk.log.closed, 1);
    let words = stop.to_string();
    assert!(words.contains("did not load (error -1009)"), "{words}");
    assert_eq!(VisibleStop::from(stop), VisibleStop::Failed(words));

    let mut desk = Fake::new(|log: &Log| {
        Ok(if log.pressed.contains(&Button::Authorize) {
            Facts {
                load_failed: Some(-1004),
                ..consent_page("Logged in as one@example.com")
            }
        } else {
            consent_page("Logged in as one@example.com")
        })
    });
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::ListenerSilent));
    assert_eq!(desk.log.pressed, [Button::Authorize]);

    let mut desk = Fake::new(|log: &Log| {
        Ok(Facts {
            load_failed: (log.reads == 2).then_some(-1001),
            ..whole_flow(log)?
        })
    })
    .with_mail(|_| Ok(MailboxCredential::Otp("123456".into())));
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
}

/// WHAT IS DECIDED ABOUT A PAGE IS DECIDED ON THE DOCUMENT'S OWN ADDRESS: the address the web
/// view is on its way to is read for this flow's callback and a local address alone. A
/// document that is not the flow's own ends the sign-in whatever the web view says it is at,
/// and nothing is pressed in it; a document of the flow's with the web view on its way to
/// the callback is the callback.
#[test]
fn a_page_is_judged_by_its_documents_own_address() {
    // Another origin's document, while the web view names a page of the flow's.
    let mut desk = Fake::new(|_: &Log| {
        Ok(Facts {
            active: LOGIN.into(),
            ..with_buttons("https://example.invalid/login", &["Authorize"])
        })
    });
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::LeftFlow));
    assert!(desk.log.pressed.is_empty() && desk.log.acted_at.is_empty());

    // The consent page stands while the web view goes to the hosted callback.
    let mut desk = Fake::new(|log: &Log| {
        Ok(if log.pressed.contains(&Button::Authorize) {
            Facts {
                active: HOSTED_CALLBACK.into(),
                ..consent_page("Logged in as one@example.com")
            }
        } else {
            consent_page("Logged in as one@example.com")
        })
    });
    assert_eq!(
        run(HOSTED_AUTHORIZE, &mut desk),
        Ok(Done::Callback(HOSTED_CALLBACK.to_owned()))
    );
    assert_eq!(desk.log.pressed, [Button::Authorize]);

    // On its way to a local address that is not this flow's callback.
    let mut desk = Fake::new(|_: &Log| {
        Ok(Facts {
            active: "http://localhost:9999/callback?code=x&state=state".into(),
            ..consent_page("Logged in as one@example.com")
        })
    });
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::InvalidCallback));
    assert!(desk.log.pressed.is_empty());

    // Every press and every set was told the address of the document the read gave.
    let mut desk = Fake::new(whole_flow).with_mail(|_| Ok(MailboxCredential::Otp("123456".into())));
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
    assert_eq!(desk.log.acted_at, [LOGIN, LOGIN, LOGIN, LOGIN, CONSENT]);
}

/// A STORE THAT HOLDS ANOTHER ACCOUNT'S SESSION IS EMPTIED, so it is no dead end: a consent
/// page that names another account, or nobody this flow can name, ends the sign-in and the
/// window's own store is forgotten before the window closes; the next sign-in starts at the
/// sign-in page. CONTROLS: no other end forgets anything.
#[test]
fn a_session_that_is_not_the_accounts_is_forgotten() {
    let consent = |text: &'static str| move |_: &Log| Ok(consent_page(text));
    let mut desk = Fake::new(consent("Logged in as other@example.com"));
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::WrongAccount));
    assert_eq!((desk.log.forgot, desk.log.closed), (1, 1));
    let mut desk = Fake::new(consent("Claude Code would like to connect"));
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::UnknownSession));
    assert_eq!((desk.log.forgot, desk.log.closed), (1, 1));
    for words in [
        Stop::WrongAccount.to_string(),
        Stop::UnknownSession.to_string(),
    ] {
        assert!(words.contains("forgotten"), "{words}");
    }

    let mut desk = Fake::new(|_: &Log| Ok(held_consent_page("Logged in as one@example.com")));
    assert!(matches!(
        run(AUTHORIZE, &mut desk),
        Err(Stop::HumanCheck(_))
    ));
    assert_eq!(desk.log.forgot, 0);
    let mut desk = Fake::new(whole_flow).with_mail(|_| Ok(MailboxCredential::Otp("123456".into())));
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
    assert_eq!(desk.log.forgot, 0);
    let mut desk = Fake::new(|_: &Log| Err(DeskError::Gone));
    assert_eq!(run(AUTHORIZE, &mut desk), Err(Stop::Closed));
    assert_eq!(desk.log.forgot, 0);
}

/// A PRESS OR A SET THAT FAILED IS TRIED AGAIN, counted with the reads that failed: a few do
/// not end the sign-in, and the code the mailbox gave once is held through them; as many in a
/// row as end it on reads end it. A SET IN A DOCUMENT THAT MOVED IS COUNTED AGAINST NOTHING:
/// the page is read again.
#[test]
fn an_act_that_failed_is_tried_again_and_counted_with_the_reads() {
    let mut desk = Fake::new(whole_flow).with_mail(|_| Ok(MailboxCredential::Otp("123456".into())));
    desk.act_errors = MAX_UNREAD - 1;
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
    assert_eq!(desk.log.polls, 1);

    // The code was read, and every set of it fails: it is held, never asked for again.
    let mut desk = Fake::new(|_: &Log| Ok(code_page(false)))
        .with_mail(|_| Ok(MailboxCredential::Otp("123456".into())));
    desk.act_errors = usize::MAX;
    assert_eq!(
        run(AUTHORIZE, &mut desk),
        Err(Stop::Desk("the page was busy".into()))
    );
    assert_eq!(desk.log.polls, 1);
    assert_eq!(desk.log.acted_at.len(), MAX_UNREAD);

    let mut desk = Fake::new(whole_flow).with_mail(|_| Ok(MailboxCredential::Otp("123456".into())));
    desk.email_fills = vec![Filled::Moved; MAX_FILLS + 2];
    desk.email_fills.push(Filled::Kept);
    desk.code_fills = vec![Filled::Moved; MAX_FILLS + 2];
    desk.code_fills.push(Filled::Kept);
    desk.page = Box::new(|log: &Log| {
        Ok(if log.pressed.contains(&Button::Authorize) {
            at(ANSWERED)
        } else if log.pressed.contains(&Button::VerifyEmailAddress) {
            consent_page("Logged in as one@example.com")
        } else if log.pressed.contains(&Button::ContinueWithEmail) {
            code_page(codes(log) > MAX_FILLS + 2)
        } else {
            login_page()
        })
    });
    assert_eq!(run(AUTHORIZE, &mut desk), Ok(Done::Delivered));
    assert_eq!(desk.log.polls, 1);
}

/// ONE SIGN-IN WINDOW AT A TIME ON THIS MAC, a token's included: the window's turn is a lock
/// beside the accounts store that the flow holds while its window is open. A second flow
/// waits for it and ends, with its own words and as an ordinary failure, where the first
/// outlasts its wait; it takes the turn once the first lets go; a cancelled scope ends the
/// wait. With no store there is no lock, and nothing to wait for.
#[test]
fn one_sign_in_window_is_open_at_a_time() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let store = dir.path().join("accounts");
    let short = (Duration::from_millis(300), Duration::from_millis(50));
    let Ok(Some(first)) = window_turn(Some(&store), &|| false, short) else {
        panic!("the first window's turn");
    };
    assert!(store.join(WINDOW_LOCK_FILE).is_file());
    let began = Instant::now();
    let Err(stop) = window_turn(Some(&store), &|| false, short) else {
        panic!("the turn is taken");
    };
    assert_eq!(stop, Stop::Busy);
    assert!(began.elapsed() >= short.0);
    let words = stop.to_string();
    assert!(words.contains("one is open at a time"), "{words}");
    assert_eq!(VisibleStop::from(stop), VisibleStop::Failed(words));
    assert_eq!(
        window_turn(Some(&store), &|| true, short).err(),
        Some(Stop::Cancelled)
    );
    drop(first);
    assert!(matches!(
        window_turn(Some(&store), &|| false, short),
        Ok(Some(_))
    ));
    assert!(matches!(window_turn(None, &|| false, short), Ok(None)));
    // The route's own bound covers a wait for the other window and the sign-in after it.
    assert!(super::super::open_url::WINDOW_ROUTE_WITHIN >= TURN_WITHIN + WITHIN + READY_WITHIN);
    assert!(super::super::open_url::WINDOW_ROUTE_WITHIN < super::super::login_drive::HANG);
}

/// A stand-in for the window's host: an owner-only `/bin/sh` program that keeps its
/// arguments and the first line it was sent beside itself, then runs `body`.
#[cfg(unix)]
struct StandIn {
    dir: aterm_tempfile::TempDir,
    exe: std::path::PathBuf,
}

/// The stand-in's loop: the `n`th request is answered with the `n`th line of the replies
/// file, a tab in it ending a line; a request with no line is not answered.
#[cfg(unix)]
const ANSWERS: &str = r#"n=0
while IFS= read -r line; do
  n=$((n+1))
  /usr/bin/sed -n "${n}p" "$0.replies" | /usr/bin/tr '\t' '\n'
done"#;

#[cfg(unix)]
impl StandIn {
    fn new(body: &str, replies: &[String]) -> StandIn {
        let dir = aterm_tempfile::tempdir().unwrap();
        let exe = dir.path().join("host");
        let mut lines = replies.join("\n");
        lines.push('\n');
        std::fs::write(dir.path().join("host.replies"), lines).unwrap();
        super::super::stand_in::proven(
            &exe,
            &format!(
                "printf '%s\\n' \"$*\" > \"$0.argv\"\nIFS= read -r first || exit 4\nprintf \
                 '%s\\n' \"$first\" > \"$0.first\"\n{body}"
            ),
        );
        StandIn { dir, exe }
    }

    /// A stand-in that says its window is open and then answers.
    fn ready(replies: &[String]) -> StandIn {
        StandIn::new(&format!("echo '{}'\n{ANSWERS}", wire::ready(true)), replies)
    }

    fn kept(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
    }

    fn open() -> wire::Open {
        wire::Open {
            title: format!("aterm \u{b7} signing in Claude {EMAIL}"),
            store: Some(store_id(&format!("claude:{EMAIL}"))),
            hosts: vec!["claude.ai".into()],
            loopback: Some(4321),
        }
    }

    /// Starts the host on this stand-in, with short bounds on its end.
    fn start(&self, within: Duration) -> Result<Host, DeskError> {
        Host::start_ending(
            &self.exe,
            &StandIn::open(),
            within,
            (Duration::from_millis(300), Duration::from_millis(500)),
        )
    }
}

/// THE HOST IS TOLD WHAT TO OPEN ON ITS STANDARD INPUT, AND ITS ARGUMENT IS THE VERB ALONE:
/// the window's title names the account, and no process listing shows it. The host's first
/// line is the one the host's own parser takes. Its events are kept for the next read: a
/// navigation that failed, with its code (one another navigation replaced is none), and one
/// the host refused.
#[cfg(unix)]
#[test]
fn the_host_gets_its_title_on_its_standard_input_and_keeps_what_its_events_say() {
    let stand_in = StandIn::ready(&[
        format!(
            "{}\t{}\t{}\t{}",
            wire::load_failed(-999),
            wire::load_failed(-1009),
            wire::event(wire::REFUSED),
            wire::reply_done(1)
        ),
        wire::reply_error(2, "the page did not answer"),
    ]);
    let mut host = stand_in.start(Duration::from_secs(60)).expect("the window");
    assert_eq!(
        stand_in.kept("host.argv"),
        format!("{}\n", aterm_signin_window::VERB)
    );
    assert!(!stand_in.kept("host.argv").contains(EMAIL));
    assert_eq!(
        wire::parse_open(stand_in.kept("host.first").trim_end()),
        Ok(StandIn::open())
    );
    assert_eq!(
        host.ask(|id| wire::Request::Load {
            id,
            url: LOGIN.into(),
            aside: false
        }),
        Ok((None, None))
    );
    assert_eq!(host.failed, Some(-1009));
    assert_eq!(host.refused, 1);
    assert_eq!(
        host.ask(|id| wire::Request::Forget { id }),
        Err(DeskError::Unread("the page did not answer".into()))
    );
    host.end();
    assert!(matches!(host.child.try_wait(), Ok(Some(_))));
}

/// A HOST THAT DOES NOT OPEN ITS WINDOW IS ITS OWN FAILURE, never a window a person closed:
/// one that ends before it says so, one that says nothing in time, and a program that cannot
/// be started at all; a session with no desktop says that. Each is ended and reaped.
#[cfg(unix)]
#[test]
fn a_host_that_opens_no_window_is_not_a_closed_window() {
    let ended = StandIn::new("exit 3", &[]);
    assert_eq!(
        ended.start(Duration::from_secs(60)).err(),
        Some(DeskError::NoWindow(
            "its host ended before the window opened".into()
        ))
    );
    let silent = StandIn::new(ANSWERS, &[]);
    assert_eq!(
        silent.start(Duration::from_millis(300)).err(),
        Some(DeskError::NoWindow("it did not open in time".into()))
    );
    let no_desktop = StandIn::new(
        &format!("echo '{}'\n{ANSWERS}", wire::event(wire::NO_DESKTOP)),
        &[],
    );
    assert_eq!(
        no_desktop.start(Duration::from_secs(60)).err(),
        Some(DeskError::NoDesktop)
    );
    let dir = aterm_tempfile::tempdir().unwrap();
    assert_eq!(
        Host::start(
            &dir.path().join("absent"),
            &StandIn::open(),
            Duration::from_secs(60)
        )
        .err(),
        Some(DeskError::NoWindow("its host could not be started".into()))
    );
    // Each is the flow's ordinary failure with why, and none is the closed window's word.
    for error in [
        DeskError::NoWindow("it did not open in time".into()),
        DeskError::Ended,
    ] {
        assert!(matches!(
            VisibleStop::from(Stop::from(error)),
            VisibleStop::Failed(_)
        ));
    }
}

/// WHAT THE HOST SAYS MID-REQUEST: a window a person closed is that (and stays that once the
/// host's pipe has closed after it); a line torn by the host's end is no reply and the host
/// has ended; a request the host never answers fails in its own time and the next may not; a
/// host that ignores the request to close is ended and reaped.
#[cfg(unix)]
#[test]
fn a_host_that_closes_ends_or_stops_answering_is_told_apart() {
    let load = |id| wire::Request::Load {
        id,
        url: LOGIN.into(),
        aside: false,
    };
    let closed = StandIn::ready(&[wire::event(wire::CLOSED)]);
    let mut host = closed.start(Duration::from_secs(60)).expect("the window");
    assert_eq!(host.ask(load), Err(DeskError::Gone));
    host.end();
    assert!(matches!(host.child.try_wait(), Ok(Some(_))));

    // The window closed and its host ended before the next request was written.
    let closed_then_gone = StandIn::new(
        &format!(
            "echo '{}'\necho '{}'\nexit 0",
            wire::ready(true),
            wire::event(wire::CLOSED)
        ),
        &[],
    );
    let mut host = closed_then_gone
        .start(Duration::from_secs(60))
        .expect("the window");
    let hang = Instant::now() + aterm_uds::deadline::ECHO_HANG;
    while matches!(host.child.try_wait(), Ok(None)) {
        assert!(Instant::now() < hang, "the stand-in never ended");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(host.ask(load), Err(DeskError::Gone));

    let torn = StandIn::new(
        &format!(
            "echo '{}'\nIFS= read -r line\nprintf '%s' '{{\"id\":1,\"ok\":tr'\nexit 0",
            wire::ready(true)
        ),
        &[],
    );
    let mut host = torn.start(Duration::from_secs(60)).expect("the window");
    assert_eq!(host.ask(load), Err(DeskError::Ended));
    host.end();

    let unanswered = StandIn::ready(&[]);
    let mut host = unanswered
        .start(Duration::from_secs(60))
        .expect("the window");
    assert_eq!(
        host.ask_within(load, Duration::from_millis(300)),
        Err(DeskError::Unread(
            "the sign-in window did not answer in time".into()
        ))
    );
    host.end();
    assert!(matches!(host.child.try_wait(), Ok(Some(_))));

    // It reads the request to close, answers nothing and stays: it is ended.
    let stays = StandIn::new(
        &format!(
            "echo '{}'\n{ANSWERS}\nexec /bin/sleep 600",
            wire::ready(true)
        ),
        &[],
    );
    let mut host = stays.start(Duration::from_secs(60)).expect("the window");
    let began = Instant::now();
    host.end();
    assert!(began.elapsed() < Duration::from_secs(60));
    assert!(matches!(host.child.try_wait(), Ok(Some(_))));
}

/// THE WINDOW'S DESK JUDGES EVERY ANSWER BY THE DOCUMENT THAT GAVE IT, over a stand-in host:
/// a read keeps the document's own address apart from the web view's and reports a failed
/// navigation once; a press counts only where the document that ran it is the one it was
/// made for; an act for a document that is not the flow's is not sent at all; a sign-in
/// link's page is opened aside once, its code is taken only from a document of the flow's at
/// the link's own place, and it has moved on only after it stood elsewhere a while; a
/// forgotten session's record goes with it.
#[cfg(unix)]
#[test]
fn the_windows_desk_judges_each_answer_by_the_document_that_gave_it() {
    let act = |id, at: &str, did: &str| {
        wire::reply_value(id, Some(&format!(r#"{{"at":"{at}","did":"{did}"}}"#)), at)
    };
    let link = |id, at: &str, code: &str| {
        wire::reply_value(
            id,
            Some(&format!(r#"{{"at":"{at}","code":"{code}","shape":"<6>"}}"#)),
            at,
        )
    };
    let read = format!(
        r#"{{"at":"{LOGIN}","title":"Sign in","text":"Welcome","markers":[],"buttons":["Continue with email"],"disabled":[],"check":null,"code":null}}"#
    );
    let stand_in = StandIn::ready(&[
        wire::reply_done(1),
        format!(
            "{}\t{}\t{}",
            wire::load_failed(-1009),
            wire::event(wire::REFUSED),
            wire::reply_value(2, Some(&read), CONSENT)
        ),
        wire::reply_value(3, Some(&read), LOGIN),
        act(4, LOGIN, "pressed"),
        act(5, "https://claude.ai/elsewhere", "pressed"),
        act(6, LOGIN, "moved"),
        act(7, LOGIN, "filled"),
        act(8, LOGIN, "untaken"),
        act(9, "https://claude.ai/elsewhere", "filled"),
        wire::reply_done(10),
        link(11, "https://claude.ai/magic-link#shown", "246810"),
        link(12, "https://example.invalid/magic-link", "999999"),
        link(13, "https://claude.ai/new", "111111"),
        link(14, "https://claude.ai/new", "111111"),
        wire::reply_done(15),
        wire::reply_done(16),
    ]);
    let records = aterm_tempfile::tempdir().unwrap();
    let flow = MailboxFlow::new(AUTHORIZE).unwrap();
    let mut desk = Window::new(
        stand_in.exe.clone(),
        Some(records.path()),
        None,
        EMAIL,
        None,
    )
    .for_flow(&flow);
    desk.open(AUTHORIZE, Duration::from_secs(60)).expect("open");
    desk.host.as_mut().unwrap().ends = (Duration::from_millis(300), Duration::from_millis(500));
    // What the host was told to open: the flow's hosts and its listener's port.
    let opened = wire::parse_open(stand_in.kept("host.first").trim_end()).unwrap();
    assert_eq!(opened.title, desk.title);
    assert_eq!(opened.store.as_deref(), Some(desk.store.as_str()));
    assert_eq!(
        opened.hosts,
        ["claude.ai", "claude.com", "platform.claude.com"]
    );
    assert_eq!(opened.loopback, Some(4321));

    let facts = desk.facts().expect("a read");
    assert_eq!(
        (facts.url.as_str(), facts.active.as_str()),
        (LOGIN, CONSENT)
    );
    assert_eq!(facts.load_failed, Some(-1009));
    assert_eq!(desk.facts().expect("a read").load_failed, None);

    assert_eq!(
        desk.press(Button::ContinueWithEmail, EMAIL, LOGIN),
        Ok(true)
    );
    // Pressed in another document than the one the press was made for: not this press.
    assert_eq!(desk.press(Button::Authorize, EMAIL, LOGIN), Ok(false));
    assert_eq!(desk.press(Button::Authorize, EMAIL, LOGIN), Ok(false));
    // A document that is not the flow's: nothing is sent.
    let sent = desk.host.as_ref().unwrap().next;
    assert_eq!(
        desk.press(Button::Authorize, EMAIL, "https://example.invalid/consent"),
        Ok(false)
    );
    assert_eq!(
        desk.fill(Field::Code, "123456", "https://example.invalid/login"),
        Ok(Filled::Moved)
    );
    assert_eq!(
        desk.fill(Field::Code, "123456", "http://claude.ai/login"),
        Ok(Filled::Moved)
    );
    assert_eq!(desk.host.as_ref().unwrap().next, sent);

    assert_eq!(desk.fill(Field::Code, "123456", LOGIN), Ok(Filled::Kept));
    assert_eq!(desk.fill(Field::Code, "123456", LOGIN), Ok(Filled::NotKept));
    assert_eq!(desk.fill(Field::Code, "123456", LOGIN), Ok(Filled::Moved));

    // The link is opened aside once (request 10) and read four times.
    assert_eq!(desk.link_code(MAGIC, Duration::ZERO), Ok(shown("246810")));
    assert_eq!(desk.link_code(MAGIC, Duration::ZERO), Ok(Linked::Waiting));
    assert_eq!(desk.link_code(MAGIC, Duration::ZERO), Ok(Linked::Waiting));
    desk.moved_at = Instant::now().checked_sub(MOVED_SETTLE);
    assert!(desk.moved_at.is_some());
    assert_eq!(desk.link_code(MAGIC, Duration::ZERO), Ok(Linked::Moved));

    desk.note_signed_in();
    assert!(desk.store_is_the_accounts());
    desk.forget();
    assert!(!desk.store_is_the_accounts());
    assert_eq!(desk.host.as_ref().unwrap().next, 15);
    desk.close();
    assert!(desk.host.is_none());
}
