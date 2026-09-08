//! The control socket: talking to a running `turborust up` from another terminal.
//!
//! Everything here exists to serve one thing — `turborust connect <node>` — but
//! the socket is deliberately built as a general seam rather than a special case,
//! because `restart`, `status` and `stop` will want the same channel and adding a
//! second mechanism later is how a CLI ends up with two ways to do everything.
//!
//! Note what this is *not*: a daemon. The socket lives and dies with the `up` that
//! created it, nothing starts it implicitly, and a stale one is reclaimed rather
//! than treated as an error. See item 0059.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Bumped when the framing changes. A client and server that disagree should say
/// so plainly rather than misparse each other.
pub const PROTOCOL: u32 = 1;

/// The control channel, which is a different primitive on each platform.
///
/// Unix gets a socket file; Windows gets a named pipe, because it has no
/// filesystem socket to place. Everything above this module is written against
/// byte streams, so only the listen-and-dial pair differs.
mod transport {
    use anyhow::{Context, Result};
    use std::path::Path;

    #[cfg(unix)]
    pub use unix::{Listener, connect};
    #[cfg(windows)]
    pub use windows::{Listener, connect};

    #[cfg(unix)]
    mod unix {
        use super::*;
        use tokio::net::{UnixListener, UnixStream};

        pub struct Listener(UnixListener);

        impl Listener {
            pub async fn bind(path: &Path) -> Result<Self> {
                let l = UnixListener::bind(path)
                    .with_context(|| format!("binding control socket {}", path.display()))?;
                Ok(Listener(l))
            }
            pub async fn accept(&mut self) -> Result<UnixStream> {
                Ok(self.0.accept().await?.0)
            }
        }

        pub async fn connect(path: &Path) -> Result<UnixStream> {
            Ok(UnixStream::connect(path).await?)
        }
    }

    #[cfg(windows)]
    mod windows {
        use super::*;
        use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};

        /// A named pipe serves one client per instance, so a fresh instance is
        /// created after each accept — that is the shape the API expects, not a
        /// workaround.
        pub struct Listener {
            name: String,
            next: NamedPipeServer,
        }

        impl Listener {
            pub async fn bind(path: &Path) -> Result<Self> {
                let name = path.to_string_lossy().to_string();
                let next = ServerOptions::new()
                    .first_pipe_instance(true)
                    .create(&name)
                    .with_context(|| format!("creating control pipe {name}"))?;
                Ok(Listener { name, next })
            }

            pub async fn accept(&mut self) -> Result<NamedPipeServer> {
                self.next.connect().await?;
                let ready =
                    std::mem::replace(&mut self.next, ServerOptions::new().create(&self.name)?);
                Ok(ready)
            }
        }

        pub async fn connect(
            path: &Path,
        ) -> Result<tokio::net::windows::named_pipe::NamedPipeClient> {
            Ok(ClientOptions::new().open(path.to_string_lossy().as_ref())?)
        }
    }
}

/// Where the control socket for a given workspace lives.
///
/// Not inside `.turborust/`, which would be the obvious place. Unix socket paths
/// are limited to about 104 bytes by `sockaddr_un`, and a workspace nested a few
/// directories deep blows through that — the failure is a bare EINVAL from
/// `bind`, which reads as "the socket is broken" rather than "your path is long".
///
/// So the socket is named by a hash of the workspace instead, in a per-user
/// directory. Both server and client derive it from the same canonical cache
/// directory, so they always agree without sharing state.
pub fn socket_path(cache_dir: &Path) -> PathBuf {
    let key = blake3::hash(cache_dir.to_string_lossy().as_bytes()).to_hex();

    // Named pipes live in their own namespace, with no path-length problem and
    // nothing to clean up.
    #[cfg(windows)]
    return PathBuf::from(format!(r"\\.\pipe\turborust-{}", &key[..12]));

    #[cfg(unix)]
    {
        let name = format!("{}.sock", &key[..12]);
        let preferred = runtime_dir(std::env::temp_dir()).join(&name);
        if preferred.as_os_str().len() < MAX_SOCKET_PATH {
            return preferred;
        }
        // A long TMPDIR is itself enough to overflow; /tmp always fits.
        runtime_dir(PathBuf::from("/tmp")).join(name)
    }
}

/// Conservative limit: `sun_path` is 104 bytes on macOS, 108 on Linux.
#[cfg(unix)]
const MAX_SOCKET_PATH: usize = 100;

/// Per-user, so one machine's sockets are not another user's to connect to.
#[cfg(unix)]
fn runtime_dir(base: PathBuf) -> PathBuf {
    let uid = unsafe { libc::getuid() };
    base.join(format!("turborust-{uid}"))
}

/// The opening request. One JSON line, so the socket stays debuggable with `nc`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Attach to a node's terminal.
    Attach { node: String, protocol: u32 },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    fn ok() -> Self {
        Response {
            ok: true,
            error: None,
        }
    }
    fn err(msg: impl Into<String>) -> Self {
        Response {
            ok: false,
            error: Some(msg.into()),
        }
    }
}

/// Frame kinds, after the handshake. Length-prefixed binary rather than JSON,
/// because terminal traffic is arbitrary bytes and escaping it would be both
/// slower and lossy.
pub mod frame {
    pub const OUTPUT: u8 = 0;
    pub const STDIN: u8 = 1;
    pub const RESIZE: u8 = 2;
}

/// Largest frame accepted, so a confused peer cannot ask for a huge allocation.
const MAX_FRAME: u32 = 1 << 20;

pub async fn write_frame<W: AsyncWriteExt + Unpin>(w: &mut W, kind: u8, body: &[u8]) -> Result<()> {
    w.write_u8(kind).await?;
    w.write_u32(body.len() as u32).await?;
    w.write_all(body).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_frame<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<(u8, Vec<u8>)> {
    let kind = r.read_u8().await?;
    let len = r.read_u32().await?;
    if len > MAX_FRAME {
        bail!("frame of {len} bytes exceeds the {MAX_FRAME} byte limit");
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).await?;
    Ok((kind, body))
}

/// Reads one newline-terminated JSON value from the stream.
async fn read_line<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<String> {
    let mut out = Vec::new();
    let mut byte = [0u8; 1];
    while out.len() < 4096 {
        if r.read_exact(&mut byte).await.is_err() {
            break;
        }
        if byte[0] == b'\n' {
            return Ok(String::from_utf8_lossy(&out).to_string());
        }
        out.push(byte[0]);
    }
    bail!("no request received")
}

async fn write_line<W: AsyncWriteExt + Unpin, T: Serialize>(w: &mut W, value: &T) -> Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    w.write_all(&line).await?;
    w.flush().await?;
    Ok(())
}

/// Tightens permissions on a path we just created.
#[cfg(unix)]
fn restrict(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("restricting {}", path.display()))?;
    Ok(())
}

/// Removes a socket left behind by a previous run.
///
/// A leftover file makes `bind` fail with "address in use", which is a confusing
/// way to report "the last run was killed". If something is genuinely listening,
/// connecting succeeds and we refuse instead.
#[cfg(unix)]
async fn reclaim(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if transport::connect(path).await.is_ok() {
        bail!(
            "another turborust is already running here ({}). Stop it first.",
            path.display()
        );
    }
    std::fs::remove_file(path).with_context(|| format!("removing stale {}", path.display()))?;
    Ok(())
}

/// Serves control connections until the process ends.
pub async fn serve(engine: std::sync::Arc<crate::engine::Engine>, path: PathBuf) -> Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        // The socket grants write access to a child's stdin, so it must not be
        // reachable by other users on a shared machine.
        #[cfg(unix)]
        restrict(parent, 0o700)?;
    }
    #[cfg(unix)]
    reclaim(&path).await?;
    let mut listener = transport::Listener::bind(&path).await?;
    #[cfg(unix)]
    restrict(&path, 0o600)?;

    loop {
        let Ok(stream) = listener.accept().await else {
            continue;
        };
        let engine = engine.clone();
        tokio::spawn(async move {
            let _ = handle_client(engine, stream).await;
        });
    }
}

async fn handle_client<S: AsyncRead + AsyncWrite + Unpin + Send>(
    engine: std::sync::Arc<crate::engine::Engine>,
    mut stream: S,
) -> Result<()> {
    let line = read_line(&mut stream).await?;
    let request: Request = match serde_json::from_str(&line) {
        Ok(r) => r,
        Err(e) => {
            write_line(&mut stream, &Response::err(format!("bad request: {e}"))).await?;
            return Ok(());
        }
    };

    match request {
        Request::Attach { node, protocol } => {
            if protocol != PROTOCOL {
                write_line(
                    &mut stream,
                    &Response::err(format!(
                        "protocol {protocol} but this turborust speaks {PROTOCOL}; \
                         the running server and your client are different versions"
                    )),
                )
                .await?;
                return Ok(());
            }
            attach(engine, stream, node).await
        }
    }
}

async fn attach<S: AsyncRead + AsyncWrite + Unpin + Send>(
    engine: std::sync::Arc<crate::engine::Engine>,
    mut stream: S,
    node: String,
) -> Result<()> {
    let Some(handle) = engine.handle_for(&node) else {
        let known = engine.node_names().join(", ");
        write_line(
            &mut stream,
            &Response::err(format!("`{node}` is not running (nodes: {known})")),
        )
        .await?;
        return Ok(());
    };

    // Only one terminal may own the input side; two people typing into the same
    // shell interleave into nonsense.
    if !engine.claim_attach(&node) {
        write_line(
            &mut stream,
            &Response::err(format!("`{node}` already has a terminal attached")),
        )
        .await?;
        return Ok(());
    }
    let _release = AttachGuard {
        engine: engine.clone(),
        node: node.clone(),
    };

    let mut output = handle.subscribe_raw();
    write_line(&mut stream, &Response::ok()).await?;

    // Replay the recent bytes, so attaching to a process already sitting at a
    // prompt shows the prompt rather than an empty screen.
    let backlog = handle.scrollback();
    if !backlog.is_empty() {
        write_frame(&mut stream, frame::OUTPUT, &backlog).await?;
    }

    // `tokio::io::split` rather than a transport-specific `into_split`, so the
    // same body serves a socket and a pipe.
    let (mut reader, mut writer) = tokio::io::split(stream);
    loop {
        tokio::select! {
            chunk = output.recv() => match chunk {
                Ok(bytes) => write_frame(&mut writer, frame::OUTPUT, &bytes).await?,
                // Lagged: the client fell behind. Skipping is correct for a
                // terminal — being current matters more than being complete.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return Ok(()),
            },
            incoming = read_frame(&mut reader) => match incoming {
                Ok((frame::STDIN, bytes)) => {
                    if !handle.write_stdin(&bytes) {
                        return Ok(());
                    }
                }
                Ok((frame::RESIZE, bytes)) => {
                    if let Some((rows, cols)) = parse_resize(&bytes) {
                        handle.resize(rows, cols);
                    }
                }
                Ok(_) => {}
                // The client detached or died.
                Err(_) => return Ok(()),
            },
        }
    }
}

/// Releases the single-attach claim however the session ends.
struct AttachGuard {
    engine: std::sync::Arc<crate::engine::Engine>,
    node: String,
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        self.engine.release_attach(&self.node);
    }
}

fn parse_resize(bytes: &[u8]) -> Option<(u16, u16)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (rows, cols) = text.split_once(',')?;
    Some((rows.trim().parse().ok()?, cols.trim().parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_round_trip() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        write_frame(&mut a, frame::STDIN, b"hello\x1b[0m\x00binary")
            .await
            .unwrap();
        let (kind, body) = read_frame(&mut b).await.unwrap();
        assert_eq!(kind, frame::STDIN);
        assert_eq!(body, b"hello\x1b[0m\x00binary");
    }

    #[tokio::test]
    async fn an_empty_frame_is_valid() {
        let (mut a, mut b) = tokio::io::duplex(64);
        write_frame(&mut a, frame::OUTPUT, b"").await.unwrap();
        let (kind, body) = read_frame(&mut b).await.unwrap();
        assert_eq!(kind, frame::OUTPUT);
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn an_absurd_length_is_refused_rather_than_allocated() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_u8(frame::OUTPUT).await.unwrap();
        a.write_u32(u32::MAX).await.unwrap();
        let err = read_frame(&mut b).await.unwrap_err().to_string();
        assert!(err.contains("exceeds"), "{err}");
    }

    #[tokio::test]
    async fn a_request_line_round_trips() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        write_line(
            &mut a,
            &Request::Attach {
                node: "api".into(),
                protocol: PROTOCOL,
            },
        )
        .await
        .unwrap();
        let line = read_line(&mut b).await.unwrap();
        let parsed: Request = serde_json::from_str(&line).unwrap();
        let Request::Attach { node, protocol } = parsed;
        assert_eq!(node, "api");
        assert_eq!(protocol, PROTOCOL);
    }

    #[test]
    fn resize_payloads_parse() {
        assert_eq!(parse_resize(b"40,120"), Some((40, 120)));
        assert_eq!(parse_resize(b" 40 , 120 "), Some((40, 120)));
        assert_eq!(parse_resize(b"garbage"), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_socket_file_is_reclaimed() {
        let dir = std::env::temp_dir().join(format!("tr-ctl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("control.sock");
        // A leftover file with nothing behind it: the previous run was killed.
        std::fs::write(&path, b"").unwrap();
        reclaim(&path).await.unwrap();
        assert!(
            !path.exists(),
            "a stale socket must be removed, not reported as in use"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_live_socket_is_not_reclaimed() {
        let dir = std::env::temp_dir().join(format!("tr-ctl-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("control.sock");
        let _listener = transport::Listener::bind(&path).await.unwrap();
        let err = reclaim(&path).await.unwrap_err().to_string();
        assert!(err.contains("already running"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ------------------------------------------------------------------ client ---

/// Ctrl-B, then `d`. Ctrl-C is deliberately *not* the detach key: it has to
/// reach the child, or attaching would be useless for the very programs that
/// most need a terminal. Overmind uses the same sequence; following an existing
/// convention beats inventing one.
const DETACH_LEAD: u8 = 0x02; // Ctrl-B
const DETACH_KEY: u8 = b'd';

/// Restores the terminal however the process leaves, including on panic.
///
/// A tool that exits with the terminal still in raw mode leaves the user with a
/// shell that does not echo, and no obvious way back. This is not optional.
struct RawGuard;

impl RawGuard {
    fn enter() -> Result<Self> {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = crossterm::terminal::disable_raw_mode();
            previous(info);
        }));
        crossterm::terminal::enable_raw_mode().context("entering raw mode")?;
        Ok(RawGuard)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Attaches this terminal to a node of a running `turborust up`.
pub async fn connect(cache_dir: &Path, node: &str) -> Result<i32> {
    let path = socket_path(cache_dir);
    let mut stream = transport::connect(&path)
        .await
        .with_context(|| format!("no turborust running here (looked for {})", path.display()))?;

    write_line(
        &mut stream,
        &Request::Attach {
            node: node.into(),
            protocol: PROTOCOL,
        },
    )
    .await?;
    let response: Response = serde_json::from_str(&read_line(&mut stream).await?)
        .context("the server sent something unparseable")?;
    if !response.ok {
        bail!(
            "{}",
            response.error.unwrap_or_else(|| "attach refused".into())
        );
    }

    let _raw = RawGuard::enter()?;
    let (mut reader, writer) = tokio::io::split(stream);
    let writer = std::sync::Arc::new(tokio::sync::Mutex::new(writer));

    // Tell the child our size now, and whenever it changes.
    if let Ok((cols, rows)) = crossterm::terminal::size() {
        let mut w = writer.lock().await;
        write_frame(&mut *w, frame::RESIZE, format!("{rows},{cols}").as_bytes()).await?;
    }
    spawn_resize_watcher(writer.clone());

    // stdin is a blocking read that cannot be cancelled, so it gets a thread and
    // hands bytes over a channel rather than being selected on directly.
    let (keys_tx, mut keys) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 1024];
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || keys_tx.send(buf[..n].to_vec()).is_err() {
                return;
            }
        }
    });

    let mut out = tokio::io::stdout();
    let mut armed = false; // saw the detach lead byte
    loop {
        tokio::select! {
            incoming = read_frame(&mut reader) => match incoming {
                Ok((frame::OUTPUT, bytes)) => {
                    out.write_all(&bytes).await?;
                    out.flush().await?;
                }
                Ok(_) => {}
                Err(_) => {
                    eprintln!("\r\nturborust: the node exited or the server went away.\r");
                    return Ok(0);
                }
            },
            typed = keys.recv() => {
                let Some(bytes) = typed else { return Ok(0) };
                if let Some(rest) = detach_split(&bytes, &mut armed) {
                    if !rest.is_empty() {
                        let mut w = writer.lock().await;
                        write_frame(&mut *w, frame::STDIN, &rest).await?;
                    }
                    eprintln!("\r\nturborust: detached from `{node}`; it keeps running.\r");
                    return Ok(0);
                }
                let mut w = writer.lock().await;
                write_frame(&mut *w, frame::STDIN, &bytes).await?;
            }
        }
    }
}

/// Splits input at a detach sequence.
///
/// Returns `Some(bytes_before_the_sequence)` when the user detached, `None` to
/// keep going. `armed` carries the lead byte across reads, since a keystroke pair
/// can easily arrive in two chunks.
fn detach_split(bytes: &[u8], armed: &mut bool) -> Option<Vec<u8>> {
    let mut before = Vec::new();
    for (i, &b) in bytes.iter().enumerate() {
        if *armed {
            *armed = false;
            if b == DETACH_KEY {
                return Some(before);
            }
            // Not the detach key, so the lead byte was meant for the child.
            before.push(DETACH_LEAD);
            before.push(b);
            continue;
        }
        if b == DETACH_LEAD {
            *armed = true;
            // If it is the last byte we have, wait for the next read to decide.
            if i + 1 == bytes.len() {
                break;
            }
            continue;
        }
        before.push(b);
    }
    None
}

#[cfg(unix)]
fn spawn_resize_watcher<W: AsyncWrite + Unpin + Send + 'static>(
    writer: std::sync::Arc<tokio::sync::Mutex<W>>,
) {
    tokio::spawn(async move {
        let Ok(mut winch) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
        else {
            return;
        };
        while winch.recv().await.is_some() {
            let Ok((cols, rows)) = crossterm::terminal::size() else {
                continue;
            };
            let mut w = writer.lock().await;
            if write_frame(&mut *w, frame::RESIZE, format!("{rows},{cols}").as_bytes())
                .await
                .is_err()
            {
                return;
            }
        }
    });
}

/// Windows has no SIGWINCH; a resize is a console event this client does not
/// listen for. The initial size is still sent, which covers the common case.
#[cfg(not(unix))]
fn spawn_resize_watcher<W: AsyncWrite + Unpin + Send + 'static>(
    _writer: std::sync::Arc<tokio::sync::Mutex<W>>,
) {
}

#[cfg(test)]
mod detach_tests {
    use super::*;

    #[test]
    fn ordinary_input_is_forwarded() {
        let mut armed = false;
        assert_eq!(detach_split(b"ls -la\n", &mut armed), None);
        assert!(!armed);
    }

    #[test]
    fn ctrl_c_reaches_the_child() {
        let mut armed = false;
        // 0x03 is Ctrl-C. If this ever detaches, attaching is useless.
        assert_eq!(detach_split(&[0x03], &mut armed), None);
    }

    #[test]
    fn the_sequence_detaches_and_keeps_what_came_before() {
        let mut armed = false;
        let mut input = b"echo hi".to_vec();
        input.push(DETACH_LEAD);
        input.push(DETACH_KEY);
        assert_eq!(detach_split(&input, &mut armed), Some(b"echo hi".to_vec()));
    }

    #[test]
    fn the_sequence_survives_being_split_across_reads() {
        // Two keystrokes almost always arrive as two reads.
        let mut armed = false;
        assert_eq!(detach_split(&[DETACH_LEAD], &mut armed), None);
        assert!(armed, "the lead byte must be remembered");
        assert_eq!(detach_split(&[DETACH_KEY], &mut armed), Some(Vec::new()));
    }

    #[test]
    fn the_lead_byte_alone_is_still_delivered_to_the_child() {
        let mut armed = false;
        assert_eq!(detach_split(&[DETACH_LEAD, b'x'], &mut armed), None);
        // Ctrl-B followed by anything else is a real Ctrl-B, and readline uses it.
        assert!(!armed);
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn a_deeply_nested_workspace_still_gets_a_usable_socket() {
        // The path that exposed this: a session scratch directory nested six
        // levels down blew past sun_path and bind failed with a bare EINVAL.
        let deep = PathBuf::from(
            "/private/tmp/claude-501/-Users-someone-Code-turborust/\
             c1191035-1d98-436a-babc-25bb33b6621d/scratchpad/connect/.turborust",
        );
        let path = socket_path(&deep);
        assert!(
            path.as_os_str().len() < MAX_SOCKET_PATH,
            "socket path is {} bytes: {}",
            path.as_os_str().len(),
            path.display()
        );
    }

    #[test]
    fn the_path_is_stable_for_a_workspace_and_distinct_between_them() {
        let a = PathBuf::from("/w/one/.turborust");
        let b = PathBuf::from("/w/two/.turborust");
        assert_eq!(
            socket_path(&a),
            socket_path(&a),
            "server and client must agree"
        );
        assert_ne!(
            socket_path(&a),
            socket_path(&b),
            "two workspaces must not collide"
        );
    }
}
