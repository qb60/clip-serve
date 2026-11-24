use std::{
    io::{self, Read, Write},
    net::{Shutdown, SocketAddrV4, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use polling::{Event, Events, Poller};

use crate::store::{SharedStore, valid_id};

const WORKERS: usize = 4;
const QUEUED: usize = 4;
const HEADER_LIMIT: usize = 8192;
const READ_TIMEOUT: Duration = Duration::from_secs(3);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_IDLE_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Server {
    endpoint: SocketAddrV4,
    healthy: Arc<AtomicBool>,
    accepting: Arc<AtomicBool>,
    poller: Arc<Poller>,
    threads: Vec<JoinHandle<()>>,
    active: Arc<[Mutex<Option<TcpStream>>; WORKERS]>,
}

impl Server {
    pub fn start(
        endpoint: SocketAddrV4,
        store: SharedStore,
        on_failure: impl FnOnce() + Send + 'static,
    ) -> io::Result<Self> {
        let server = Self::prepare(endpoint, store, on_failure)?;
        server.activate();
        Ok(server)
    }

    pub fn prepare(
        endpoint: SocketAddrV4,
        store: SharedStore,
        on_failure: impl FnOnce() + Send + 'static,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(endpoint)?;
        let std::net::SocketAddr::V4(endpoint) = listener.local_addr()? else {
            unreachable!("the listener is bound to IPv4");
        };
        listener.set_nonblocking(true)?;
        let poller = Arc::new(Poller::new()?);
        let healthy = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::sync_channel::<TcpStream>(QUEUED);
        let rx = Arc::new(Mutex::new(rx));
        let mut server = Self {
            endpoint,
            healthy,
            accepting: Arc::new(AtomicBool::new(false)),
            poller,
            threads: Vec::with_capacity(WORKERS + 1),
            active: Arc::new(std::array::from_fn(|_| Mutex::new(None))),
        };

        // Start acceptance first so a partially started pool can still be stopped.
        let poller = Arc::clone(&server.poller);
        let healthy = server.health();
        let (registered, registration) = mpsc::channel();
        server
            .threads
            .push(
                thread::Builder::new()
                    .name("HTTP accept".into())
                    .spawn(move || {
                        // The accept thread deletes the registration before dropping the listener.
                        let result = unsafe { poller.add(&listener, Event::readable(0)) };
                        let ready = result.is_ok();
                        let _ = registered.send(result);
                        if !ready {
                            return;
                        }
                        let result = accept(&listener, &poller, &healthy, &tx);
                        if let Err(err) = poller.delete(&listener) {
                            log::warn!("Could not deregister HTTP listener ({:?}).", err.kind());
                        }
                        drop(listener);
                        if let Err(err) = result
                            && healthy.swap(false, Ordering::AcqRel)
                        {
                            log::error!("HTTP listener stopped ({:?}).", err.kind());
                            on_failure();
                        }
                    })?,
            );

        registration.recv().map_err(io::Error::other)??;
        for index in 0..WORKERS {
            let active = Arc::clone(&server.active);
            let accepting = Arc::clone(&server.accepting);
            let (rx, store, healthy) = (Arc::clone(&rx), Arc::clone(&store), server.health());
            server
                .threads
                .push(
                    thread::Builder::new()
                        .name("HTTP worker".into())
                        .spawn(move || {
                            loop {
                                let next = rx.lock().unwrap().recv();
                                let Ok(mut socket) = next else { break };
                                {
                                    let mut slot = active[index].lock().unwrap();
                                    if !healthy.load(Ordering::Acquire) {
                                        break;
                                    }
                                    if !accepting.load(Ordering::Acquire) {
                                        continue;
                                    }
                                    let Ok(handle) = socket.try_clone() else {
                                        continue;
                                    };
                                    *slot = Some(handle);
                                }
                                // Clients can disconnect, time out or send invalid requests.
                                let _ = serve(&mut socket, &store, &healthy);
                                active[index].lock().unwrap().take();
                            }
                        })?,
                );
        }
        Ok(server)
    }

    pub fn activate(&self) {
        self.accepting.store(true, Ordering::Release);
    }

    pub fn endpoint(&self) -> SocketAddrV4 {
        self.endpoint
    }

    pub fn health(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.healthy)
    }

    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.healthy.store(false, Ordering::Release);
        if let Err(err) = self.poller.notify() {
            log::error!(
                "Could not wake HTTP listener for shutdown ({:?}).",
                err.kind()
            );
        }
        for slot in self.active.iter() {
            if let Some(socket) = slot.lock().unwrap().as_ref() {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
        for thread in self.threads.drain(..) {
            if thread.join().is_err() {
                log::error!("An HTTP thread panicked.");
            }
        }
    }
}

fn accept(
    listener: &TcpListener,
    poller: &Poller,
    healthy: &AtomicBool,
    tx: &SyncSender<TcpStream>,
) -> io::Result<()> {
    let mut events = Events::new();
    while healthy.load(Ordering::Acquire) {
        events.clear();
        poller.wait(&mut events, None)?;
        if !healthy.load(Ordering::Acquire) {
            break;
        }
        if events.is_empty() {
            continue;
        }
        // Drain a bounded batch so shutdown wins even under a steady stream of connections.
        for _ in 0..16 {
            match listener.accept() {
                Ok((socket, _)) => {
                    if socket.set_nonblocking(false).is_err() {
                        continue;
                    }
                    if let Err(TrySendError::Disconnected(_)) = tx.try_send(socket) {
                        return Err(io::ErrorKind::BrokenPipe.into());
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::Interrupted
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::ConnectionReset
                    ) =>
                {
                    continue;
                }
                Err(err) => return Err(err),
            }
        }
        poller.modify(listener, Event::readable(0))?;
    }
    Ok(())
}

fn route(path: &str) -> Option<&str> {
    let id = path.strip_prefix("/image/")?.strip_suffix(".png")?;
    valid_id(id).then_some(id)
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or(io::ErrorKind::TimedOut.into())
}

fn send(
    socket: &mut TcpStream,
    healthy: &AtomicBool,
    deadline: Instant,
    mut bytes: &[u8],
) -> io::Result<()> {
    while !bytes.is_empty() {
        if !healthy.load(Ordering::Acquire) {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        socket.set_write_timeout(Some(remaining(deadline)?.min(WRITE_IDLE_TIMEOUT)))?;
        match socket.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn serve(socket: &mut TcpStream, store: &SharedStore, healthy: &AtomicBool) -> io::Result<()> {
    let read_deadline = Instant::now() + READ_TIMEOUT;
    let mut buffer = [0; HEADER_LIMIT];
    let mut used = 0;
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut request = httparse::Request::new(&mut headers);

    let status = loop {
        if !healthy.load(Ordering::Acquire) {
            return Ok(());
        }
        if used == buffer.len() {
            break "431 Request Header Fields Too Large";
        }
        socket.set_read_timeout(Some(remaining(read_deadline)?))?;
        match socket.read(&mut buffer[used..]) {
            Ok(0) => return Ok(()),
            Ok(n) => used += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
        if !buffer[..used].windows(4).any(|part| part == b"\r\n\r\n") {
            continue;
        }
        break match request.parse(&buffer[..used]) {
            Ok(httparse::Status::Complete(_)) => "",
            Err(httparse::Error::TooManyHeaders) => "431 Request Header Fields Too Large",
            _ => "400 Bad Request",
        };
    };

    let deadline = Instant::now() + WRITE_TIMEOUT;
    let method = request.method.unwrap_or_default();
    let (status, extra, png) = if !status.is_empty() {
        (status, "", None)
    } else if !matches!(method, "GET" | "HEAD") {
        ("405 Method Not Allowed", "Allow: GET, HEAD\r\n", None)
    } else if let Some(png) = request
        .path
        .and_then(route)
        .and_then(|id| store.lock().unwrap().get(id))
    {
        ("200 OK", "Content-Type: image/png\r\n", Some(png))
    } else {
        ("404 Not Found", "", None)
    };
    let length = png.as_ref().map_or(0, |png| png.len());
    let header = format!(
        "HTTP/1.1 {status}\r\n{extra}Content-Length: {length}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n"
    );
    send(socket, healthy, deadline, header.as_bytes())?;
    if method != "HEAD"
        && let Some(png) = png
    {
        send(socket, healthy, deadline, &png)?;
    }
    Ok(())
}
