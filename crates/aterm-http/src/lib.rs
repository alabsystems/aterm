// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! First-party HTTP/1.1 client for `aterm`: one bounded request at a time over TCP or platform-verified TLS ([`Trust::PlatformVerifier`], or explicit [`Trust::Roots`]).
//! Deliberately omits redirect following, connection pooling, HTTP/2, cookies, and automatic decompression.

pub mod client;
/// `TcpStream::connect_timeout`, with a refusal reported promptly on Windows too.
pub mod connect;
/// Whether macOS's Local Network privacy refuses this process a `.local` name.
mod local_network;
pub mod pem;
pub mod proxy;
pub mod stream;
pub mod tls;
pub mod uri;
pub mod verifier;

pub use client::{
    Body, BodyReader, Client, Error, Exchange, Method, Phase, RequestBuilder, Response, SendError,
};
pub use proxy::ProxyMode;
pub use stream::{AlwaysAuthorized, Connect, Deadline, Guard, TcpConnector};
pub use tls::Trust;
pub use uri::{Scheme, Uri};
