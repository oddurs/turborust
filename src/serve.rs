//! In-process static server for wasm/SPA frontends, with browser live-reload.
//!
//! Two things this must get right that hand-rolled dev servers routinely miss:
//!
//! 1. `.wasm` must be served as `application/wasm`. Browsers refuse
//!    `WebAssembly.instantiateStreaming` on anything else, and the resulting
//!    console error blames your code, not your server.
//! 2. Reload is a *full page reload*, not HMR. True hot module replacement needs
//!    the bundler to hand the runtime a module-shaped patch; a wasm binary has no
//!    module boundaries left to patch. Claiming HMR here would be a lie.

use crate::config::{ErrorMode, Overlay, ProxyRule};
use crate::overlay;
use crate::state::AppState;
use anyhow::{Context, Result};
use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response, Sse, sse::Event};
use axum::routing::get;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::{BroadcastStream, IntervalStream};

pub const RELOAD_PATH: &str = overlay::RELOAD_PATH;

/// How often the overlay's state stream is sampled. Frames are only sent when
/// the payload actually differs, so an idle page costs nothing.
const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(250);

const RELOAD_SNIPPET: &str = r#"<script>
(function(){
  var backoff = 250;
  function connect(){
    var es = new EventSource("/__turborust/reload");
    es.onopen = function(){ backoff = 250; };
    es.onmessage = function(){ location.reload(); };
    es.onerror = function(){
      es.close();
      // The dev server restarting is normal; keep trying, but do not spin.
      setTimeout(connect, backoff);
      backoff = Math.min(backoff * 2, 5000);
    };
  }
  connect();
})();
</script>"#;

struct ServeState {
    dir: PathBuf,
    spa: bool,
    live_reload: bool,
    reload: broadcast::Sender<String>,
    overlay: Overlay,
    /// Absolute workspace root, for `vscode://` jump links in the overlay.
    root: PathBuf,
    app: Arc<Mutex<AppState>>,
    /// Control handles, so the overlay can restart a node.
    wires: Arc<std::collections::BTreeMap<String, crate::engine::Wire>>,
    proxy: Vec<ProxyRule>,
    proxy_client: crate::proxy::ProxyClient,
    /// False when bound beyond loopback without explicit consent.
    control_allowed: bool,
}

pub struct ServeOpts {
    pub dir: PathBuf,
    pub port: u16,
    pub serve: crate::config::Serve,
    pub spa: bool,
    pub live_reload: bool,
    pub reload: broadcast::Sender<String>,
    pub overlay: Overlay,
    pub root: PathBuf,
    pub app: Arc<Mutex<AppState>>,
    pub wires: Arc<std::collections::BTreeMap<String, crate::engine::Wire>>,
    pub proxy: Vec<ProxyRule>,
}

pub async fn serve(opts: ServeOpts) -> Result<()> {
    let dir = opts
        .dir
        .canonicalize()
        .with_context(|| format!("serve dir {} does not exist yet", opts.dir.display()))?;
    let port = opts.port;
    let state = Arc::new(ServeState {
        dir,
        spa: opts.spa,
        live_reload: opts.live_reload,
        reload: opts.reload,
        overlay: opts.overlay,
        root: opts.root,
        app: opts.app,
        wires: opts.wires,
        proxy: opts.proxy,
        proxy_client: crate::proxy::client(),
        control_allowed: opts.serve.control_allowed(),
    });

    let app = Router::new()
        .route(RELOAD_PATH, get(reload_stream))
        .route(overlay::STATE_PATH, get(state_stream))
        .route(overlay::LOGS_PATH, get(logs_stream))
        .route(overlay::RESTART_PATH, axum::routing::post(restart_node))
        .route(overlay::JS_PATH, get(overlay_js))
        .route(overlay::CSS_PATH, get(overlay_css))
        // `any` rather than `get`: a proxied POST must reach the backend.
        .fallback(axum::routing::any(fallback))
        .with_state(state);

    let host = opts.serve.host.clone();
    let scheme = if opts.serve.tls.is_some() {
        "https"
    } else {
        "http"
    };
    let addr: std::net::SocketAddr = format!("{host}:{port}")
        .parse()
        .or_else(|_| format!("127.0.0.1:{port}").parse())
        .with_context(|| format!("bad serve host `{host}`"))?;

    if opts.serve.open {
        // After the bind, not before: opening a browser at a URL that is not
        // listening yet produces an error page the user then has to reload.
        let url = format!("{scheme}://{}:{port}/", display_host(&host));
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let _ = open_browser(&url);
        });
    }

    match &opts.serve.tls {
        #[cfg(feature = "tls")]
        Some(tls) => {
            // rustls needs a crypto provider chosen explicitly when the crate is
            // built without a default one. `ring` rather than `aws-lc-rs`
            // deliberately: aws-lc-sys needs a C toolchain (cmake, nasm) at build
            // time, which would make `cargo install turborust` fail on a clean
            // Windows machine. Installing twice is harmless.
            let _ = rustls::crypto::ring::default_provider().install_default();
            let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
                opts.dir.parent().unwrap_or(Path::new(".")).join(&tls.cert),
                opts.dir.parent().unwrap_or(Path::new(".")).join(&tls.key),
            )
            .await
            .context("loading the TLS certificate and key")?;
            axum_server::bind_rustls(addr, config)
                .serve(app.into_make_service())
                .await
                .context("static server (tls)")?;
        }
        #[cfg(not(feature = "tls"))]
        Some(_) => {
            anyhow::bail!(
                "this turborust was built without TLS support. \
                 Reinstall with `--features tls`, or drop the `tls` block."
            );
        }
        None => {
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .with_context(|| format!("binding {addr}"))?;
            axum::serve(listener, app).await.context("static server")?;
        }
    }
    Ok(())
}

/// Streams a full state snapshot whenever anything changes.
///
/// Polling the shared state and de-duplicating the serialized frame is simpler
/// and more robust than threading a change-notification channel through every
/// supervisor — and at 250ms an idle page sends nothing at all.
async fn state_stream(State(st): State<Arc<ServeState>>) -> impl IntoResponse {
    let app = st.app.clone();
    let mut last = String::new();
    let stream =
        IntervalStream::new(tokio::time::interval(SNAPSHOT_INTERVAL)).filter_map(move |_| {
            let json = {
                let guard = app.lock().ok()?;
                serde_json::to_string(&guard.snapshot()).ok()?
            };
            if json == last {
                return None;
            }
            last = json.clone();
            Some(Ok::<Event, Infallible>(Event::default().data(json)))
        });
    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

/// Streams output lines as they arrive, resuming from `?after=<seq>`.
async fn logs_stream(
    State(st): State<Arc<ServeState>>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let app = st.app.clone();
    let mut cursor: u64 = q.get("after").and_then(|v| v.parse().ok()).unwrap_or(0);
    let stream = IntervalStream::new(tokio::time::interval(Duration::from_millis(200))).filter_map(
        move |_| {
            let lines = app.lock().ok()?.logs_since(cursor);
            if lines.is_empty() {
                return None;
            }
            cursor = lines.last().map(|l| l.seq).unwrap_or(cursor);
            let json = serde_json::to_string(&lines).ok()?;
            Some(Ok::<Event, Infallible>(Event::default().data(json)))
        },
    );
    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

/// Restarts one node on request from the overlay.
///
/// This mutates supervisor state, so it is same-origin only. The server already
/// binds loopback, but a dev server that any page on the machine can drive is not
/// acceptable even in development.
async fn restart_node(
    State(st): State<Arc<ServeState>>,
    AxumPath(node): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    // Same-origin is not sufficient once the origin is routable: every page on
    // the network shares it. Binding beyond loopback therefore closes this
    // endpoint unless the config says otherwise.
    if !st.control_allowed {
        return (
            StatusCode::FORBIDDEN,
            "this server is bound beyond loopback; set allow_remote_control = true to permit this",
        )
            .into_response();
    }
    if !is_same_origin(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin requests are refused").into_response();
    }
    match st.wires.get(&node) {
        Some(wire) => {
            wire.ctl_tx
                .send(crate::ctl::Ctl::Restart("manual restart (overlay)".into()));
            (StatusCode::ACCEPTED, "restarting").into_response()
        }
        None => (StatusCode::NOT_FOUND, "no such node").into_response(),
    }
}

/// Trusts `Sec-Fetch-Site: same-origin`, which every browser that can reach this
/// endpoint sends and no page can forge.
fn is_same_origin(headers: &HeaderMap) -> bool {
    headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "same-origin" || v == "none")
        .unwrap_or(false)
}

async fn overlay_js() -> Response {
    asset(overlay::JS, "text/javascript; charset=utf-8")
}

async fn overlay_css() -> Response {
    asset(overlay::CSS, "text/css; charset=utf-8")
}

fn asset(body: &'static str, mime: &'static str) -> Response {
    let mut res = Response::new(Body::from(body));
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// `0.0.0.0` is not a URL anyone can visit; show something they can.
fn display_host(host: &str) -> String {
    if host == "0.0.0.0" || host == "::" {
        "localhost".into()
    } else {
        host.into()
    }
}

fn open_browser(url: &str) -> std::io::Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(url)
        .status()
        .map(|_| ())
}

async fn reload_stream(State(st): State<Arc<ServeState>>) -> impl IntoResponse {
    let rx = st.reload.subscribe();
    let stream = BroadcastStream::new(rx).map(|msg| {
        let who = msg.unwrap_or_else(|_| "?".to_string());
        Ok::<Event, Infallible>(Event::default().data(who))
    });
    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

/// Proxy rules are checked before static files, so a forwarded path is never
/// shadowed by a file that happens to share its name.
async fn fallback(State(st): State<Arc<ServeState>>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    if let Some(rule) = crate::proxy::match_rule(&st.proxy, &path) {
        return crate::proxy::forward(&st.proxy_client, rule, req).await;
    }
    if req.method() != axum::http::Method::GET {
        return (StatusCode::METHOD_NOT_ALLOWED, "static files are GET only").into_response();
    }
    file_handler(State(st), req.uri().clone()).await
}

async fn file_handler(State(st): State<Arc<ServeState>>, uri: Uri) -> Response {
    let rel = uri.path().trim_start_matches('/');
    match resolve(&st.dir, rel, st.spa) {
        Some(path) => match tokio::fs::read(&path).await {
            Ok(bytes) => render(&path, bytes, &st),
            Err(_) => not_found(),
        },
        None => not_found(),
    }
}

/// Maps a request path to a file, refusing anything that escapes the served dir.
fn resolve(dir: &Path, rel: &str, spa: bool) -> Option<PathBuf> {
    let candidate = if rel.is_empty() {
        dir.join("index.html")
    } else {
        dir.join(rel)
    };
    // Canonicalize before the prefix check: `..` and symlinks both have to be
    // resolved for the containment test to mean anything.
    if let Ok(p) = candidate.canonicalize() {
        if p.starts_with(dir) {
            if p.is_dir() {
                let idx = p.join("index.html");
                return idx.is_file().then_some(idx);
            }
            return Some(p);
        }
        return None;
    }
    // Unknown path: a client-side router owns it.
    if spa && !rel.contains('.') {
        let idx = dir.join("index.html");
        return idx.is_file().then_some(idx);
    }
    None
}

fn render(path: &Path, bytes: Vec<u8>, st: &ServeState) -> Response {
    let mime = mime_for(path);
    let is_html = mime.starts_with("text/html");
    let body = if is_html && st.live_reload {
        let html = String::from_utf8_lossy(&bytes);
        // With the overlay on, its client owns reloading too — injecting both
        // would open two EventSources and reload twice.
        let snippet = if st.overlay.enabled && st.overlay.errors != ErrorMode::Silent {
            overlay::bootstrap_tag(&st.overlay, &st.root)
        } else {
            RELOAD_SNIPPET.to_string()
        };
        Body::from(inject(html.as_ref(), &snippet))
    } else {
        Body::from(bytes)
    };
    let mut res = Response::new(body);
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_str(mime).unwrap());
    // Dev assets must never be cached; a stale wasm blob against fresh JS glue is
    // a uniquely confusing failure.
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, must-revalidate"),
    );
    res
}

/// Places the client just before `</body>` so it runs after the app's own
/// scripts are at least parsed.
fn inject(html: &str, snippet: &str) -> String {
    match html.rfind("</body>") {
        Some(i) => format!("{}{}{}", &html[..i], snippet, &html[i..]),
        None => format!("{html}{snippet}"),
    }
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        // Non-negotiable for `instantiateStreaming`.
        "wasm" => "application/wasm",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "map" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tr-serve-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    #[test]
    fn wasm_gets_the_right_mime() {
        assert_eq!(mime_for(Path::new("app_bg.wasm")), "application/wasm");
        assert_eq!(
            mime_for(Path::new("a.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(mime_for(Path::new("weird.xyz")), "application/octet-stream");
    }

    #[test]
    fn injects_before_closing_body() {
        let out = inject(
            "<html><body><div id=app></div></body></html>",
            RELOAD_SNIPPET,
        );
        assert!(out.contains("EventSource"));
        assert!(out.find("EventSource").unwrap() < out.find("</body>").unwrap());
    }

    #[test]
    fn injects_into_bodyless_html() {
        assert!(inject("<h1>hi</h1>", RELOAD_SNIPPET).contains("EventSource"));
    }

    #[test]
    fn overlay_tag_injects_exactly_one_client() {
        let tag = overlay::bootstrap_tag(&Overlay::default(), Path::new("/w"));
        let out = inject("<html><body></body></html>", &tag);
        assert_eq!(
            out.matches("<script").count(),
            1,
            "two clients would reload twice"
        );
        assert!(out.contains(overlay::JS_PATH));
    }

    #[test]
    fn refuses_path_traversal() {
        let dir = tmp("traversal");
        std::fs::write(dir.join("index.html"), "<html></html>").unwrap();
        let secret = dir.parent().unwrap().join("tr-secret.txt");
        std::fs::write(&secret, "nope").unwrap();
        assert_eq!(resolve(&dir, "../tr-secret.txt", false), None);
        assert_eq!(resolve(&dir, "../../etc/passwd", true), None);
        let _ = std::fs::remove_file(&secret);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spa_falls_back_only_for_extensionless_paths() {
        let dir = tmp("spa");
        std::fs::write(dir.join("index.html"), "<html></html>").unwrap();
        assert!(resolve(&dir, "dashboard/settings", true).is_some());
        // A missing asset must 404, not silently return HTML — otherwise a typo'd
        // wasm path returns an HTML page and the browser error is nonsense.
        assert_eq!(resolve(&dir, "app_bg.wasm", true), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn serves_index_for_root_and_directories() {
        let dir = tmp("index");
        std::fs::write(dir.join("index.html"), "<html></html>").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/index.html"), "<html></html>").unwrap();
        assert_eq!(resolve(&dir, "", false), Some(dir.join("index.html")));
        assert_eq!(
            resolve(&dir, "sub", false),
            Some(dir.join("sub/index.html"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
