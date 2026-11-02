// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Measure split, two-byte request/reply frames over admitted fleet TLS.
//! With no arguments, use loopback. Paired LAN runs use listen IP:PORT and connect IP:PORT.
//! Each mode gets a fresh connection; timings are observations, never pass thresholds.

#[cfg(unix)]
mod probe {
    use aterm_net::{
        fleet::{self, FleetOp, FleetScope, FleetSecret},
        lan::{Connection, Listener, Scope},
    };
    use std::{
        io::{self, Read, Write},
        net::{SocketAddr, TcpStream},
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    type Error = Box<dyn std::error::Error + Send + Sync>;
    const ROUNDS: usize = 32;
    const HANG: Duration = Duration::from_secs(60);
    const MODES: [(&str, Option<bool>); 3] = [
        ("default", None),
        ("coalesced", Some(false)),
        ("immediate", Some(true)),
    ];

    fn configure(stream: &TcpStream, requested: Option<bool>) -> io::Result<bool> {
        if let Some(enabled) = requested {
            stream.set_nodelay(enabled)?;
        }
        stream.set_read_timeout(Some(HANG))?;
        stream.set_write_timeout(Some(HANG))?;
        stream.nodelay()
    }

    fn frame(stream: &mut impl Write, bytes: [u8; 2]) -> io::Result<()> {
        stream.write_all(&bytes[..1])?;
        stream.write_all(&bytes[1..])?;
        stream.flush()
    }

    fn serve(listener: &Listener, requested: Option<bool>) -> Result<bool, Error> {
        let until = Instant::now() + HANG;
        let accepted = loop {
            match listener.accept() {
                Ok(accepted) => break accepted,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= until {
                        return Err("accept exceeded its hang detector".into());
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => return Err(error.into()),
            }
        };
        let (tcp, proof) = accepted.into_parts();
        configure(&tcp, requested)?;
        let secret = FleetSecret::new(&[7; 32])?;
        let (mut channel, _) =
            fleet::server_handshake(tcp, &secret, FleetScope::Facts, &[FleetOp::Survey], HANG)?;
        let nodelay = configure(channel.stream().get_ref(), requested)?;
        channel.stream().write_all(&[u8::from(nodelay)])?;
        channel.stream().flush()?;
        for sequence in 0..=ROUNDS {
            let mut bytes = [0; 2];
            channel.stream().read_exact(&mut bytes)?;
            if bytes != [sequence as u8, !(sequence as u8)] {
                return Err("request bytes changed".into());
            }
            frame(channel.stream(), bytes)?;
        }
        proof.recheck()?;
        Ok(nodelay)
    }

    fn measure(
        address: SocketAddr,
        scope: Scope,
        name: &str,
        requested: Option<bool>,
    ) -> Result<bool, Error> {
        let (tcp, proof) = Connection::connect(address, scope, HANG)?.into_parts();
        configure(&tcp, requested)?;
        let secret = FleetSecret::new(&[7; 32])?;
        let (mut channel, _) = fleet::client_handshake(tcp, &secret, FleetOp::Survey, HANG)?;
        let client_nodelay = configure(channel.stream().get_ref(), requested)?;
        let mut server_option = [0];
        channel.stream().read_exact(&mut server_option)?;
        if server_option[0] > 1 {
            return Err("invalid server socket option".into());
        }
        let server_nodelay = server_option[0] == 1;
        let mut samples = Vec::with_capacity(ROUNDS);
        for sequence in 0..=ROUNDS {
            let bytes = [sequence as u8, !(sequence as u8)];
            let started = Instant::now();
            frame(channel.stream(), bytes)?;
            let mut answer = [0; 2];
            channel.stream().read_exact(&mut answer)?;
            if answer != bytes {
                return Err("reply bytes changed".into());
            }
            if sequence != 0 {
                samples.push(started.elapsed().as_micros());
            }
        }
        proof.recheck()?;
        let total_us: u128 = samples.iter().sum();
        let mut sorted = samples.clone();
        sorted.sort_unstable();
        println!(
            "mode={name} client_nodelay={client_nodelay} server_nodelay={server_nodelay} \
             n={ROUNDS} total_us={total_us} p50_us={} p95_us={} samples_us={samples:?}",
            sorted[(ROUNDS - 1) / 2],
            sorted[(ROUNDS * 95).div_ceil(100) - 1],
        );
        Ok(server_nodelay)
    }

    pub fn run() -> Result<(), Error> {
        let (done, end) = mpsc::channel::<()>();
        let watchdog = thread::spawn(move || {
            if matches!(end.recv_timeout(HANG), Err(mpsc::RecvTimeoutError::Timeout)) {
                eprintln!("short-frame probe exceeded its one-minute hang detector");
                std::process::exit(124);
            }
        });
        let arguments: Vec<_> = std::env::args().skip(1).collect();
        match arguments.as_slice() {
            [] => {
                for (name, requested) in MODES {
                    let listener = Listener::bind("127.0.0.1:0".parse()?, Scope::Loopback)?;
                    let address = listener.local_addr()?;
                    let server = thread::spawn(move || serve(&listener, requested));
                    let received = measure(address, Scope::Loopback, name, requested)?;
                    let observed = server.join().map_err(|_| "server thread panicked")??;
                    if observed != received {
                        return Err("server option did not match its report".into());
                    }
                }
            }
            [verb, address] if verb == "listen" => {
                let listener = Listener::bind(address.parse()?, Scope::Lan)?;
                println!("listen={}", listener.local_addr()?);
                io::stdout().flush()?;
                for (name, requested) in MODES {
                    let nodelay = serve(&listener, requested)?;
                    println!("served={name} nodelay={nodelay} n={ROUNDS}");
                    io::stdout().flush()?;
                }
            }
            [verb, address] if verb == "connect" => {
                let address = address.parse()?;
                for (name, requested) in MODES {
                    measure(address, Scope::Lan, name, requested)?;
                }
            }
            _ => return Err("usage: short_frames [listen IP:PORT | connect IP:PORT]".into()),
        }
        let _ = done.send(());
        let _ = watchdog.join();
        Ok(())
    }
}

fn main() -> std::process::ExitCode {
    #[cfg(unix)]
    match probe::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
    #[cfg(not(unix))]
    {
        eprintln!("short-frame probe requires Unix");
        std::process::ExitCode::from(2)
    }
}
