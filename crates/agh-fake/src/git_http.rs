// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The web origin's smart-HTTP git endpoints (protocol v0; git falls back to it from v2):
//! the ref advertisement of `git_refs`, a delete-only `git-receive-pack`, and a 501 for
//! `git-upload-pack`, since clones and fetches in tests use local repositories. Credentials
//! are Basic, username `x-access-token`, checked against `add_token`; on these paths they are
//! no violation. A credential that is not one (another username, an unknown token) is 401 on
//! every git path, a public repository's included, as GitHub refuses bad credentials.

use crate::FakeOrigin;
use crate::routes::{Answer, CREDENTIAL, Request};
use crate::state::State;

/// The sha git writes for "no object".
const ZERO: &str = "0000000000000000000000000000000000000000";
/// The username agh's credential helper answers.
const USERNAME: &str = "x-access-token";
/// The flush packet.
const FLUSH: &[u8] = b"0000";

/// One of the git endpoints a path names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Endpoint {
    /// `GET …/info/refs?service=…`.
    InfoRefs,
    /// `POST …/git-receive-pack`.
    ReceivePack,
    /// `POST …/git-upload-pack`.
    UploadPack,
}

/// The two services.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Service {
    /// `git-upload-pack`: needs pull on a private repository.
    Upload,
    /// `git-receive-pack`: needs push.
    Receive,
}

impl Service {
    /// The service's name.
    const fn name(self) -> &'static str {
        match self {
            Service::Upload => "git-upload-pack",
            Service::Receive => "git-receive-pack",
        }
    }
}

/// The repository `owner/name` (any `.git` suffix dropped) and the endpoint a path names.
fn parse(path: &str) -> Option<(String, Endpoint)> {
    let segs: Vec<&str> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let (owner, name, endpoint) = match segs.as_slice() {
        [o, r, "info", "refs"] => (*o, *r, Endpoint::InfoRefs),
        [o, r, "git-receive-pack"] => (*o, *r, Endpoint::ReceivePack),
        [o, r, "git-upload-pack"] => (*o, *r, Endpoint::UploadPack),
        _ => return None,
    };
    let name = name.strip_suffix(".git").unwrap_or(name);
    Some((
        format!(
            "{}/{}",
            aterm_codec::pct::decode(owner),
            aterm_codec::pct::decode(name)
        ),
        endpoint,
    ))
}

/// Whether `req` reaches a git endpoint of the web origin.
pub(crate) fn is_git(req: &Request) -> bool {
    req.origin == FakeOrigin::Web && parse(req.path()).is_some()
}

/// Whether header `name: value` is the Basic credential a git path of the web origin takes,
/// which is no violation there (any other credential header still is).
pub(crate) fn allowed_basic(req: &Request, name: &str, value: &str) -> bool {
    is_git(req) && name.eq_ignore_ascii_case(CREDENTIAL) && basic_payload(value).is_some()
}

/// The decoded `user:password` of a `Basic` credential value.
fn basic_payload(value: &str) -> Option<String> {
    let (scheme, encoded) = value.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let bytes = aterm_codec::base64::decode(encoded.trim()).ok()?;
    String::from_utf8(bytes).ok()
}

/// The login a git request's Basic credential authorizes: username `x-access-token` and a
/// token `add_token` registered. Any other username, or an unknown token, is no login.
fn basic_login(state: &State, req: &Request) -> Option<String> {
    let payload = basic_payload(req.header(CREDENTIAL)?)?;
    let (user, token) = payload.split_once(':')?;
    (user == USERNAME && !token.is_empty())
        .then(|| state.login_of(token))
        .flatten()
}

/// The login whose registered token a git request's Basic credential carries, as its
/// password or as its username (GitHub's `{token}:x-oauth-basic` form): with a Bearer or
/// `token` header, what `Seen::as_login` records, apart from whether the credential
/// authorizes the request (`basic_login`).
pub(crate) fn basic_token_login(state: &State, req: &Request) -> Option<String> {
    let payload = basic_payload(req.header(CREDENTIAL)?)?;
    let (user, token) = payload.split_once(':').unwrap_or((payload.as_str(), ""));
    [token, user]
        .into_iter()
        .filter(|t| !t.is_empty())
        .find_map(|t| state.login_of(t))
}

/// The longest payload one pkt-line holds (git's `LARGE_PACKET_MAX` of 65520, less its
/// four-digit length).
const PKT_PAYLOAD_MAX: usize = 65516;

/// The longest ref name the fake takes, from the git endpoints or `POST git/refs`, so every
/// line that names one fits a pkt-line.
pub(crate) const REF_NAME_MAX: usize = 1024;

// A ref line (`{sha} {ref}\0{capabilities}\n`, or a report's `ng {ref} stale info\n`) fits one
// pkt-line.
const _: () = assert!(REF_NAME_MAX + 256 <= PKT_PAYLOAD_MAX);

/// One pkt-line holding `payload`, which its callers keep to [`PKT_PAYLOAD_MAX`] bytes: a
/// report line names a ref of at most [`REF_NAME_MAX`] bytes, and `advertise` refuses a
/// fixture ref too long for one.
fn pkt(out: &mut Vec<u8>, payload: &[u8]) {
    out.extend_from_slice(format!("{:04x}", payload.len() + 4).as_bytes());
    out.extend_from_slice(payload);
}

/// A plain-text answer.
fn text(status: u16, body: &str) -> Answer {
    Answer {
        status,
        headers: vec![("Content-Type".into(), "text/plain; charset=utf-8".into())],
        body: body.as_bytes().to_vec(),
    }
}

/// The 401 that makes git ask its credential helper.
fn unauthorized() -> Answer {
    let mut a = text(401, "Authentication required");
    a.headers
        .push(("WWW-Authenticate".into(), "Basic realm=\"GitHub\"".into()));
    a
}

/// A git endpoint's answer.
pub(crate) fn answer(state: &mut State, req: &Request) -> Answer {
    let Some((slug, endpoint)) = parse(req.path()) else {
        return text(404, "Not Found");
    };
    // A credential is judged before the endpoint, as GitHub authenticates first.
    let login = basic_login(state, req);
    if req.header(CREDENTIAL).is_some() && login.is_none() {
        return unauthorized();
    }
    let (service, method) = match endpoint {
        Endpoint::UploadPack => return text(501, "Not Implemented"),
        Endpoint::ReceivePack => (Service::Receive, "POST"),
        Endpoint::InfoRefs => match req.query("service").as_deref() {
            Some("git-upload-pack") => (Service::Upload, "GET"),
            Some("git-receive-pack") => (Service::Receive, "GET"),
            _ => return text(403, "Service not enabled"),
        },
    };
    if req.method != method {
        return text(405, "Method Not Allowed");
    }
    let Some(index) = state.repo_index(&slug) else {
        return if login.is_some() {
            text(404, "Repository not found.")
        } else {
            unauthorized()
        };
    };
    let repo = &state.repos[index];
    let allowed = match service {
        Service::Upload => repo.visible_to(login.as_deref()),
        Service::Receive => repo.is_push(login.as_deref()),
    };
    if !allowed {
        return unauthorized();
    }
    match endpoint {
        Endpoint::InfoRefs => advertise(state, index, service),
        _ => receive(state, index, &req.body),
    }
}

/// The v0 advertisement of repository `index`'s refs for `service`.
fn advertise(state: &State, index: usize, service: Service) -> Answer {
    let repo = &state.repos[index];
    let caps = match service {
        Service::Receive => "report-status delete-refs".to_string(),
        Service::Upload => format!("symref=HEAD:refs/heads/{}", repo.spec.default_branch),
    };
    let refs = repo.refs();
    let long = |name: &str| name.len() > REF_NAME_MAX;
    if long(&repo.spec.default_branch) || refs.iter().any(|(reference, _)| long(reference)) {
        return text(500, "a ref name too long for a pkt-line");
    }
    let mut body = Vec::new();
    pkt(
        &mut body,
        format!("# service={}\n", service.name()).as_bytes(),
    );
    body.extend_from_slice(FLUSH);
    if refs.is_empty() {
        pkt(
            &mut body,
            format!("{ZERO} capabilities^{{}}\0{caps}\n").as_bytes(),
        );
    }
    for (i, (reference, sha)) in refs.iter().enumerate() {
        let line = if i == 0 {
            format!("{sha} {reference}\0{caps}\n")
        } else {
            format!("{sha} {reference}\n")
        };
        pkt(&mut body, line.as_bytes());
    }
    body.extend_from_slice(FLUSH);
    Answer {
        status: 200,
        headers: vec![
            (
                "Content-Type".into(),
                format!("application/x-{}-advertisement", service.name()),
            ),
            ("Cache-Control".into(), "no-cache".into()),
        ],
        body,
    }
}

/// The pkt-lines of `body` up to its first flush, each without its trailing LF; `None` when
/// a length is not four hex digits or runs past the body, or no flush ends the list.
fn commands(body: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut lines = Vec::new();
    let mut at = 0;
    loop {
        let head = std::str::from_utf8(body.get(at..at + 4)?).ok()?;
        let len = usize::from_str_radix(head, 16).ok()?;
        if len == 0 {
            return Some(lines);
        }
        let payload = body.get(at + 4..at.checked_add(len)?)?;
        lines.push(payload.strip_suffix(b"\n").unwrap_or(payload).to_vec());
        at += len;
    }
}

/// A delete-only `git-receive-pack`: each `{old} {zero} {ref}` whose `old` is the ref's sha
/// deletes it (`ok {ref}`), another `old` is `ng {ref} stale info`, and a command whose new
/// sha is not zero is `ng {ref} only deletes`. Capabilities after the first command's NUL,
/// and anything after the flush (a pack), are ignored. As git's receive-pack does, every
/// command is read before any ref changes, so a malformed one lands nothing.
fn receive(state: &mut State, index: usize, body: &[u8]) -> Answer {
    let Some(lines) = commands(body) else {
        return text(400, "malformed receive-pack request");
    };
    let mut parsed = Vec::with_capacity(lines.len());
    for line in &lines {
        let command = line.split(|b| *b == 0).next().unwrap_or(&[]);
        let command = String::from_utf8_lossy(command).into_owned();
        let mut words = command.splitn(3, ' ').map(str::to_string);
        let (Some(old), Some(new), Some(reference)) = (words.next(), words.next(), words.next())
        else {
            return text(400, "malformed receive-pack command");
        };
        if reference.contains(' ') || reference.len() > REF_NAME_MAX {
            return text(400, "malformed receive-pack command");
        }
        parsed.push((old, new, reference));
    }
    let mut report = Vec::new();
    pkt(&mut report, b"unpack ok\n");
    for (old, new, reference) in parsed {
        let repo = &mut state.repos[index];
        let status = if new != ZERO {
            format!("ng {reference} only deletes\n")
        } else if repo.ref_sha(&reference).as_deref() == Some(old.as_str()) {
            repo.delete_ref(&reference);
            format!("ok {reference}\n")
        } else {
            format!("ng {reference} stale info\n")
        };
        pkt(&mut report, status.as_bytes());
    }
    report.extend_from_slice(FLUSH);
    Answer {
        status: 200,
        headers: vec![
            (
                "Content-Type".into(),
                "application/x-git-receive-pack-result".into(),
            ),
            ("Cache-Control".into(), "no-cache".into()),
        ],
        body: report,
    }
}
