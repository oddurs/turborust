//! Forwarding rules for the static server.
//!
//! Exists so a full-stack app has one origin. Without it a page served on one
//! port calling an API on another is cross-origin, which means CORS headers that
//! exist only for development and an API origin that has to change for
//! production — precisely what a dev server should remove.

use crate::config::ProxyRule;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, Request, Response, StatusCode, Uri, header};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;

pub type ProxyClient = Client<HttpConnector, Body>;

pub fn client() -> ProxyClient {
    Client::builder(TokioExecutor::new()).build_http()
}

/// Headers that describe *this* connection rather than the message, and so must
/// not be copied onto a different one. Forwarding `Connection` or
/// `Transfer-Encoding` to a backend produces failures that look like the
/// backend's fault.
const HOP_BY_HOP: [HeaderName; 8] = [
    header::CONNECTION,
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    header::TE,
    header::TRAILER,
    header::TRANSFER_ENCODING,
    header::UPGRADE,
    HeaderName::from_static("keep-alive"),
];

/// The first rule matching `path`, if any. Rules arrive longest-prefix-first.
pub fn match_rule<'a>(rules: &'a [ProxyRule], path: &str) -> Option<&'a ProxyRule> {
    rules.iter().find(|r| prefix_matches(&r.path, path))
}

/// A prefix matches on path *segments*, so `/api` does not swallow `/apidocs`.
fn prefix_matches(prefix: &str, path: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    if prefix.is_empty() {
        return true;
    }
    match path.strip_prefix(prefix) {
        Some("") => true,
        Some(rest) => rest.starts_with('/'),
        None => false,
    }
}

/// Builds the upstream URI for a request matched by `rule`.
pub fn target_uri(rule: &ProxyRule, path_and_query: &str) -> Result<Uri, String> {
    let (path, query) = match path_and_query.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path_and_query, None),
    };
    let forwarded = if rule.strip_prefix {
        let stripped = path
            .strip_prefix(rule.path.trim_end_matches('/'))
            .unwrap_or(path);
        if stripped.is_empty() { "/" } else { stripped }
    } else {
        path
    };
    let base = rule.to.trim_end_matches('/');
    let full = match query {
        Some(q) => format!("{base}{forwarded}?{q}"),
        None => format!("{base}{forwarded}"),
    };
    full.parse()
        .map_err(|e| format!("bad proxy target `{full}`: {e}"))
}

pub fn strip_hop_by_hop(headers: &mut HeaderMap) {
    for name in HOP_BY_HOP {
        headers.remove(&name);
    }
}

/// Forwards a request upstream and streams the response back.
pub async fn forward(
    client: &ProxyClient,
    rule: &ProxyRule,
    mut req: Request<Body>,
) -> Response<Body> {
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".into());

    let uri = match target_uri(rule, &path_and_query) {
        Ok(u) => u,
        Err(e) => return plain(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    *req.uri_mut() = uri;
    strip_hop_by_hop(req.headers_mut());

    match client.request(req).await {
        Ok(upstream) => {
            // Convert without buffering: an SSE endpoint behind the proxy has to
            // keep streaming, and collecting the body would break it.
            let (parts, body) = upstream.into_parts();
            let mut response = Response::from_parts(parts, Body::new(body));
            strip_hop_by_hop(response.headers_mut());
            response
        }
        Err(e) => unreachable_upstream(rule, &e.to_string()),
    }
}

/// What the browser sees when the backend is not answering.
///
/// A bare connection error makes the app look broken when it is merely
/// rebuilding. This is a recognisable 502 the overlay can label, and it carries
/// a `Retry-After` so a fetch loop has something to honour.
fn unreachable_upstream(rule: &ProxyRule, detail: &str) -> Response<Body> {
    let body = format!(
        "turborust: `{}` is not answering at {}.\n\
         It is probably restarting; this request was not sent.\n\n{detail}\n",
        rule.path, rule.to
    );
    let mut res = plain(StatusCode::BAD_GATEWAY, &body);
    res.headers_mut()
        .insert("retry-after", header::HeaderValue::from_static("1"));
    res.headers_mut().insert(
        "x-turborust-proxy",
        header::HeaderValue::from_static("upstream-unreachable"),
    );
    res
}

fn plain(status: StatusCode, body: &str) -> Response<Body> {
    let mut res = Response::new(Body::from(body.to_string()));
    *res.status_mut() = status;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(path: &str, strip: bool) -> ProxyRule {
        ProxyRule {
            path: path.into(),
            to: "http://127.0.0.1:8788".into(),
            strip_prefix: strip,
            websocket: false,
        }
    }

    #[test]
    fn a_prefix_matches_on_segment_boundaries() {
        assert!(prefix_matches("/api", "/api"));
        assert!(prefix_matches("/api", "/api/users"));
        // The one that bites: /api must not swallow /apidocs.
        assert!(!prefix_matches("/api", "/apidocs"));
        assert!(!prefix_matches("/api", "/other"));
    }

    #[test]
    fn the_longest_prefix_wins() {
        // The plan sorts rules longest-first; match_rule relies on that.
        let rules = vec![rule("/api/v2", false), rule("/api", false)];
        assert_eq!(
            match_rule(&rules, "/api/v2/things").unwrap().path,
            "/api/v2"
        );
        assert_eq!(match_rule(&rules, "/api/things").unwrap().path, "/api");
        assert!(match_rule(&rules, "/static/x.css").is_none());
    }

    #[test]
    fn the_path_is_preserved_by_default() {
        let uri = target_uri(&rule("/api", false), "/api/users").unwrap();
        assert_eq!(uri.to_string(), "http://127.0.0.1:8788/api/users");
    }

    #[test]
    fn strip_prefix_removes_the_mount_point() {
        let uri = target_uri(&rule("/api", true), "/api/users").unwrap();
        assert_eq!(uri.to_string(), "http://127.0.0.1:8788/users");
    }

    #[test]
    fn stripping_everything_still_leaves_a_root_path() {
        let uri = target_uri(&rule("/api", true), "/api").unwrap();
        assert_eq!(uri.to_string(), "http://127.0.0.1:8788/");
    }

    #[test]
    fn the_query_string_survives() {
        let uri = target_uri(&rule("/api", true), "/api/search?q=hello&n=2").unwrap();
        assert_eq!(uri.to_string(), "http://127.0.0.1:8788/search?q=hello&n=2");
    }

    #[test]
    fn hop_by_hop_headers_are_dropped() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONNECTION, "keep-alive".parse().unwrap());
        headers.insert(header::TRANSFER_ENCODING, "chunked".parse().unwrap());
        headers.insert(header::AUTHORIZATION, "Bearer x".parse().unwrap());
        strip_hop_by_hop(&mut headers);
        assert!(!headers.contains_key(header::CONNECTION));
        assert!(!headers.contains_key(header::TRANSFER_ENCODING));
        // End-to-end headers must survive; dropping auth would be a real bug.
        assert!(headers.contains_key(header::AUTHORIZATION));
    }

    #[test]
    fn an_unreachable_backend_is_labelled_not_just_failed() {
        let res = unreachable_upstream(&rule("/api", false), "connection refused");
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(res.headers()["x-turborust-proxy"], "upstream-unreachable");
        assert!(res.headers().contains_key("retry-after"));
    }
}
