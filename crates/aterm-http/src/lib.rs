// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A first-party HTTP/1.1 client for aterm: one bounded request at a time to
//! an endpoint the caller names, over TCP or TLS the platform verifies.
//!
//! Two callers use it. The supervisor's network probe (`aterm-gui`'s
//! `harness_netprobe`) opens a [`stream::Stream`] and completes a
//! platform-verified TLS handshake under one [`Deadline`]. The dev-only
//! real-Claude lab (`aterm-lab`'s stub) drives its loopback API stand-in with
//! [`Client`] over plain HTTP. The request/response half was written for the
//! LLM title-summary worker, retired with Smart Titles on 2026-10-15; its docs
//! say what a caller gets, not who calls it.
//!
//! # Trust
//!
//! HTTPS endpoints are checked against the OPERATING SYSTEM's trust store,
//! never a root set baked into the binary, so an endpoint is reached the way
//! the OS would reach it; a caller that names its own roots
//! ([`Trust::Roots`]) REPLACES the platform roots rather than adding to them.
//! [`verifier`] makes the OS calls itself (`SecTrustEvaluateWithError` on
//! macOS, `CertGetCertificateChain` on Windows, the distro CA store plus
//! `rustls`'s webpki on Linux).
//!
//! # Shape
//!
//! * [`uri`] — strict absolute-URL parsing; rejects userinfo and control
//!   characters instead of sanitising them.
//! * [`tls`] — the two trust models, over `rustls` + the `ring` provider.
//! * [`verifier`] — the platform certificate verifier behind
//!   [`Trust::PlatformVerifier`].
//! * [`pem`] — the Linux/BSD CA-store parser the verifier reads.
//! * [`proxy`] — `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY`, failing
//!   closed on a setting it cannot use; a caller keeps a loopback endpoint off
//!   every proxy with [`ProxyMode::Direct`].
//! * [`stream`] — TCP/TLS bytes, one global deadline, and the revocable
//!   authority re-checked at every read and write.
//! * [`client`] — request rendering and response parsing.
//! * [`connect`] — a timed TCP connect whose refusal is prompt on Windows too,
//!   for a caller that splits a short budget across a host's addresses.
//!
//! # What it deliberately does NOT do
//!
//! No redirect following (a followed redirect is how a bearer token reaches a
//! host the caller never named), no connection pool, no HTTP/2, no cookie
//! jar, and no automatic decompression. A caller making one bounded call at a
//! time needs none of it, and each would be another place for a response to
//! influence where the next request goes.

pub mod client;
/// `TcpStream::connect_timeout`, with a refusal reported promptly on Windows too.
pub mod connect;
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
