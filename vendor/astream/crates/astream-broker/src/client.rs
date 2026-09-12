//! A blocking client for the broker: connect, publish (awaiting the ack), and
//! subscribe (blocking on each delivery). The blocking calls are what make tests
//! deterministic — a publish round-trips before the next step, and a subscriber's
//! `recv` returns exactly when the next delivery arrives (Condvar-driven), with no
//! reliance on sleeps or wall-clock.

use crate::proto::{decode_response, encode_request, read_frame, write_frame, Request, Response};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::path::Path;
use std::time::Duration;

/// One record as a client sees it: `(offset, subject, body)` — what a `Delivery`
/// carries.
pub type Record = (u64, String, Vec<u8>);

/// The answer to a bounded, NON-TERMINAL read ([`Client::last`], [`Client::fetch`]):
/// the page of records, then the `(next, head)` its closing `Mark` carried.
pub type Page = (Vec<Record>, (u64, u64));

/// A connection to a broker over a byte stream. Three transports:
///
/// - [`connect`](Client::connect) — a Unix socket, same machine, unsealed (the
///   kernel scopes who can reach the socket path).
/// - [`connect_tcp`](Client::connect_tcp) — PLAINTEXT TCP, multi-machine on a
///   trusted network only: no confidentiality, no peer authentication.
/// - `connect_tcp_sealed` (behind the `aead` feature) — TCP inside an
///   XChaCha20-Poly1305 sealed record layer under a pre-shared key: confidential
///   and authenticated across an untrusted network.
///
/// `S` is the byte stream; any `Read + Write` works (see
/// [`from_stream`](Client::from_stream)), and the Frame protocol is identical on
/// every transport.
#[cfg(unix)]
pub struct Client<S = UnixStream> {
    stream: S,
}

/// A connection to a broker over a byte stream. Windows twin: std has no
/// Unix-domain sockets there, so TCP (`connect_tcp`) is the default (and only)
/// transport.
#[cfg(not(unix))]
pub struct Client<S = TcpStream> {
    stream: S,
}

#[cfg(unix)]
impl Client<UnixStream> {
    /// Connect to the broker's Unix socket at `socket_path` (same machine,
    /// unsealed — reachability is scoped by the socket path's permissions).
    pub fn connect(socket_path: impl AsRef<Path>) -> io::Result<Client<UnixStream>> {
        Ok(Client {
            stream: UnixStream::connect(socket_path)?,
        })
    }
}

impl Client<TcpStream> {
    /// Connect to the broker over PLAINTEXT TCP: multi-machine, but for a trusted
    /// network only — the bytes are neither encrypted nor authenticated. For an
    /// untrusted network use `connect_tcp_sealed` (the `aead` feature), which
    /// carries the same protocol inside an authenticated-encryption record layer.
    pub fn connect_tcp(addr: impl ToSocketAddrs) -> io::Result<Client<TcpStream>> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        Ok(Client { stream })
    }
}

#[cfg(feature = "aead")]
impl Client<astream_aead::SealedStream<TcpStream>> {
    /// Connect to a broker served by [`serve_tcp_sealed`](crate::Broker::serve_tcp_sealed)
    /// over an XChaCha20-Poly1305-sealed transport under the pre-shared `key`. Runs
    /// the sealed handshake before returning — a fresh hello from each side, bound
    /// with the direction and the record sequence into every record's AAD, and a
    /// key-confirming record each way — so a broker that lacks the key is refused
    /// HERE (the connect fails; nothing but an empty confirm was sent under the
    /// key), and a record recorded from any other connection or direction cannot
    /// be replayed into this one. The Frame protocol is unchanged; it just rides
    /// inside an encrypted, ordered record layer. The handshake has a deadline, so
    /// an unresponsive peer cannot hang the connect.
    pub fn connect_tcp_sealed(
        addr: impl ToSocketAddrs,
        key: [u8; astream_aead::KEY_LEN],
    ) -> io::Result<Client<astream_aead::SealedStream<TcpStream>>> {
        // How long the broker may take to complete the handshake; lifted after it.
        const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let stream = astream_aead::SealedStream::handshake_client(stream, key)?;
        stream.get_ref().set_read_timeout(None)?;
        Ok(Client { stream })
    }
}

#[cfg(feature = "handshake")]
impl Client<astream_aead::SealedStream<TcpStream>> {
    /// Connect to a broker served by [`serve_tcp_handshake`](crate::Broker::serve_tcp_handshake),
    /// running the Rung-7 forward-secret handshake (ephemeral X25519 authenticated
    /// by the pre-shared `psk`) before the Frame protocol. Each connection derives a
    /// fresh session key, so a later `psk` compromise cannot decrypt this session's
    /// recorded traffic. A wrong `psk`, or a peer not speaking the handshake, fails
    /// the connect.
    pub fn connect_tcp_handshake(
        addr: impl ToSocketAddrs,
        psk: [u8; astream_aead::KEY_LEN],
    ) -> io::Result<Client<astream_aead::SealedStream<TcpStream>>> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        // Bound the handshake I/O so a broken or hostile server cannot hang the
        // client forever on the key-agreement read; reset to blocking afterward.
        let hs_timeout = std::time::Duration::from_secs(10);
        stream.set_read_timeout(Some(hs_timeout))?;
        stream.set_write_timeout(Some(hs_timeout))?;
        let sealed = astream_aead::client_handshake(stream, &psk)?;
        sealed.get_ref().set_read_timeout(None)?;
        sealed.get_ref().set_write_timeout(None)?;
        Ok(Client { stream: sealed })
    }
}

#[cfg(feature = "identity")]
impl Client<astream_aead::SealedStream<TcpStream>> {
    /// Connect to a broker served by [`serve_tcp_identity`](crate::Broker::serve_tcp_identity)
    /// with **static public-key identity, no shared secret** (Rung 8). Runs a
    /// mutual signed-DH (SIGMA/Ed25519) handshake: the broker must present exactly
    /// `expected_server` (its pinned host key), and we prove our own `identity`
    /// (which the broker must have allow-listed). The session key comes from the
    /// ephemeral DH alone (forward-secret under identity-key compromise). A wrong
    /// host key, an unauthorized identity, or a bad signature fails the connect.
    pub fn connect_tcp_identity(
        addr: impl ToSocketAddrs,
        identity: astream_aead::IdentityKeypair,
        expected_server: [u8; astream_aead::ID_PUB_LEN],
    ) -> io::Result<Client<astream_aead::SealedStream<TcpStream>>> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        let hs_timeout = std::time::Duration::from_secs(10);
        stream.set_read_timeout(Some(hs_timeout))?;
        stream.set_write_timeout(Some(hs_timeout))?;
        let sealed = astream_aead::client_identity_handshake(stream, &identity, &expected_server)?;
        sealed.get_ref().set_read_timeout(None)?;
        sealed.get_ref().set_write_timeout(None)?;
        Ok(Client { stream: sealed })
    }
}

impl<S: Read + Write> Client<S> {
    /// Wrap an already-connected byte stream (any `Read + Write`, e.g. a boxed
    /// `dyn` stream chosen at runtime) as a client. The caller has already done
    /// whatever transport setup (nodelay, sealing) the stream needs; this is the
    /// inverse of [`into_stream`](Self::into_stream).
    pub fn from_stream(stream: S) -> Client<S> {
        Client { stream }
    }

    /// Give the underlying byte stream back (e.g. to box it behind a `dyn` trait so
    /// one code path serves every transport). The connection stays open.
    pub fn into_stream(self) -> S {
        self.stream
    }

    /// Ask the broker for this connection's NONCE — the value an [`attach`](Self::attach)
    /// proof is computed over. Answered by a guarded and an unguarded broker alike, so
    /// one client works against either.
    pub fn hello(&mut self) -> io::Result<[u8; 32]> {
        write_frame(&mut self.stream, &encode_request(&Request::Hello))?;
        match read_frame(&mut self.stream)? {
            Some(p) => match decode_response(&p) {
                Some(Response::Nonce { nonce }) => nonce.as_slice().try_into().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("broker nonce is {} bytes, expected 32", nonce.len()),
                    )
                }),
                Some(Response::Error { msg, .. }) => Err(io::Error::other(msg)),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected response",
                )),
            },
            None => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "broker closed",
            )),
        }
    }

    /// Add a grant to this connection's keyring by presenting a PROOF the caller has
    /// already computed over the nonce from [`hello`](Self::hello) — the primitive
    /// under [`attach`](Self::attach), for a caller that holds its tag somewhere this
    /// process cannot reach it (an agent, a signer). Blocks for the acknowledgement
    /// and returns its `(next, head)`; a refused attach is an error HERE, not a
    /// surprise at the next request.
    pub fn attach_with_proof(&mut self, grant: &str, proof: &[u8]) -> io::Result<(u64, u64)> {
        write_frame(
            &mut self.stream,
            &encode_request(&Request::Attach {
                grant: grant.to_string(),
                proof: proof.to_vec(),
            }),
        )?;
        match read_frame(&mut self.stream)? {
            Some(p) => match decode_response(&p) {
                Some(Response::Mark { next, head, .. }) => Ok((next, head)),
                Some(Response::Error { msg, .. }) => Err(io::Error::other(msg)),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected response",
                )),
            },
            None => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "broker closed",
            )),
        }
    }

    /// Add the capability `grant`/`tag` to this connection's keyring: `Hello` for the
    /// connection's nonce, then `Attach` carrying `HMAC-SHA256(tag, nonce ‖ grant)` —
    /// a PROOF OF POSSESSION, so the tag never crosses the wire and a captured attach
    /// cannot be replayed onto another connection. Blocks for the acknowledgement and
    /// returns its `(next, head)`.
    ///
    /// `Attach` APPENDS: several narrow grants on one connection are the intended
    /// shape (read one subtree, commit under another, publish as a bound principal in
    /// a third), and each request is authorized existentially over the ring. A
    /// repeated grant string replaces rather than growing it.
    ///
    /// # Errors
    /// The socket errors, or the broker refuses the attach (a proof that does not
    /// verify, a producer-id collision in the binding table, or a full keyring).
    #[cfg(feature = "cap")]
    pub fn attach(&mut self, grant: &str, tag: &[u8]) -> io::Result<(u64, u64)> {
        let tag: [u8; 32] = tag.try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("capability tag is {} bytes, expected 32", tag.len()),
            )
        })?;
        let nonce = self.hello()?;
        let proof = astream_cap::attach_proof(&tag, &nonce, grant);
        self.attach_with_proof(grant, &proof)
    }

    /// Without the `cap` feature there is no way to compute an attach PROOF: the tag
    /// stays on the client and the wire carries `HMAC-SHA256(tag, nonce ‖ grant)`
    /// instead, which needs the same vetted MAC a guarded broker uses. Build with
    /// `cap` (or compute the proof yourself and call
    /// [`attach_with_proof`](Self::attach_with_proof)).
    #[cfg(not(feature = "cap"))]
    pub fn attach(&mut self, _grant: &str, _tag: &[u8]) -> io::Result<(u64, u64)> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "presenting a capability needs the `cap` feature: an Attach carries a proof of \
             possession (HMAC-SHA256 over the broker's per-connection nonce), never the tag, \
             so the client must be able to compute that MAC",
        ))
    }

    /// Publish `body` to `subject`, deduped by `(producer_id, producer_seq)`. Blocks
    /// for the ack; returns `(offset, deduped)` — `deduped` true means a re-send
    /// returning the original offset.
    pub fn publish(
        &mut self,
        producer_id: u64,
        producer_seq: u64,
        subject: &str,
        body: &[u8],
    ) -> io::Result<(u64, bool)> {
        write_frame(
            &mut self.stream,
            &encode_request(&Request::Publish {
                producer_id,
                producer_seq,
                subject: subject.to_string(),
                body: body.to_vec(),
            }),
        )?;
        match read_frame(&mut self.stream)? {
            Some(p) => match decode_response(&p) {
                Some(Response::PublishAck { offset, deduped }) => Ok((offset, deduped)),
                Some(Response::Error { msg, .. }) => Err(io::Error::other(msg)),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected response",
                )),
            },
            None => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "broker closed",
            )),
        }
    }

    /// PIPELINED publish (send side): write a publish WITHOUT waiting for its ack, so a
    /// single connection can keep many in flight (they coalesce into one group-commit
    /// fsync at the broker). Pair each `send_publish` with a later
    /// [`recv_publish_ack`](Self::recv_publish_ack) in the SAME order — the broker
    /// streams acks back in request order. Interleave sends and receives within a
    /// bounded window to avoid filling the socket buffers.
    pub fn send_publish(
        &mut self,
        producer_id: u64,
        producer_seq: u64,
        subject: &str,
        body: &[u8],
    ) -> io::Result<()> {
        write_frame(
            &mut self.stream,
            &encode_request(&Request::Publish {
                producer_id,
                producer_seq,
                subject: subject.to_string(),
                body: body.to_vec(),
            }),
        )
    }

    /// PIPELINED publish (receive side): read the next ack in order, returning
    /// `(offset, deduped)`. See [`send_publish`](Self::send_publish).
    pub fn recv_publish_ack(&mut self) -> io::Result<(u64, bool)> {
        match read_frame(&mut self.stream)? {
            Some(p) => match decode_response(&p) {
                Some(Response::PublishAck { offset, deduped }) => Ok((offset, deduped)),
                Some(Response::Error { msg, .. }) => Err(io::Error::other(msg)),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected response",
                )),
            },
            None => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "broker closed",
            )),
        }
    }

    /// The largest in-flight window [`publish_pipelined`](Self::publish_pipelined) will
    /// use, regardless of the requested window. Kept well under the broker's
    /// `PIPELINE_DEPTH` so the interleaved send/recv loop can never wedge the connection
    /// (a window at/above the broker's bound + non-draining could deadlock until the
    /// broker's ack write-timeout).
    pub const MAX_PIPELINE_WINDOW: usize = 512;

    /// Convenience windowed pipeline: publish `bodies` under `producer_id` (seqs
    /// `1..=len`), keeping at most `window` acks outstanding, and return the acks in
    /// order. Far faster than calling [`publish`](Self::publish) in a loop because the
    /// in-flight publishes share group-commit fsyncs — one connection, many in flight.
    /// The effective window is clamped to `[1, MAX_PIPELINE_WINDOW]` so it cannot
    /// out-run the broker's bounded ack queue.
    pub fn publish_pipelined(
        &mut self,
        producer_id: u64,
        subject: &str,
        bodies: &[&[u8]],
        window: usize,
    ) -> io::Result<Vec<(u64, bool)>> {
        let window = window.clamp(1, Self::MAX_PIPELINE_WINDOW);
        let mut acks = Vec::with_capacity(bodies.len());
        let mut sent = 0usize;
        while acks.len() < bodies.len() {
            // Top the in-flight window back up.
            while sent < bodies.len() && sent - acks.len() < window {
                self.send_publish(producer_id, (sent + 1) as u64, subject, bodies[sent])?;
                sent += 1;
            }
            // Then drain one ack (keeps sends and receives interleaved, no deadlock).
            acks.push(self.recv_publish_ack()?);
        }
        Ok(acks)
    }

    /// REGISTER A LAST WILL on this connection: the record the broker appends on your
    /// behalf when the connection ends, for any reason — a clean close, a crash, a
    /// cable pull the broker eventually notices. Blocks for the acknowledgement and
    /// returns its `(next, head)`.
    ///
    /// One per connection: a second call replaces the first. The will fires as an
    /// ordinary publish under `(producer_id, producer_seq)`, so publishing that key
    /// yourself first makes it a no-op (an exactly-once goodbye), and it is FENCED —
    /// it appends nothing if a record with a higher `producer_seq` from the same
    /// producer has landed since.
    ///
    /// The fence reads sequences BELOW [`ACK_SEQ_BASE`] only: an ack's sequence is
    /// derived from an offset rather than an incarnation, so acking under this producer
    /// id does not suppress this will. For the same reason a `producer_seq` in the
    /// reserved half is REFUSED here — the fence could not protect a will that lives
    /// where it does not look.
    pub fn will(
        &mut self,
        producer_id: u64,
        producer_seq: u64,
        subject: &str,
        body: &[u8],
    ) -> io::Result<(u64, u64)> {
        write_frame(
            &mut self.stream,
            &encode_request(&Request::Will {
                producer_id,
                producer_seq,
                subject: subject.to_string(),
                body: body.to_vec(),
            }),
        )?;
        match read_frame(&mut self.stream)? {
            Some(p) => match decode_response(&p) {
                Some(Response::Mark { next, head, .. }) => Ok((next, head)),
                Some(Response::Error { msg, .. }) => Err(io::Error::other(msg)),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected response",
                )),
            },
            None => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "broker closed",
            )),
        }
    }

    /// LAST-VALUE query: the most recent record of every subject matching `filter`
    /// whose subject sorts strictly after `after` (`""` for the first page), at most
    /// `max`, in ascending SUBJECT order. Returns the page and the `(next, head)` of
    /// the closing `Mark`.
    ///
    /// `next` pairs with THIS PAGE, and with no other: it is the head this page was
    /// read at, so a `subscribe` from it tails gap-free and dup-free from THIS
    /// snapshot. A PAGED reader must therefore tail from the FIRST page's `next` and
    /// fold newest-wins across the pages — later pages are read at later heads, and
    /// subscribing from the LAST page's `next` silently skips any record that
    /// superseded a value an earlier page already reported.
    ///
    /// The broker CLAMPS `max` and bounds its scan of the subject index, so a short
    /// page does not mean the answer ended. Use
    /// [`last_page`](Self::last_page) whenever the whole answer matters: it returns the
    /// cursor that says whether there is more.
    ///
    /// Non-terminal: the connection is fully usable afterwards (this is the one read
    /// verb that does NOT consume the client).
    pub fn last(&mut self, filter: &str, after: &str, max: u32) -> io::Result<Page> {
        self.last_page(filter, after, max)
            .map(|(page, mark, _)| (page, mark))
    }

    /// [`last`](Self::last) with the RESUME CURSOR: `(page, (next, head), resume)`.
    ///
    /// `resume` is the `after` for the next page; an EMPTY `resume` means the broker's
    /// scan reached the end of the filter's range and this was the last page. Page
    /// until it is empty — a page shorter than `max` is NOT the end, because the
    /// broker bounds how many index entries one request may visit (matched or not) as
    /// well as how many rows it may return.
    pub fn last_page(
        &mut self,
        filter: &str,
        after: &str,
        max: u32,
    ) -> io::Result<(Vec<Record>, (u64, u64), String)> {
        write_frame(
            &mut self.stream,
            &encode_request(&Request::Last {
                filter: filter.to_string(),
                after: after.to_string(),
                max,
            }),
        )?;
        self.read_page_resumable()
    }

    /// BOUNDED READ: at most `max` records matching `filter` with offset >= `from`,
    /// scanning at most the broker's [`FETCH_SCAN_MAX`](crate::broker::FETCH_SCAN_MAX)
    /// records. Returns the page and the `(next, head)` of its closing `Mark`; `next`
    /// is the offset AFTER the last record the broker SCANNED, so paging with it
    /// advances even when a page matched nothing. `max = 0` is the head query.
    ///
    /// Non-terminal: unlike `subscribe` it never tails and does not consume the
    /// client — the connection is fully usable afterwards.
    pub fn fetch(&mut self, from: u64, filter: &str, max: u32) -> io::Result<Page> {
        write_frame(
            &mut self.stream,
            &encode_request(&Request::Fetch {
                from_offset: from,
                filter: filter.to_string(),
                max,
            }),
        )?;
        self.read_page()
    }

    /// Read `Delivery*` then the closing `Mark` of a bounded, non-terminal read,
    /// keeping the `Mark`'s subject cursor.
    ///
    /// PRECONDITION: no pipelined publish is un-acked on this connection. The broker
    /// flushes pending acks BEFORE the page (they arrive first, in order), so a caller
    /// that owes itself acks must drain them first; the blocking `publish` never does.
    fn read_page_resumable(&mut self) -> io::Result<(Vec<Record>, (u64, u64), String)> {
        let mut page = Vec::new();
        loop {
            match read_frame(&mut self.stream)? {
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "broker closed",
                    ))
                }
                Some(p) => match decode_response(&p) {
                    Some(Response::Delivery {
                        offset,
                        subject,
                        body,
                    }) => page.push((offset, subject, body)),
                    Some(Response::Mark { next, head, resume }) => {
                        return Ok((page, (next, head), resume))
                    }
                    Some(Response::Error { msg, .. }) => return Err(io::Error::other(msg)),
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "unexpected response",
                        ))
                    }
                },
            }
        }
    }

    /// [`read_page_resumable`](Self::read_page_resumable) without the cursor — for the
    /// verbs whose `Mark` never carries one.
    fn read_page(&mut self) -> io::Result<Page> {
        self.read_page_resumable()
            .map(|(page, mark, _)| (page, mark))
    }

    /// Subscribe to every record matching `filter` from `from_offset`, then live.
    /// Consumes the client (the connection becomes a delivery stream).
    pub fn subscribe(self, from_offset: u64, filter: &str) -> io::Result<Subscription<S>> {
        let mut stream = self.stream;
        write_frame(
            &mut stream,
            &encode_request(&Request::Subscribe {
                from_offset,
                filter: filter.to_string(),
            }),
        )?;
        Ok(Subscription {
            stream,
            carry: Vec::new(),
        })
    }

    /// Durably advance consumer `group`'s committed offset to `upto` (a pure commit).
    /// Blocks for the ack; returns the commit record's offset.
    pub fn commit(&mut self, group: &str, upto: u64) -> io::Result<u64> {
        write_frame(
            &mut self.stream,
            &encode_request(&Request::Commit {
                group: group.to_string(),
                upto,
            }),
        )?;
        self.read_ack()
    }

    /// The read-process-write TRANSACTION: append `out_body` to `out_subject` AND
    /// commit `group` to `upto` atomically (one durable record), deduped by
    /// `(producer_id, producer_seq)`. Returns `(offset, deduped)`.
    #[allow(clippy::too_many_arguments)]
    pub fn process_and_produce(
        &mut self,
        producer_id: u64,
        producer_seq: u64,
        out_subject: &str,
        out_body: &[u8],
        group: &str,
        upto: u64,
    ) -> io::Result<(u64, bool)> {
        write_frame(
            &mut self.stream,
            &encode_request(&Request::ProcessAndProduce {
                producer_id,
                producer_seq,
                out_subject: out_subject.to_string(),
                out_body: out_body.to_vec(),
                group: group.to_string(),
                upto,
            }),
        )?;
        match read_frame(&mut self.stream)? {
            Some(p) => match decode_response(&p) {
                Some(Response::PublishAck { offset, deduped }) => Ok((offset, deduped)),
                Some(Response::Error { msg, .. }) => Err(io::Error::other(msg)),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected response",
                )),
            },
            None => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "broker closed",
            )),
        }
    }

    fn read_ack(&mut self) -> io::Result<u64> {
        match read_frame(&mut self.stream)? {
            Some(p) => match decode_response(&p) {
                Some(Response::PublishAck { offset, .. }) => Ok(offset),
                Some(Response::Error { msg, .. }) => Err(io::Error::other(msg)),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected response",
                )),
            },
            None => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "broker closed",
            )),
        }
    }

    /// Subscribe as consumer `group`: the broker resumes delivery from the group's
    /// DURABLE committed offset (no client-tracked cursor), then tails live.
    pub fn subscribe_group(self, group: &str, filter: &str) -> io::Result<Subscription<S>> {
        let mut stream = self.stream;
        write_frame(
            &mut stream,
            &encode_request(&Request::SubscribeGroup {
                group: group.to_string(),
                filter: filter.to_string(),
            }),
        )?;
        Ok(Subscription {
            stream,
            carry: Vec::new(),
        })
    }

    /// COUNTERFACTUAL fork-delivery: stream the alternate timeline that results from
    /// forking the topic at `fork_at` and swapping that record for
    /// `(replacement_subject, replacement_body)`, filtered by `filter`. The live log
    /// is unchanged; the snapshot is delivered then the stream ends (`recv` → `None`).
    pub fn fork_subscribe(
        self,
        fork_at: u64,
        replacement_subject: &str,
        replacement_body: &[u8],
        filter: &str,
    ) -> io::Result<Subscription<S>> {
        let mut stream = self.stream;
        write_frame(
            &mut stream,
            &encode_request(&Request::ForkSubscribe {
                fork_at,
                replacement_subject: replacement_subject.to_string(),
                replacement_body: replacement_body.to_vec(),
                filter: filter.to_string(),
            }),
        )?;
        Ok(Subscription {
            stream,
            carry: Vec::new(),
        })
    }
}

/// Take at most `max` records from a group subscription WITHOUT committing anything.
///
/// Stops early on end-of-stream, or when a read TIMES OUT — so a caller that wants
/// "whatever has arrived within an idle window" sets that window with
/// [`Subscription::set_read_timeout`] first. That method exists on the three
/// transports whose socket is reachable — `Subscription<UnixStream>` (Unix builds),
/// `Subscription<TcpStream>` and (feature `aead`) the sealed
/// `Subscription<SealedStream<TcpStream>>`, which the `handshake` and `identity`
/// transports also produce. On any OTHER stream type there is no idle window: bound
/// the socket yourself before subscribing (reach it with
/// [`Subscription::get_ref`]), or this parks in the read until the broker sends
/// something.
///
/// The idle stop is frame-atomic: a timeout that lands part-way through a record
/// keeps that record's bytes on the subscription and resumes it on the next call
/// (see [`Subscription::recv_event`]), so the same `sub` can be taken from again.
/// The records are already durable and the group's cursor has NOT moved, so a
/// caller that dies here sees them again.
pub fn take<S: Read + Write>(sub: &mut Subscription<S>, max: usize) -> io::Result<Vec<Record>> {
    let mut out = Vec::with_capacity(max.min(64));
    while out.len() < max {
        match sub.recv() {
            Ok(Some(rec)) => out.push(rec),
            Ok(None) => break, // the broker closed
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                break; // idle: nothing more right now
            }
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// THE TWO-CONNECTION DRAIN: take at most `max` records from `sub` (a group
/// subscription) and then durably commit `group` through the last one on
/// `committer` — a SECOND connection, because `subscribe_group` consumes its client
/// and a group subscription's own connection is a one-way delivery stream. That price
/// is the reason this is a library helper rather than something every caller
/// re-invents.
///
/// The order is deliberate and is what makes redelivery the failure mode: the commit
/// happens AFTER the records are in the caller's hands, so a crash between the two
/// redelivers them (at-least-once into an idempotent sink), never loses them. A
/// caller that must process before committing calls [`take`] and commits itself.
///
/// Returns the records; an empty result commits nothing.
pub fn drain<S: Read + Write, C: Read + Write>(
    sub: &mut Subscription<S>,
    committer: &mut Client<C>,
    group: &str,
    max: usize,
) -> io::Result<Vec<Record>> {
    let records = take(sub, max)?;
    if let Some((last, _, _)) = records.last() {
        committer.commit(group, *last)?;
    }
    Ok(records)
}

/// The top half of the producer sequence space, RESERVED for [`ack`].
///
/// Defined by the LOG ([`crate::store::ACK_SEQ_BASE`]) and re-exported here, so the
/// derivation below and the store's last-will fence read one constant: acks occupy
/// sequences at or above this value, ordinary publishes must stay below it, and the
/// fence — which asks whether a producer has been heard from since it registered its
/// will — ignores the reserved half entirely. `asb` enforces the lower half on `--seq`
/// and `--seq-file`.
pub use crate::store::ACK_SEQ_BASE;

/// THE READ-PROCESS-WRITE ACK: append the ack record AND advance `group` past
/// `offset` in ONE durable record, so the answer and the cursor can never disagree.
///
/// The `producer_seq` is [`ACK_SEQ_BASE`]` | offset`, which is what makes a retry
/// idempotent: a repeated ack for the same offset returns `deduped = true`, appends
/// nothing, and re-applies no commit — and it does so whether the retry came through
/// this helper or through `asb ack`, because both derive the key the same way.
///
/// HONEST BOUNDARY: `offset` must be below [`ACK_SEQ_BASE`] (any real log offset is,
/// by many orders of magnitude) and ordinary publishes by the same `producer_id` must
/// stay below it too, which `asb` enforces on `--seq`/`--seq-file` and a library caller
/// must observe itself. Within those bounds acks and ordinary publishes by one producer
/// id cannot collide in the dedup map, so a separate acking id is no longer required
/// for that.
///
/// THAT IS A STATEMENT ABOUT DEDUP, AND ABOUT NOTHING ELSE. An ack does not raise the
/// producer's last-will fence: the reserved half is excluded from
/// `BrokerLog::producer_high_water`, precisely because an ack's sequence is derived
/// from an offset rather than from an incarnation and is above every sequence a will
/// can hold. Acking under the producer id that also holds a will is therefore safe —
/// it was not, and one ack suppressed that producer's goodbye permanently — and a
/// will registered at a sequence in the reserved half is refused at registration.
///
/// SECOND HONEST BOUNDARY — THIS DERIVATION CHANGED, AND THE CHANGE IS NOT UPGRADE
/// COMPATIBLE. This helper previously keyed an ack on the BARE `offset`. An ack already
/// on the log from a pre-change caller therefore sits under `(producer_id, offset)`,
/// while a retry of that same logical ack from this version is keyed
/// `(producer_id, ACK_SEQ_BASE | offset)` — a different key, so the broker sees a new
/// record and appends a second answer instead of deduping. `PROTO_VERSION` cannot catch
/// this: the frames are well formed and current, and the incompatibility lives in the
/// DURABLE dedup map rather than in the codec. Drain a producer's in-flight acks before
/// upgrading it, or accept at most one duplicated answer per ack whose outcome was in
/// doubt across the upgrade. The group commit is monotone, so the CURSOR is unaffected
/// either way; only the answer record can double.
pub fn ack<S: Read + Write>(
    client: &mut Client<S>,
    producer_id: u64,
    group: &str,
    offset: u64,
    out_subject: &str,
    out_body: &[u8],
) -> io::Result<(u64, bool)> {
    debug_assert!(
        offset < ACK_SEQ_BASE,
        "an ack's input offset must stay in the lower half of the sequence space"
    );
    client.process_and_produce(
        producer_id,
        ACK_SEQ_BASE | offset,
        out_subject,
        out_body,
        group,
        offset,
    )
}

/// A live delivery stream from the broker.
#[cfg(unix)]
pub struct Subscription<S = UnixStream> {
    stream: S,
    /// The bytes of the frame currently being assembled that a TIMED-OUT read
    /// already took off the stream. Empty at every frame boundary; see
    /// [`recv_event`](Subscription::recv_event).
    carry: Vec<u8>,
}

/// A live delivery stream from the broker (Windows twin: TCP default, as above).
#[cfg(not(unix))]
pub struct Subscription<S = TcpStream> {
    stream: S,
    /// The bytes of the frame currently being assembled that a TIMED-OUT read
    /// already took off the stream (as above).
    carry: Vec<u8>,
}

#[cfg(unix)]
impl Subscription<UnixStream> {
    /// A handle that can end this subscription from ANOTHER thread: `recv` blocks
    /// in a socket read with no timeout, so an owner that wants to stop a tailing
    /// thread (and release the broker-side connection) shuts the socket down through
    /// this closer, which makes the blocked `recv` return `Ok(None)`.
    pub fn closer(&self) -> io::Result<SubscriptionCloser> {
        Ok(SubscriptionCloser {
            stream: self.stream.try_clone()?,
        })
    }
}

/// Ends a Unix-socket [`Subscription`] from outside its reading thread (see
/// [`Subscription::closer`]). Dropping the closer does nothing; `close` is explicit.
#[cfg(unix)]
pub struct SubscriptionCloser {
    stream: UnixStream,
}

#[cfg(unix)]
impl SubscriptionCloser {
    /// Shut the subscription's socket down in both directions: the reader's
    /// blocked `recv` returns `Ok(None)` and the broker sees the peer go away.
    /// Idempotent; an already-closed socket is not an error.
    pub fn close(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// What a subscription can carry: a record, or the `Mark` that closes a bounded
/// read. [`Subscription::recv`] keeps its signature and SKIPS marks; a caller that
/// needs the cursor reads [`recv_event`](Subscription::recv_event) instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// One delivered record: `(offset, subject, body)`.
    Delivery {
        offset: u64,
        subject: String,
        body: Vec<u8>,
    },
    /// The end of a bounded read: resume from `next`; `head` is the
    /// subscriber-visible head the answer was read with.
    Mark { next: u64, head: u64 },
}

/// The partial-frame buffer a subscription keeps between frames: big enough that an
/// ordinary record never reallocates, small enough that a 16 MiB one is not held for
/// the life of the subscription. Matches `proto`'s read chunk.
const CARRY_RESTING: usize = 64 * 1024;

/// A subscription's stream as `read_frame` sees it: the socket, plus the bytes of
/// the frame currently being assembled.
///
/// `read_frame` is built out of `read_exact`, which on a timeout returns the error
/// having ALREADY consumed whatever bytes it managed to read — into a buffer it then
/// drops. Over `SO_RCVTIMEO` that silently eats a prefix of a record and leaves the
/// rest queued, so the next read parses a body as a header. This adapter closes that
/// hole the same way the sealed record layer closes it one level down: every byte
/// handed to `read_frame` is appended to `carry`, and the next call replays `carry`
/// from the start (`pos`) before reading the socket again. `read_frame` asks for the
/// same bytes in the same order every time, so the replay reconstructs exactly the
/// prefix the timed-out call had, and the frame finishes from where it stopped.
///
/// The cost is that a frame in flight is held twice — once in `carry`, once in the
/// buffer `read_frame` is filling. Both grow with what the peer has actually SENT,
/// not with the length it declared, so a peer that announces a huge frame and stalls
/// still pins only what it sent (see `proto::READ_CHUNK`), now doubled.
struct Resume<'a, S> {
    stream: &'a mut S,
    carry: &'a mut Vec<u8>,
    pos: usize,
}

impl<S: Read> Read for Resume<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos < self.carry.len() {
            let n = buf.len().min(self.carry.len() - self.pos);
            buf[..n].copy_from_slice(&self.carry[self.pos..self.pos + n]);
            self.pos += n;
            return Ok(n);
        }
        let n = self.stream.read(buf)?;
        self.carry.extend_from_slice(&buf[..n]);
        self.pos += n;
        Ok(n)
    }
}

impl<S: Read + Write> Subscription<S> {
    /// The underlying byte stream, for transport-level control — a timeout, a
    /// shutdown — that does not touch the record bytes. This is the escape hatch for
    /// a stream type that has no [`set_read_timeout`](Self::set_read_timeout) of its
    /// own (a boxed `dyn` stream, say): reach the socket through it and bound the
    /// read there. Do NOT read or write the stream itself — a subscription that has
    /// timed out mid-frame holds the rest of that frame, and bytes taken from
    /// underneath it are gone from the stream this resumes.
    pub fn get_ref(&self) -> &S {
        &self.stream
    }

    /// Block until the next matching delivery; `Ok(None)` if the broker closes.
    /// A `Mark` is SKIPPED (this method's signature is unchanged) — read
    /// [`recv_event`](Self::recv_event) to see it.
    pub fn recv(&mut self) -> io::Result<Option<Record>> {
        loop {
            match self.recv_event()? {
                None => return Ok(None),
                Some(Event::Mark { .. }) => continue,
                Some(Event::Delivery {
                    offset,
                    subject,
                    body,
                }) => return Ok(Some((offset, subject, body))),
            }
        }
    }

    /// Block until the next event — a delivery OR a `Mark`; `Ok(None)` if the broker
    /// closes.
    ///
    /// A read that TIMES OUT part-way through a frame (see
    /// [`set_read_timeout`](Subscription::set_read_timeout)) is resumable, not a
    /// desync: `read_frame` is fed through the `Resume` adapter, which keeps every
    /// byte it handed out for the unfinished frame and replays them before
    /// touching the socket again. The frame is then completed from where the
    /// timeout left it, so the caller sees the whole record once — never a prefix
    /// dropped on the floor and the tail of a body parsed as the next header. Any
    /// other error clears the partial frame; the stream is not resumable past it.
    pub fn recv_event(&mut self) -> io::Result<Option<Event>> {
        let mut r = Resume {
            stream: &mut self.stream,
            carry: &mut self.carry,
            pos: 0,
        };
        let framed = read_frame(&mut r);
        let timed_out = matches!(
            &framed,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
        );
        if !timed_out {
            // A frame boundary (or an error the stream cannot resume past): nothing is
            // owed. `clear` keeps the capacity, which is what makes the ordinary case
            // allocation-free — but one big record should not pin its buffer for the
            // life of the subscription, so hand back anything past the resting size.
            self.carry.clear();
            if self.carry.capacity() > CARRY_RESTING {
                self.carry.shrink_to(CARRY_RESTING);
            }
        }
        match framed? {
            Some(p) => match decode_response(&p) {
                Some(Response::Delivery {
                    offset,
                    subject,
                    body,
                }) => Ok(Some(Event::Delivery {
                    offset,
                    subject,
                    body,
                })),
                Some(Response::Mark { next, head, .. }) => Ok(Some(Event::Mark { next, head })),
                Some(Response::Error { msg, .. }) => Err(io::Error::other(msg)),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected response",
                )),
            },
            None => Ok(None),
        }
    }
}

#[cfg(unix)]
impl Subscription<UnixStream> {
    /// Bound how long [`recv`](Self::recv) / [`recv_event`](Self::recv_event) block
    /// with nothing arriving: past it the read returns `WouldBlock`/`TimedOut`, which
    /// is how [`drain`] stops on an idle stream instead of parking forever.
    ///
    /// The timeout is on the SOCKET, so it can land part-way through a record. That
    /// is not a desync: the partial frame stays on the subscription and the next
    /// `recv` finishes it (see [`recv_event`](Self::recv_event)), so the same `sub`
    /// is safe to drain from again.
    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(d)
    }
}

impl Subscription<TcpStream> {
    /// The TCP twin of [`Subscription::set_read_timeout`].
    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(d)
    }
}

#[cfg(feature = "aead")]
impl Subscription<astream_aead::SealedStream<TcpStream>> {
    /// The SEALED twin of [`Subscription::set_read_timeout`] — the same idle window
    /// for a subscription from `connect_tcp_sealed`, `connect_tcp_handshake` or
    /// `connect_tcp_identity` (all three hand back a `SealedStream<TcpStream>`).
    /// Without it a direct consumer on the fabric's own transport had no way to bound
    /// [`drain`] and parked in the read forever.
    ///
    /// The timeout is set on the TCP socket UNDER the record layer, so it can land
    /// part-way through a sealed record as well as part-way through a frame. Both
    /// layers resume: the record layer keeps its partial record and its sequence
    /// counter (`astream_aead`'s `read_record`), and [`recv_event`](Self::recv_event)
    /// keeps the partial frame.
    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.stream.get_ref().set_read_timeout(d)
    }
}
