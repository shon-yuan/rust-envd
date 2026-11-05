//! Native CUA contract through the ordinary daemon app (docs/native-cua.md).
//! RED: requires the new public `cua` API and both daemons' `with_cua` builder.
//! All execution uses in-memory tools; no desktop registry, helper, or socket.
#![cfg(any(target_os = "linux", windows))]

use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use axum::{
    body::{to_bytes, Body, Bytes},
    http::{header, HeaderValue, Request, StatusCode},
    response::Response,
    Router,
};
use coorworker_envd::{
    auth::ReadWrite,
    cua::{CallFuture, CuaService, DesktopContext, DesktopTools, ToolDescription, ToolOutput},
    daemon::{app, Daemon},
    http::ConfigurationState,
};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tower::ServiceExt;

const TOKEN: &str = "native-cua-fixture-token";
const WAIT: Duration = Duration::from_secs(5);
const REQUEST_LIMIT: usize = 1 << 20;
const CUA_PATHS: [(&str, &str); 8] = [
    ("GET", "/cua/tools"),
    ("POST", "/cua/call"),
    ("GET", "/cua/health"),
    ("GET", "/cua/metrics"),
    ("GET", "/cua"),
    ("GET", "/cua/"),
    ("POST", "/cua/mcp"),
    ("GET", "/cua/unknown"),
];

#[derive(Default)]
struct FakeTools {
    descriptions: AtomicUsize,
    calls: Mutex<Vec<(String, Value)>>,
    block_next: AtomicBool,
    entered: Notify,
    release: Notify,
}

impl FakeTools {
    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap().clone()
    }
}

impl DesktopTools for FakeTools {
    fn tools(&self) -> Vec<ToolDescription> {
        self.descriptions.fetch_add(1, Ordering::SeqCst);
        vec![ToolDescription {
            name: "probe".into(),
            description: "Echo isolated fixture input".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "minLength": 1},
                    "count": {"type": "integer", "minimum": 0, "maximum": 2}
                },
                "required": ["text"],
                "additionalProperties": false
            }),
            read_only: true,
            idempotent: true,
            ..ToolDescription::default()
        }]
    }

    fn invoke<'a>(&'a self, name: &'a str, args: Value) -> CallFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push((name.into(), args.clone()));
            if self.block_next.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            match args["text"].as_str().unwrap() {
                "fail" => ToolOutput {
                    success: false,
                    content: vec![json!({"type": "text", "text": "fixture refused action"})],
                    data: None,
                },
                "oversized" => ToolOutput {
                    success: true,
                    content: vec![json!({"type": "text", "text": "x".repeat(32 << 20)})],
                    data: None,
                },
                _ => ToolOutput {
                    success: true,
                    content: vec![
                        json!({"type": "text", "text": args["text"]}),
                        json!({"type": "image", "data": "AA==", "mime_type": "image/png"}),
                    ],
                    data: Some(args),
                },
            }
        })
    }
}

// Inject the existing platform metadata seams so /init rotation/reset is real,
// without contacting Linux MMDS or reading Windows provisioning files.
#[derive(Default)]
struct TokenSource(Mutex<Option<String>>);

impl TokenSource {
    fn set(&self, token: &str) {
        *self.0.lock().unwrap() = Some(token.into());
    }
}

#[cfg(target_os = "linux")]
impl coorworker_envd::mmds::TokenHashProvider for TokenSource {
    fn token_hash(&self) -> coorworker_envd::mmds::BoxFuture<'_, std::io::Result<String>> {
        use sha2::{Digest, Sha512};
        let token = self.0.lock().unwrap().clone();
        Box::pin(async move {
            Ok(token.map_or_else(String::new, |token| {
                format!("{:x}", Sha512::digest(token.as_bytes()))
            }))
        })
    }
}

#[cfg(windows)]
impl coorworker_envd::platform::MetadataBackend for TokenSource {
    fn init_token(&self) -> std::io::Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().clone().map(String::into_bytes))
    }
}

fn daemon_with_tokens(source: Arc<TokenSource>) -> Daemon {
    #[cfg(target_os = "linux")]
    let daemon = Daemon::with_token_hash_provider(source);
    #[cfg(windows)]
    let daemon = Daemon::with_metadata(source);
    daemon.with_now(Arc::new(|| 0))
}

struct Fixture {
    router: Router,
    daemon: Arc<Daemon>,
    source: Arc<TokenSource>,
    tools: Arc<FakeTools>,
}

impl Fixture {
    fn new() -> Self {
        let source = Arc::new(TokenSource::default());
        let tools = Arc::new(FakeTools::default());
        let service = Arc::new(CuaService::new(tools.clone()));
        let daemon = Arc::new(daemon_with_tokens(source.clone()).with_cua(service));
        Self {
            router: app(daemon.clone()),
            daemon,
            source,
            tools,
        }
    }

    async fn handover() -> Self {
        let source = Arc::new(TokenSource::default());
        let tools = Arc::new(FakeTools::default());
        let daemon =
            daemon_with_tokens(source.clone()).with_cua(Arc::new(CuaService::new(tools.clone())));
        let daemon = Arc::new(daemon.with_handover_auth().await);
        Self {
            router: app(daemon.clone()),
            daemon,
            source,
            tools,
        }
    }

    async fn install(&self, token: &str) {
        self.source.set(token);
        init(&self.router, json!({"accessToken": token})).await;
        assert!(self.daemon.initialized().await);
    }

    async fn admission(&self, token: Option<&str>, expected: StatusCode) {
        let before = self.tools.calls().len();
        let response = send(
            &self.router,
            call(token, json!({"text": "credential regression"})),
        )
        .await;
        assert_eq!(response.status(), expected, "token {token:?}");
        if expected == StatusCode::OK {
            assert_eq!(document(response).await["success"], true);
        } else {
            error(response, expected).await;
        }
        assert_eq!(
            self.tools.calls().len(),
            before + usize::from(expected == StatusCode::OK)
        );
    }
}

fn request(method: &str, path: &str, token: Option<&str>, body: Body) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8");
    if let Some(token) = token {
        builder = builder.header("x-access-token", token);
    }
    builder.body(body).unwrap()
}

fn call(token: Option<&str>, arguments: Value) -> Request<Body> {
    request(
        "POST",
        "/cua/call",
        token,
        Body::from(json!({"name": "probe", "arguments": arguments}).to_string()),
    )
}

fn unread_body(polls: &Arc<AtomicUsize>) -> Body {
    let polls = polls.clone();
    Body::from_stream(futures_util::stream::once(async move {
        polls.fetch_add(1, Ordering::SeqCst);
        Ok::<_, std::io::Error>(Bytes::from_static(b"{"))
    }))
}

async fn send(router: &Router, request: Request<Body>) -> Response {
    tokio::time::timeout(WAIT, router.clone().oneshot(request))
        .await
        .expect("daemon request blocked")
        .unwrap()
}

async fn init(router: &Router, body: Value) {
    let response = send(
        router,
        request("POST", "/init", None, Body::from(body.to_string())),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT, "init {body}");
}

async fn text(response: Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), 4 << 20)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

async fn document(response: Response) -> Value {
    assert!(response.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("application/json"));
    serde_json::from_str(&text(response).await).unwrap()
}

async fn error(response: Response, expected: StatusCode) {
    assert_eq!(response.status(), expected);
    assert!(response.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("application/json"));
    let body = text(response).await;
    assert!(body.ends_with('\n'), "S0 JSON errors retain their newline");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["code"], expected.as_u16());
    assert!(!value["message"].as_str().unwrap().is_empty());
    assert_eq!(
        value.as_object().unwrap().len(),
        2,
        "native S0 error envelope"
    );
}

#[path = "cua_native/contracts.rs"]
mod contracts;
#[path = "cua_native/lifecycle.rs"]
mod lifecycle;
