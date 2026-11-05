//! The oracle's request mux (chi v1.5.5 + connect-go v1.18.1 handlers), which
//! decides four outcomes before any handler code runs:
//!
//! - a method chi does not know → 405, empty body, no headers (chi resolves
//!   the method before the route, `mux.go:476-480`);
//! - a known HTTP operation with the wrong method → 405, empty body, one
//!   `Allow` header per method (`chi.methodNotAllowedHandler`);
//! - a known Connect procedure with a non-POST method → 405 + `Allow: POST`
//!   (connect-go `Handler.ServeHTTP` has no handler for that method);
//! - a known Connect procedure whose Content-Type is not one its protocol
//!   kind speaks → 415 + `Accept-Post`, empty body (`protocolHandler == nil`);
//! - anything else → chi's `http.NotFound` text.
//!
//! Without this layer axum's fallback would hand every unmatched path to the
//! Connect service, whose JSON "method not found" error and 400-style status
//! are not what the oracle puts on the wire. It also drops an empty
//! `Connect-Protocol-Version` header, which connect-go treats as absent
//! (`validateConnectVersion`) but the Rust service rejects as an unsupported
//! version.
use axum::{
    extract::Request,
    http::{header, HeaderName, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

use super::plain_http_error;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Unary,
    Stream,
}

impl Kind {
    /// connect-go registers the Connect protocol's unary codecs
    /// (`application/json`, `application/proto`) only for unary methods and
    /// its streaming codecs (`application/connect+json`, `+proto`) only for
    /// streaming methods; the gRPC and gRPC-web protocols are registered for
    /// both. A payload of the other kind is refused before parsing.
    fn accepts(self, content_type: &str) -> bool {
        let grpc = content_type == "application/grpc"
            || content_type.starts_with("application/grpc+")
            || content_type == "application/grpc-web"
            || content_type.starts_with("application/grpc-web+");
        match self {
            Kind::Unary => {
                grpc || content_type == "application/json" || content_type == "application/proto"
            }
            Kind::Stream => {
                grpc || content_type == "application/connect+json"
                    || content_type == "application/connect+proto"
            }
        }
    }

    fn accept_post(self) -> &'static str {
        match self {
            Kind::Unary => "application/grpc, application/grpc+json, application/grpc+proto, application/grpc-web, application/grpc-web+json, application/grpc-web+proto, application/json, application/proto",
            Kind::Stream => "application/connect+json, application/connect+proto, application/grpc, application/grpc+json, application/grpc+proto, application/grpc-web, application/grpc-web+json, application/grpc-web+proto",
        }
    }
}

/// The 13 HTTP operations' chi patterns (api.gen.go:969-996 plus main.go:427).
fn http_methods(path: &str) -> Option<&'static [&'static str]> {
    Some(match path {
        "/health" | "/metrics" | "/envs" => &["GET"],
        "/init" | "/freeze" | "/unfreeze" | "/collapse" | "/fsfreeze" | "/fsthaw"
        | "/files/compose" | "/upgrade" => &["POST"],
        "/files" => &["GET", "POST"],
        _ => return None,
    })
}

/// The 17 Connect procedures at their generated attach paths
/// (process.connect.go / filesystem.connect.go).
fn rpc_kind(path: &str) -> Option<Kind> {
    Some(match path {
        "/process.Process/List"
        | "/process.Process/Update"
        | "/process.Process/SendInput"
        | "/process.Process/SendSignal"
        | "/process.Process/CloseStdin" => Kind::Unary,
        "/process.Process/Connect" | "/process.Process/Start" | "/process.Process/StreamInput" => {
            Kind::Stream
        }
        "/filesystem.Filesystem/Stat"
        | "/filesystem.Filesystem/MakeDir"
        | "/filesystem.Filesystem/Move"
        | "/filesystem.Filesystem/ListDir"
        | "/filesystem.Filesystem/Remove"
        | "/filesystem.Filesystem/CreateWatcher"
        | "/filesystem.Filesystem/GetWatcherEvents"
        | "/filesystem.Filesystem/RemoveWatcher" => Kind::Unary,
        "/filesystem.Filesystem/WatchDir" => Kind::Stream,
        _ => return None,
    })
}

/// connect-go's `canonicalizeContentType`: the base type, trimmed, lowercased.
fn canonical_content_type(headers: &axum::http::HeaderMap) -> String {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// chi's `methodMap` (v5.5.0 `mux.go:128`): the only methods it routes. The
/// lookup is case-sensitive, so `get` or `PRI` is as unknown as `FOO`.
const CHI_METHODS: &[&str] = &[
    "CONNECT", "DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT", "QUERY", "TRACE",
];

fn empty(status: StatusCode) -> Response {
    status.into_response()
}

pub(crate) async fn gate(request: Request, next: Next) -> Response {
    let method = request.method().as_str();
    if super::cua::namespace(request.uri().path()) {
        let allowed = match request.uri().path() {
            "/cua/tools" | "/cua/health" | "/cua/metrics" => "GET",
            "/cua/call" => "POST",
            _ => return super::json_error(StatusCode::NOT_FOUND, "unknown CUA route"),
        };
        if method != allowed {
            let mut response =
                super::json_error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
            response
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static(allowed));
            return response;
        }
        return next.run(request).await;
    }
    // `routeHTTP` resolves the method before it touches the tree: an unknown
    // method is the no-methods `MethodNotAllowedHandler` — a 405 with no
    // `Allow` — whatever the path is (`mux.go:476-480`).
    if !CHI_METHODS.contains(&method) {
        return empty(StatusCode::METHOD_NOT_ALLOWED);
    }
    let path = request.uri().path().to_owned();
    if let Some(methods) = http_methods(&path) {
        if methods.contains(&method) {
            return next.run(request).await;
        }
        // chi's `methodNotAllowedHandler`: one `Allow` header per method on
        // the matched node, then the status and an empty write
        // (`mux.go:529-540`).
        let mut response = empty(StatusCode::METHOD_NOT_ALLOWED);
        for allowed in methods {
            response
                .headers_mut()
                .append(header::ALLOW, HeaderValue::from_static(allowed));
        }
        return response;
    }
    let Some(kind) = rpc_kind(&path) else {
        return plain_http_error(StatusCode::NOT_FOUND, "404 page not found");
    };
    if request.method() != Method::POST {
        let mut response = empty(StatusCode::METHOD_NOT_ALLOWED);
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST"));
        return response;
    }
    let content_type = canonical_content_type(request.headers());
    if !kind.accepts(&content_type) {
        let mut response = empty(StatusCode::UNSUPPORTED_MEDIA_TYPE);
        if let Ok(value) = HeaderValue::from_str(kind.accept_post()) {
            response
                .headers_mut()
                .insert(HeaderName::from_static("accept-post"), value);
        }
        return response;
    }
    let mut request = request;
    // connect-go `validateConnectVersion`: an empty header is no header.
    if request
        .headers()
        .get("connect-protocol-version")
        .is_some_and(|value| value.is_empty())
    {
        request.headers_mut().remove("connect-protocol-version");
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, middleware, routing::any, Router};
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route("/health", any(|| async { StatusCode::NO_CONTENT }))
            .route("/init", any(|| async { StatusCode::NO_CONTENT }))
            .fallback(|| async { StatusCode::OK })
            .layer(middleware::from_fn(gate))
    }

    async fn send(method: &str, path: &str, content_type: Option<&str>) -> Response {
        let mut request = axum::http::Request::builder().method(method).uri(path);
        if let Some(content_type) = content_type {
            request = request.header("content-type", content_type);
        }
        app()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn unknown_paths_are_chi_404s_without_reaching_the_fallback() {
        for path in [
            "/",
            "/does-not-exist",
            "/health/",
            "/HEALTH",
            "/init/",
            "//health",
            "/files/../health",
        ] {
            let response = send("GET", path, None).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "text/plain; charset=utf-8"
            );
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        }
    }

    #[tokio::test]
    async fn known_http_operations_with_a_wrong_method_are_empty_405s() {
        for (method, path, allowed) in [
            ("POST", "/health", &["GET"][..]),
            ("HEAD", "/health", &["GET"][..]),
            ("GET", "/init", &["POST"][..]),
            ("GET", "/files/compose", &["POST"][..]),
            ("HEAD", "/files", &["GET", "POST"][..]),
        ] {
            let response = send(method, path, None).await;
            assert_eq!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{method} {path}"
            );
            // chi writes only the status and one Allow header per method;
            // net/http adds the framing.
            let values: Vec<&str> = response
                .headers()
                .get_all(header::ALLOW)
                .iter()
                .map(|value| value.to_str().expect("ascii"))
                .collect();
            assert_eq!(values, allowed, "{method} {path}");
            assert!(response.headers().get(header::CONTENT_TYPE).is_none());
            assert!(response.headers().get("accept-post").is_none());
        }
    }

    #[tokio::test]
    async fn unknown_methods_are_405s_without_allow_before_any_path_lookup() {
        // chi resolves the method against its methodMap before touching the
        // tree (`mux.go:476-480`); the no-methods MethodNotAllowedHandler
        // emits no Allow header, even for a path that does not exist.
        for (method, path) in [
            ("PRI", "/health"),
            ("PRI", "/does-not-exist"),
            ("get", "/health"),
            ("OPTIONSish", "/init"),
        ] {
            let response = send(method, path, None).await;
            assert_eq!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{method} {path}"
            );
            assert!(
                response.headers().get(header::ALLOW).is_none(),
                "{method} {path}"
            );
        }
        // QUERY is in chi v5.5.0's methodMap, so it takes the normal lookup.
        let response = send("QUERY", "/health", None).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[header::ALLOW], "GET");
    }

    #[tokio::test]
    async fn unknown_rpc_procedures_are_404s() {
        for path in [
            "/process.Process/Nope",
            "/process.Nope/List",
            "/process.process/List",
            "/process.Process/List/",
            "/process.Process/",
            "/process.Process",
        ] {
            let response = send("POST", path, Some("application/connect+json")).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn non_post_rpc_requests_are_empty_405s_with_allow_post() {
        let response = send("GET", "/process.Process/List", Some("application/json")).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[header::ALLOW], "POST");
        assert!(response.headers().get(header::CONTENT_TYPE).is_none());
    }

    #[tokio::test]
    async fn cross_kind_content_types_are_empty_415s_with_accept_post() {
        let unary = send(
            "POST",
            "/process.Process/List",
            Some("application/connect+json"),
        )
        .await;
        assert_eq!(unary.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(unary.headers().get(header::CONTENT_TYPE).is_none());
        assert!(unary.headers()["accept-post"]
            .to_str()
            .unwrap()
            .contains("application/json"));

        let stream = send("POST", "/process.Process/Connect", Some("application/json")).await;
        assert_eq!(stream.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(stream.headers()["accept-post"]
            .to_str()
            .unwrap()
            .contains("application/connect+json"));

        let unknown = send("POST", "/process.Process/List", Some("text/plain")).await;
        assert_eq!(unknown.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn allowed_requests_reach_the_fallback() {
        for (method, path, content_type, expected) in [
            ("GET", "/health", None, StatusCode::NO_CONTENT),
            ("POST", "/init", None, StatusCode::NO_CONTENT),
            (
                "POST",
                "/process.Process/List",
                Some("application/json; charset=utf-8"),
                StatusCode::OK,
            ),
            (
                "POST",
                "/process.Process/StreamInput",
                Some("application/connect+json"),
                StatusCode::OK,
            ),
            (
                "POST",
                "/process.Process/List",
                Some("application/grpc+proto"),
                StatusCode::OK,
            ),
        ] {
            let response = send(method, path, content_type).await;
            assert_eq!(response.status(), expected, "{method} {path}");
        }
    }
}
