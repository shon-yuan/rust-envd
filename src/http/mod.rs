//! HTTP surface shared by the Linux parity daemon and the Windows port.
//!
//! Everything here is platform-neutral: the connection-level checks Go's
//! `net/http` performs before a handler, rs/cors, the chi/connect-go mux
//! frontdoor, authorization, and the two REST handlers whose only state is the
//! daemon configuration (`/health`, `/envs`). Both daemons mount these through
//! [`layered`], so the wire behaviour is literally the same code on each.
pub mod cors;
pub mod cua;
pub mod frontdoor;

use crate::configuration::Configuration;
use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{header, Method, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Router,
};
use std::sync::Arc;
use tokio::sync::Mutex;

/// The state an HTTP handler needs: the mutable configuration the daemon
/// guards for the lifetime of the process.
pub trait ConfigurationState: Send + Sync + 'static {
    fn configuration(&self) -> &Arc<Mutex<Configuration>>;
}

/// main.go:325-331 order: CORS is outermost, then authorization, then the
/// chi mux (here [`frontdoor::gate`] followed by the two routers). The gate
/// sits inside authorization because Go's auth middleware wraps the mux, so an
/// unauthenticated unknown path is 401, not 404.
///
/// The two outermost layers reproduce `net/http`'s connection-level checks,
/// which run before any handler: a failed `Expect` is answered 417
/// (`server.go:2059-2068`) and `OPTIONS *` is answered by the general options
/// handler (`server.go:3327`).
pub fn layered<S: ConfigurationState>(router: Router, state: Arc<S>) -> Router {
    crate::transport::ensure_protojson_schema();
    router
        .layer(axum::middleware::from_fn(frontdoor::gate))
        .layer(axum::middleware::from_fn_with_state(state, authorize))
        .layer(axum::middleware::from_fn(cors::cors))
        .layer(axum::middleware::from_fn(options_asterisk))
        .layer(axum::middleware::from_fn(expectation))
}

/// `net/http`'s Expect handling (`server.go:2059-2068`): a non-empty `Expect`
/// whose first value has no `100-continue` token is answered 417 before any
/// handler runs, with `Connection: close` set (`sendExpectationFailed`).
async fn expectation(request: Request<Body>, next: Next) -> Response {
    let expect = request.headers().get(header::EXPECT);
    let continue_expected = expect.is_some_and(|value| {
        value
            .as_bytes()
            .split(|&byte| byte == b',')
            .any(|token| token.trim_ascii().eq_ignore_ascii_case(b"100-continue"))
    });
    if expect.is_some_and(|value| !value.is_empty()) && !continue_expected {
        let mut response = StatusCode::EXPECTATION_FAILED.into_response();
        response.headers_mut().insert(
            header::CONNECTION,
            header::HeaderValue::from_static("close"),
        );
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            header::HeaderValue::from_static("0"),
        );
        return response;
    }
    next.run(request).await
}

/// `serverHandler.ServeHTTP` answers `OPTIONS *` with the general options
/// handler before the mux (`server.go:3327`): 200 with `Content-Length: 0`,
/// draining at most 4 KiB of body — anything larger aborts the read and the
/// connection closes (`globalOptionsHandler`).
async fn options_asterisk(request: Request<Body>, next: Next) -> Response {
    if !is_general_options(&request) {
        return next.run(request).await;
    }
    let mut response = StatusCode::OK.into_response();
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        header::HeaderValue::from_static("0"),
    );
    if to_bytes(request.into_body(), 4 << 10).await.is_err() {
        response.headers_mut().insert(
            header::CONNECTION,
            header::HeaderValue::from_static("close"),
        );
    }
    response
}

/// `serverHandler.ServeHTTP` compares the raw request target with `"*"`
/// (`server.go:3327`), so `OPTIONS *?x` reaches the mux rather than the
/// general options handler.
fn is_general_options(request: &Request<Body>) -> bool {
    request.method() == Method::OPTIONS
        && request
            .uri()
            .path_and_query()
            .is_some_and(|target| target.as_str() == "*")
}

/// The authorization middleware: token policy from [`crate::auth`] plus the
/// Basic username check against the platform account database.
pub async fn authorize<S: ConfigurationState>(
    State(state): State<Arc<S>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let configuration = state.configuration().lock().await;
    if cua::namespace(request.uri().path()) {
        let mut tokens = request.headers().get_all("x-access-token").iter();
        let token = tokens.next().and_then(|value| value.to_str().ok());
        let admitted = tokens.next().is_none()
            && token.is_some_and(|value| !value.contains(','))
            && configuration.access.initialized()
            && configuration.access.has_token()
            && configuration.access.matches_current_token(token);
        drop(configuration);
        if !admitted {
            return json_error(
                StatusCode::UNAUTHORIZED,
                "CUA requires initialized envd credentials",
            );
        }
        return next.run(request).await;
    }
    let result = configuration.access.authorize(
        request.method().as_str(),
        request.uri().path(),
        request
            .headers()
            .get("x-access-token")
            .and_then(|header| header.to_str().ok()),
    );
    drop(configuration);
    if let Err(error) = result {
        return json_error(StatusCode::UNAUTHORIZED, &error.to_string());
    }
    if let Some(username) = crate::exec_context::basic_username(request.headers()) {
        let checked = tokio::task::spawn_blocking(move || {
            let invalid = || {
                connectrpc::ConnectError::unauthenticated(format!("invalid username: '{username}'"))
            };
            crate::platform::lookup_account(&username).map_err(|_| invalid())?;
            Ok::<_, connectrpc::ConnectError>(())
        })
        .await
        .unwrap_or_else(|_| Err(connectrpc::ConnectError::internal("account lookup failed")));
        if let Err(error) = checked {
            return basic_error_response(error, &request);
        }
    }
    let gzip_response = crate::transport::input_response_encoding(&request);
    let response = next.run(crate::transport::observe_request(request)).await;
    crate::transport::encode_input_response(response, gzip_response).await
}

/// Go http.Error: plain text, newline-terminated, nosniff.
pub fn plain_http_error(status: StatusCode, message: &str) -> Response {
    let mut response = (status, format!("{message}\n")).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    response
}

/// The JSON error envelope the oracle's REST handlers put on the wire.
pub fn json_error(status: StatusCode, message: &str) -> Response {
    let mut response = (
        status,
        format!(
            "{}\n",
            serde_json::json!({"code":status.as_u16(),"message":message})
        ),
    )
        .into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        header::HeaderValue::from_static("nosniff"),
    );
    response
}

pub fn basic_error_response(error: connectrpc::ConnectError, request: &Request<Body>) -> Response {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let content_type = content_type.as_str();
    let is_post = request.method() == Method::POST;
    if is_post
        && (content_type == "application/grpc-web"
            || content_type.starts_with("application/grpc-web+"))
    {
        let mut response = Body::empty().into_response();
        if let Ok(value) = header::HeaderValue::from_str(content_type) {
            response.headers_mut().insert(header::CONTENT_TYPE, value);
        }
        let code = if error.code == connectrpc::ErrorCode::Unauthenticated {
            "16"
        } else {
            "13"
        };
        response
            .headers_mut()
            .insert("grpc-status", header::HeaderValue::from_static(code));
        let message = error.message.as_deref().unwrap_or("");
        let escaped: String = message
            .bytes()
            .map(|byte| {
                if !(0x20..0x7f).contains(&byte) || byte == b'%' {
                    format!("%{byte:02X}")
                } else {
                    char::from(byte).to_string()
                }
            })
            .collect();
        if let Ok(value) = header::HeaderValue::from_str(&escaped) {
            response.headers_mut().insert("grpc-message", value);
        }
        return response;
    }
    let valid_version = request
        .headers()
        .get("connect-protocol-version")
        .is_none_or(|value| value.is_empty() || value == "1");
    let grpc = content_type == "application/grpc" || content_type.starts_with("application/grpc+");
    let streaming = content_type.starts_with("application/connect+") && valid_version;
    if is_post && (grpc || streaming) {
        let mut headers = request.headers().clone();
        if let Ok(value) = header::HeaderValue::from_str(content_type) {
            headers.insert(header::CONTENT_TYPE, value);
        }
        error.into_http_response(&headers).map(Body::new)
    } else {
        error
            .into_http_response(&header::HeaderMap::new())
            .map(Body::new)
    }
}

/// store.go:193: 204 with `Cache-Control: no-store` and an EMPTY content-type
/// header (verified by the malformed-input gate) rather than none at all.
pub async fn health() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, header::HeaderValue::from_static(""));
    response
}

/// Go sets application/json and encodes with json.NewEncoder, whose trailing
/// newline is on the wire.
pub async fn envs<S: ConfigurationState>(State(state): State<Arc<S>>) -> Response {
    let configuration = state.configuration().lock().await;
    let mut response = match serde_json::to_vec(&configuration.environment.all()) {
        Ok(mut body) => {
            body.push(b'\n');
            Body::from(body).into_response()
        }
        Err(error) => {
            eprintln!("failed to encode environment: {error}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

#[cfg(test)]
mod options_asterisk_tests {
    use super::*;

    fn request(method: &str, target: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(target)
            .body(Body::empty())
            .expect("request")
    }

    #[test]
    fn only_the_exact_star_target_is_general_options() {
        // `*?x` cannot reach here: `valid_request_uri` rejects it as Go's
        // `url.ParseRequestURI` does, before hyper parses the head.
        assert!(is_general_options(&request("OPTIONS", "*")));
        assert!(!is_general_options(&request("OPTIONS", "/health")));
        assert!(!is_general_options(&request("GET", "*")));
    }
}
