// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A first-party HTTP/1.1 client for aterm's LLM title-summary worker: one
//! JSON POST to a chat-completions endpoint, usually a loopback Ollama.
//!
//! # Trust
//!
//! HTTPS endpoints are checked against the OPERATING SYSTEM's trust store,
//! never a root set baked into the binary. TLS cannot be dropped for
//! loopback-only plaintext: the `OpenAiCompatible` provider takes an
//! operator-configured endpoint, and the bearer-token file, CA-bundle override
//! and proxy handling exist to reach a remote HTTPS service. [`verifier`] makes
//! the OS calls itself (`SecTrustEvaluateWithError` on macOS,
//! `CertGetCertificateChain` on Windows, the distro CA store plus `rustls`'s
//! webpki on Linux).
//!
//! # Shape
//!
//! * [`uri`] — strict absolute-URL parsing; rejects userinfo and control
//!   characters instead of sanitising them.
//! * [`pem`] — CA-bundle decoding for the per-endpoint trust override.
//! * [`tls`] — the two trust models, over `rustls` + the `ring` provider.
//! * [`verifier`] — the platform certificate verifier behind
//!   [`Trust::PlatformVerifier`].
//! * [`proxy`] — `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY`, failing
//!   closed on a setting it cannot use; a caller keeps a loopback endpoint off
//!   every proxy with [`ProxyMode::Direct`].
//! * [`stream`] — TCP/TLS bytes, one global deadline, and the revocable
//!   authority re-checked at every read and write.
//! * [`client`] — request rendering and response parsing.
//!
//! # What it deliberately does NOT do
//!
//! No redirect following (a followed redirect is how a bearer token reaches a
//! host the operator never named), no connection pool, no HTTP/2, no cookie
//! jar, and no automatic decompression. One bounded worker making one call at
//! a time needs none of it, and each would be another place for a response to
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
