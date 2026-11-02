//! Go `withCORS` (main.go:134-156) over rs/cors v1.11.1, the oracle's exact
//! middleware: origins `*`, methods HEAD..DELETE, request headers `*`, the
//! Connect + `Location, Cache-Control, X-Content-Type-Options` expose list,
//! and a 7200 s preflight cache. CORS is the OUTERMOST layer (main.go:329),
//! so a preflight is answered before authentication and before routing — even
//! for paths that do not exist.
//!
//! Two rs/cors behaviors are easy to miss and are reproduced here:
//! - `handlePreflight` always writes `Vary` and short-circuits with 204, but
//!   adds the allow headers only when the origin is allowed and the requested
//!   method is in the allow list (OPTIONS itself is always allowed).
//! - `handleActualRequest` ALWAYS writes `Vary: Origin` on every non-preflight
//!   response, with or without an Origin header.
use axum::{
    extract::Request,
    http::{header, HeaderName, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

/// rs/cors `isMethodAllowed`: OPTIONS is always allowed as a preflight method.
const ALLOWED_METHODS: [&str; 6] = ["HEAD", "GET", "POST", "PUT", "PATCH", "DELETE"];
const PREFLIGHT_VARY: &str =
    "Origin, Access-Control-Request-Method, Access-Control-Request-Headers";
const EXPOSE_HEADERS: &str = "Grpc-Status, Grpc-Message, Grpc-Status-Details-Bin, Location, Cache-Control, X-Content-Type-Options";
const MAX_AGE: &str = "7200";

fn method_allowed(method: &str) -> bool {
    method == "OPTIONS" || ALLOWED_METHODS.contains(&method)
}

fn header_text<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}

fn insert(response: &mut Response, name: HeaderName, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        response.headers_mut().insert(name, value);
    }
}

/// Append to an existing `Vary` (rs/cors appends a second header line), else set.
fn add_vary(response: &mut Response, value: &'static str) {
    let value = HeaderValue::from_static(value);
    if response.headers().contains_key(header::VARY) {
        response.headers_mut().append(header::VARY, value);
    } else {
        response.headers_mut().insert(header::VARY, value);
    }
}

pub(crate) async fn cors(request: Request, next: Next) -> Response {
    let origin = header_text(request.headers(), "origin").to_owned();
    let requested_method = header_text(request.headers(), "access-control-request-method");
    if request.method() == Method::OPTIONS && !requested_method.is_empty() {
        // rs/cors handlePreflight: standalone 204, chain never runs.
        let requested_headers =
            header_text(request.headers(), "access-control-request-headers").to_owned();
        let mut response = StatusCode::NO_CONTENT.into_response();
        add_vary(&mut response, PREFLIGHT_VARY);
        if origin.is_empty() || !method_allowed(requested_method) {
            return response;
        }
        insert(&mut response, header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
        insert(
            &mut response,
            header::ACCESS_CONTROL_ALLOW_METHODS,
            requested_method,
        );
        if !requested_headers.is_empty() {
            insert(
                &mut response,
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                &requested_headers,
            );
        }
        insert(&mut response, header::ACCESS_CONTROL_MAX_AGE, MAX_AGE);
        return response;
    }

    let method = request.method().as_str().to_owned();
    let mut response = next.run(request).await;
    // handleActualRequest writes Vary unconditionally.
    add_vary(&mut response, "Origin");
    if origin.is_empty() || !method_allowed(&method) {
        return response;
    }
    insert(&mut response, header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
    insert(
        &mut response,
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        EXPOSE_HEADERS,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::plain_http_error;
    use axum::{body::Body, middleware, routing::get, Router};
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route("/health", get(|| async { StatusCode::NO_CONTENT }))
            .fallback(|| async { plain_http_error(StatusCode::NOT_FOUND, "404 page not found") })
            .layer(middleware::from_fn(cors))
    }

    async fn send(method: &str, path: &str, headers: &[(&str, &str)]) -> Response {
        let mut request = axum::http::Request::builder().method(method).uri(path);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        app()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn preflight_is_204_with_echoed_allow_headers() {
        let response = send(
            "OPTIONS",
            "/does-not-exist",
            &[
                ("origin", "https://example.com"),
                ("access-control-request-method", "POST"),
                (
                    "access-control-request-headers",
                    "content-type,x-access-token",
                ),
            ],
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(response.headers()["vary"], PREFLIGHT_VARY);
        assert_eq!(response.headers()["access-control-allow-origin"], "*");
        assert_eq!(response.headers()["access-control-allow-methods"], "POST");
        assert_eq!(
            response.headers()["access-control-allow-headers"],
            "content-type,x-access-token"
        );
        assert_eq!(response.headers()["access-control-max-age"], "7200");
        assert!(response.headers().get(header::CONTENT_TYPE).is_none());
    }

    #[tokio::test]
    async fn preflight_with_a_disallowed_method_only_varies() {
        let response = send(
            "OPTIONS",
            "/health",
            &[
                ("origin", "https://example.com"),
                ("access-control-request-method", "TRACE"),
            ],
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(response.headers()["vary"], PREFLIGHT_VARY);
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());
    }

    #[tokio::test]
    async fn actual_requests_add_origin_and_expose_headers() {
        let response = send("GET", "/health", &[("origin", "https://example.com")]).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(response.headers()["vary"], "Origin");
        assert_eq!(response.headers()["access-control-allow-origin"], "*");
        assert_eq!(
            response.headers()["access-control-expose-headers"],
            EXPOSE_HEADERS
        );
    }

    #[tokio::test]
    async fn actual_requests_without_origin_still_vary() {
        let response = send("GET", "/health", &[]).await;
        assert_eq!(response.headers()["vary"], "Origin");
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());
    }
}
