//! Readiness probes and port hygiene.
//!
//! "Started" is not "ready". Dependency ordering that only waits for a process to
//! *exist* is the reason `depends_on` in most dev tooling is decorative — the
//! frontend still boots before the API can accept a connection. Here a dependent
//! service waits for its dependency to answer.

use crate::config::Health;
use anyhow::Result;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const CONNECT_TIMEOUT: Duration = Duration::from_millis(750);

/// One attempt at the network probes. Never errors — a failed probe is just
/// "not ready yet".
///
/// Every probe a node declares must pass, so `{ tcp = 8080, log = "ready" }`
/// means both. The log half is checked by the caller, which is the only place
/// with access to the node's output.
pub async fn probe(h: &Health) -> bool {
    if let Some(url) = &h.http
        && !http_ok(url).await
    {
        return false;
    }
    if let Some(port) = h.tcp
        && !tcp_open("127.0.0.1", port).await
    {
        return false;
    }
    true
}

pub async fn tcp_open(host: &str, port: u16) -> bool {
    matches!(
        tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port))).await,
        Ok(Ok(_))
    )
}

/// GET the URL; any 2xx or 3xx is healthy.
///
/// Hand-rolled rather than pulling in an HTTP client: a readiness probe against
/// localhost needs the status line and nothing else.
async fn http_ok(url: &str) -> bool {
    let Some((host, port, path)) = split_url(url) else {
        return false;
    };
    let Ok(Ok(mut stream)) =
        tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port))).await
    else {
        return false;
    };
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: turborust\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(req.as_bytes()).await.is_err() {
        return false;
    }
    let mut buf = [0u8; 128];
    let Ok(Ok(n)) = tokio::time::timeout(CONNECT_TIMEOUT, stream.read(&mut buf)).await else {
        return false;
    };
    let head = String::from_utf8_lossy(&buf[..n]);
    status_is_ok(&head)
}

fn status_is_ok(head: &str) -> bool {
    head.split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .map(|c| (200..400).contains(&c))
        .unwrap_or(false)
}

/// Splits `http://host:port/path` into its parts. Only http is supported —
/// a dev-time localhost probe has no business negotiating TLS.
fn split_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()?),
        None => (authority.to_string(), 80u16),
    };
    if host.is_empty() {
        return None;
    }
    Some((host, port, path.to_string()))
}

/// Polls until the probe passes or `timeout` elapses.
pub async fn wait_ready(h: &Health, interval: Duration, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if probe(h).await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(interval).await;
    }
}

/// Waits for a port to become free.
///
/// A supervisor that restarts faster than the kernel releases a listening socket
/// produces "address already in use" on every other restart. Waiting is the fix;
/// reporting *who* holds it is the courtesy.
pub async fn wait_port_free(port: u16, timeout: Duration) -> Result<bool> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if !tcp_open("127.0.0.1", port).await {
            return Ok(true);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Best-effort "what is listening on this port", for error messages.
pub fn port_holder(port: u16) -> Option<String> {
    let out = std::process::Command::new("lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fcn"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let cmd = text
        .lines()
        .find(|l| l.starts_with('c'))
        .map(|l| l[1..].to_string())?;
    let pid = text
        .lines()
        .find(|l| l.starts_with('p'))
        .map(|l| l[1..].to_string());
    Some(match pid {
        Some(p) => format!("{cmd} (pid {p})"),
        None => cmd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls() {
        assert_eq!(
            split_url("http://localhost:3000/healthz"),
            Some(("localhost".into(), 3000, "/healthz".into()))
        );
        assert_eq!(
            split_url("http://example.com"),
            Some(("example.com".into(), 80, "/".into()))
        );
        assert_eq!(split_url("https://example.com"), None);
        assert_eq!(split_url("nonsense"), None);
    }

    #[test]
    fn reads_status_lines() {
        assert!(status_is_ok("HTTP/1.1 200 OK\r\n"));
        assert!(status_is_ok("HTTP/1.1 302 Found\r\n"));
        assert!(!status_is_ok("HTTP/1.1 500 Internal Server Error\r\n"));
        assert!(!status_is_ok("garbage"));
    }

    #[tokio::test]
    async fn probes_a_real_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(tcp_open("127.0.0.1", port).await);
        drop(listener);
        // The port must eventually read as free once nothing is listening.
        assert!(wait_port_free(port, Duration::from_secs(3)).await.unwrap());
    }

    #[tokio::test]
    async fn http_probe_accepts_2xx() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = listener.accept().await {
                let mut junk = [0u8; 512];
                let _ = s.read(&mut junk).await;
                let _ = s
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });
        let h = Health {
            tcp: None,
            http: Some(format!("http://127.0.0.1:{port}/healthz")),
            log: None,
            interval: "50ms".into(),
        };
        assert!(probe(&h).await);
    }
}
