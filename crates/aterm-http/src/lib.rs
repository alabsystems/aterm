// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A first-party HTTP/1.1 client — the replacement for `ureq` in aterm's
//! LLM title-summary worker.
//!
//! # Why this exists
//!
//! `ureq` was aterm's largest remaining third-party surface: 89,080 lines
//! across 12 packages (`ureq`, `ureq-proto`, `http`, `bytes`, `httparse`,
//! `utf8-zero`, `percent-encoding`, `webpki-roots`, and the platform-verifier
//! chain). It was used from three files to make ONE shape of call: a JSON POST
//! to a chat-completions endpoint, usually a loopback Ollama.
//!
//! # THE TRUST DECISION — recorded, not silently made
//!
//! `crates/aterm-gui/Cargo.toml` asked ureq for its `platform-verifier`
//! feature deliberately, so that HTTPS endpoints are checked against the
//! OPERATING SYSTEM's trust store rather than a root set baked into the
//! binary. Three replacement models were possible:
//!
//! * **(a) keep the OS trust store.** Chosen, and implemented here.
//! * **(b) bundle a root set.** A real security change; NOT made.
//! * **(c) loopback-only plaintext.** Refuted by the code: the
//!   `OpenAiCompatible` provider takes an operator-configured absolute
//!   endpoint, and the surrounding machinery (bearer-token file, CA-bundle
//!   override, `HTTP(S)_PROXY` handling) exists precisely to reach a remote
//!   HTTPS service. Dropping TLS would have broken a supported configuration.
//!
//! Platform verification therefore stays, and is now FIRST-PARTY: [`verifier`]
//! makes the OS calls itself (`SecTrustEvaluateWithError` on macOS,
//! `CertGetCertificateChain` on Windows, the distro CA store plus `rustls`'s own
//! webpki on Linux). What left with ureq was its HTTP/1.1 stack and the bundled
//! roots it carried (8 packages, 69,546 lines); what left with
//! `rustls-platform-verifier` was another 4 packages / 19,534 lines on mac-arm.
//! The trust DECISION did not change in either step — the same operating system
//! is still the one making it.
//!
//! # Shape
//!
//! * [`uri`] — strict absolute-URL parsing; rejects userinfo and control
//!   characters instead of sanitising them.
//! * [`pem`] — CA-bundle decoding for the per-endpoint trust override.
//! * [`tls`] — the two trust models, over `rustls` + the `ring` provider
//!   (the same pin `aterm-net` already ships).
//! * [`verifier`] — the FIRST-PARTY platform certificate verifier behind
//!   [`Trust::PlatformVerifier`]: Security.framework on macOS, `crypt32` on
//!   Windows, `/etc/ssl/certs` + webpki on Linux.
//! * [`proxy`] — `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`, and the rule that a
//!   loopback endpoint is never proxied.
//! * [`stream`] — TCP/TLS bytes, one global deadline, and the revocable
//!   authority re-checked at every read and write.
//! * [`client`] — request rendering and response parsing.
//!
//! # What it deliberately does NOT do
//!
//! No redirect following (the retired client was configured
//! `max_redirects(0)`; chasing a redirect is how a bearer token reaches a host
//! the operator never named), no connection pool, no HTTP/2, no cookie jar,
//! and no automatic decompression. One bounded worker making one call at a
//! time needs none of it, and each would be another place for a response to
//! influence where the next request goes.

pub mod client;
pub mod pem;
pub mod proxy;
pub mod stream;
pub mod tls;
pub mod uri;
pub mod verifier;

pub use client::{Client, Error, RequestBuilder, Response};
pub use proxy::ProxyMode;
pub use stream::{AlwaysAuthorized, Connect, Deadline, Guard, TcpConnector};
pub use tls::Trust;
pub use uri::{Scheme, Uri};
