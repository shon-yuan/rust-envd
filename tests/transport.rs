//! Runtime selection probes; these handlers deliberately do not execute guest commands.
use std::convert::Infallible;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use buffa::Message;
use bytes::Bytes;
use connectrpc::handler::{client_streaming_handler_fn, handler_fn, streaming_handler_fn};
use connectrpc::{
    ConnectRpcService, RequestContext, Response, Router, ServiceResult, ServiceStream,
};
use coorworker_envd::{proto::process, transport::rpc_service};
use futures_util::{stream, StreamExt};
use http::{Request, StatusCode};
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::{
    body::Frame,
    client::conn::{http1, http2},
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::{
    net::TcpStream,
    sync::{mpsc, Notify},
};

fn envelope(payload: &[u8]) -> Bytes {
    let mut bytes = Vec::with_capacity(5 + payload.len());
    bytes.push(0);
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes.into()
}

struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

async fn spawn(router: Router) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let bound = connectrpc::Server::bind("127.0.0.1:0").await.unwrap();
    let addr = bound.local_addr().unwrap();
    let service: ConnectRpcService = rpc_service(router);
    let task = tokio::spawn(async move {
        bound.serve_with_service(service).await.unwrap();
    });
    (addr, task)
}

async fn stream_handler(
    _: RequestContext,
    _: process::StartRequest,
) -> ServiceResult<ServiceStream<process::StartResponse>> {
    let first = serde_json::from_value(serde_json::json!({"event":{"start":{"pid":17}}})).unwrap();
    Response::stream_ok(stream::once(async { Ok(first) }).chain(stream::pending()))
}

#[tokio::test]
async fn grpc_http2_stream_has_protobuf_bytes_and_terminal_trailers() {
    bounded_test(async {
        let router = Router::new().route_server_stream(
            "process.Process",
            "Start",
            streaming_handler_fn(|_: RequestContext, _: process::StartRequest| async {
                let response: process::StartResponse =
                    serde_json::from_value(serde_json::json!({"event":{"data":{"stdout":"/wCA"}}}))
                        .unwrap();
                Response::stream_ok(stream::iter([Ok(response)]))
            }),
        );
        let (addr, server) = spawn(router).await;
        let (mut client, connection) = http2::handshake(
            TokioExecutor::new(),
            TokioIo::new(TcpStream::connect(addr).await.unwrap()),
        )
        .await
        .unwrap();
        let driver = tokio::spawn(connection);
        let request = Request::post(format!("http://{addr}/process.Process/Start"))
            .header("content-type", "application/grpc+proto")
            .header("te", "trailers")
            .body(Full::new(envelope(
                &process::StartRequest::default().encode_to_vec(),
            )))
            .unwrap();
        let response = client.send_request(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.version(), http::Version::HTTP_2);
        let body = response.into_body().collect().await.unwrap();
        assert_eq!(body.trailers().unwrap()["grpc-status"], "0");
        let bytes = body.to_bytes();
        assert_eq!(bytes[0], 0);
        let length = u32::from_be_bytes(bytes[1..5].try_into().unwrap()) as usize;
        assert_eq!(bytes.len(), length + 5);
        let message = process::StartResponse::decode_from_slice(&bytes[5..]).unwrap();
        assert_eq!(
            serde_json::to_value(message).unwrap()["event"]["data"]["stdout"],
            "/wCA"
        );
        driver.abort();
        server.abort();
    })
    .await;
}

#[tokio::test]
async fn connect_rejects_unknown_protocol_version() {
    bounded_test(async {
        let router = Router::new().route_server_stream(
            "process.Process",
            "Start",
            streaming_handler_fn(stream_handler),
        );
        let (addr, server) = spawn(router).await;
        let (mut client, connection) =
            http1::handshake(TokioIo::new(TcpStream::connect(addr).await.unwrap()))
                .await
                .unwrap();
        let driver = tokio::spawn(connection);
        let request = Request::post("/process.Process/Start")
            .header("host", addr.to_string())
            .header("content-type", "application/connect+json")
            .header("connect-protocol-version", "2")
            .body(Full::new(envelope(b"{}")))
            .unwrap();
        let response = client.send_request(request).await.unwrap();
        // Go Connect reports pre-handler streaming errors in an EndStream envelope (HTTP 200).
        assert_eq!(response.status(), StatusCode::OK);
        let body = tokio::time::timeout(Duration::from_secs(2), response.into_body().collect())
            .await
            .expect("invalid protocol version must terminate immediately")
            .unwrap()
            .to_bytes();
        assert_eq!(body[0], 2);
        assert!(String::from_utf8_lossy(&body).contains("invalid_argument"));
        driver.abort();
        server.abort();
    })
    .await;
}

#[tokio::test]
async fn streaming_deadline_is_enforced_after_headers() {
    bounded_test(async {
        let dropped = Arc::new(AtomicBool::new(false));
        let (addr, server) = spawn(guarded_start_router(dropped.clone())).await;
        let socket = TcpStream::connect(addr).await.unwrap();
        let (mut client, connection) = http1::handshake(TokioIo::new(socket)).await.unwrap();
        let driver = tokio::spawn(connection);
        let request = Request::post("/process.Process/Start")
            .header("host", addr.to_string())
            .header("content-type", "application/connect+json")
            .header("connect-protocol-version", "1")
            .header("connect-timeout-ms", "150")
            .body(Full::new(envelope(b"{}")))
            .unwrap();
        let mut response = client.send_request(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let first = response
            .body_mut()
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap();
        assert!(String::from_utf8_lossy(&first).contains("17"));
        let rest = tokio::time::timeout(Duration::from_secs(2), response.into_body().collect())
            .await
            .expect("stream deadline must end the body")
            .unwrap()
            .to_bytes();
        assert!(String::from_utf8_lossy(&rest).contains("deadline_exceeded"));
        wait_for_drop(&dropped).await;
        driver.abort();
        server.abort();
    })
    .await;
}

#[tokio::test]
async fn disconnect_drops_stream_subscription() {
    bounded_test(async {
        let dropped = Arc::new(AtomicBool::new(false));
        let observed = dropped.clone();
        let router = Router::new().route_server_stream(
            "process.Process",
            "Start",
            streaming_handler_fn(move |_: RequestContext, _: process::StartRequest| {
                let guard = Dropped(observed.clone());
                async move {
                    let event: process::StartResponse =
                        serde_json::from_value(serde_json::json!({"event":{"start":{"pid":17}}}))
                            .unwrap();
                    Response::stream_ok(stream::unfold(
                        (Some(event), guard),
                        |(event, guard)| async move {
                            match event {
                                Some(event) => Some((Ok(event), (None, guard))),
                                None => std::future::pending().await,
                            }
                        },
                    ))
                }
            }),
        );
        let (addr, server) = spawn(router).await;
        let (mut client, connection) =
            http1::handshake(TokioIo::new(TcpStream::connect(addr).await.unwrap()))
                .await
                .unwrap();
        let driver = tokio::spawn(connection);
        let request = Request::post("/process.Process/Start")
            .header("host", addr.to_string())
            .header("content-type", "application/connect+json")
            .header("connect-protocol-version", "1")
            .body(Full::new(envelope(b"{}")))
            .unwrap();
        let mut response = client.send_request(request).await.unwrap();
        response.body_mut().frame().await.unwrap().unwrap();
        drop(response);
        drop(client);
        driver.abort();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("disconnect must drop subscription state");
        server.abort();
    })
    .await;
}

#[tokio::test]
async fn client_input_reaches_handler_before_request_eof() {
    bounded_test(async {
        let received = Arc::new(Notify::new());
        let handler_received = received.clone();
        let router = Router::new().route_client_stream(
            "process.Process",
            "StreamInput",
            client_streaming_handler_fn(
                move |_: RequestContext, mut input: ServiceStream<process::StreamInputRequest>| {
                    let received = handler_received.clone();
                    async move {
                        let mut count = 0;
                        while let Some(message) = input.next().await {
                            message?;
                            count += 1;
                            received.notify_one();
                        }
                        assert_eq!(count, 2);
                        Response::ok(process::StreamInputResponse::default())
                    }
                },
            ),
        );
        let (addr, server) = spawn(router).await;
        let (mut client, connection) =
            http1::handshake(TokioIo::new(TcpStream::connect(addr).await.unwrap()))
                .await
                .unwrap();
        let driver = tokio::spawn(connection);
        let (sender, receiver) = mpsc::channel::<Bytes>(2);
        let body = StreamBody::new(stream::unfold(receiver, |mut receiver| async move {
            receiver
                .recv()
                .await
                .map(|bytes| (Ok::<_, Infallible>(Frame::data(bytes)), receiver))
        }));
        let request = Request::post("/process.Process/StreamInput")
            .header("host", addr.to_string())
            .header("content-type", "application/connect+json")
            .header("connect-protocol-version", "1")
            .body(body)
            .unwrap();
        let response = tokio::spawn(async move { client.send_request(request).await.unwrap() });
        sender
            .send(envelope(br#"{"start":{"process":{"pid":17}}}"#))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), received.notified())
            .await
            .expect("input must stream before EOF");
        sender
            .send(envelope(br#"{"data":{"input":{"stdin":"/wCA"}}}"#))
            .await
            .unwrap();
        drop(sender);
        let response = response.await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(!String::from_utf8_lossy(&bytes).contains("error"));
        driver.abort();
        server.abort();
    })
    .await;
}

async fn bounded_test(test: impl std::future::Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(10), test)
        .await
        .expect("transport test exceeded its total time budget");
}

async fn wait_for_drop(dropped: &AtomicBool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("RPC state must be released");
}

fn guarded_start_router(dropped: Arc<AtomicBool>) -> Router {
    Router::new().route_server_stream(
        "process.Process",
        "Start",
        streaming_handler_fn(move |_: RequestContext, _: process::StartRequest| {
            let guard = Dropped(dropped.clone());
            async move {
                let event: process::StartResponse =
                    serde_json::from_value(serde_json::json!({"event":{"start":{"pid":17}}}))
                        .unwrap();
                Response::stream_ok(stream::unfold(
                    (Some(event), guard),
                    |(event, guard)| async move {
                        match event {
                            Some(event) => Some((Ok(event), (None, guard))),
                            None => std::future::pending().await,
                        }
                    },
                ))
            }
        }),
    )
}

fn decode_grpc_message<M: Message>(bytes: &[u8]) -> M {
    assert!(bytes.len() >= 5, "missing gRPC envelope");
    assert_eq!(bytes[0], 0);
    let length = u32::from_be_bytes(bytes[1..5].try_into().unwrap()) as usize;
    assert_eq!(bytes.len(), length + 5);
    M::decode_from_slice(&bytes[5..]).unwrap()
}

#[tokio::test]
async fn http2_reset_drops_subscription_and_connection_still_serves_list() {
    bounded_test(async {
        let dropped = Arc::new(AtomicBool::new(false));
        let router = guarded_start_router(dropped.clone()).route(
            "process.Process",
            "List",
            handler_fn(|_: RequestContext, _: process::ListRequest| async {
                let response: process::ListResponse =
                    serde_json::from_value(serde_json::json!({"processes":[{"pid":29}]})).unwrap();
                Response::ok(response)
            }),
        );
        let (addr, server) = spawn(router).await;
        let (mut client, connection) = http2::handshake(
            TokioExecutor::new(),
            TokioIo::new(TcpStream::connect(addr).await.unwrap()),
        )
        .await
        .unwrap();
        let driver = tokio::spawn(connection);
        let request = Request::post(format!("http://{addr}/process.Process/Start"))
            .header("content-type", "application/grpc+proto")
            .header("te", "trailers")
            .body(Full::new(envelope(
                &process::StartRequest::default().encode_to_vec(),
            )))
            .unwrap();
        let mut response = client.send_request(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let first = response
            .body_mut()
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap();
        let message: process::StartResponse = decode_grpc_message(&first);
        assert_eq!(
            serde_json::to_value(message).unwrap()["event"]["start"]["pid"],
            17
        );
        assert!(!dropped.load(Ordering::SeqCst));
        // Dropping an unfinished H2 response resets only this stream. Keep the
        // driver and SendRequest alive, so connection shutdown cannot satisfy this.
        drop(response);
        wait_for_drop(&dropped).await;
        assert!(
            !driver.is_finished(),
            "connection must survive stream cancellation"
        );
        let request = Request::post(format!("http://{addr}/process.Process/List"))
            .header("content-type", "application/grpc+proto")
            .header("te", "trailers")
            .body(Full::new(envelope(
                &process::ListRequest::default().encode_to_vec(),
            )))
            .unwrap();
        let response = client.send_request(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.version(), http::Version::HTTP_2);
        let body = response.into_body().collect().await.unwrap();
        assert_eq!(body.trailers().unwrap()["grpc-status"], "0");
        let list: process::ListResponse = decode_grpc_message(&body.to_bytes());
        assert_eq!(list.processes.len(), 1);
        assert_eq!(list.processes[0].pid, 29);
        driver.abort();
        server.abort();
    })
    .await;
}

#[tokio::test]
async fn grpc_deadline_returns_failure_trailers_and_drops_subscription() {
    bounded_test(async {
        let dropped = Arc::new(AtomicBool::new(false));
        let (addr, server) = spawn(guarded_start_router(dropped.clone())).await;
        let (mut client, connection) = http2::handshake(
            TokioExecutor::new(),
            TokioIo::new(TcpStream::connect(addr).await.unwrap()),
        )
        .await
        .unwrap();
        let driver = tokio::spawn(connection);
        let request = Request::post(format!("http://{addr}/process.Process/Start"))
            .header("content-type", "application/grpc+proto")
            .header("te", "trailers")
            .header("grpc-timeout", "500m")
            .body(Full::new(envelope(
                &process::StartRequest::default().encode_to_vec(),
            )))
            .unwrap();
        let mut response = client.send_request(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.version(), http::Version::HTTP_2);
        assert!(response.headers().get("grpc-status").is_none());
        let first = response
            .body_mut()
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap();
        let message: process::StartResponse = decode_grpc_message(&first);
        assert_eq!(
            serde_json::to_value(message).unwrap()["event"]["start"]["pid"],
            17
        );
        assert!(!dropped.load(Ordering::SeqCst));
        let rest = response.into_body().collect().await.unwrap();
        // DEADLINE_EXCEEDED must be an actual terminal H2 trailer, not a JSON
        // message, a transport reset, or a successful grpc-status.
        assert_eq!(rest.trailers().unwrap()["grpc-status"], "4");
        assert!(rest.to_bytes().is_empty());
        wait_for_drop(&dropped).await;
        driver.abort();
        server.abort();
    })
    .await;
}

#[tokio::test]
async fn upload_deadline_drops_handler_without_waiting_for_request_eof() {
    bounded_test(async {
        let dropped = Arc::new(AtomicBool::new(false));
        let handler_dropped = dropped.clone();
        let received = Arc::new(Notify::new());
        let handler_received = received.clone();
        let router = Router::new().route_client_stream(
            "process.Process",
            "StreamInput",
            client_streaming_handler_fn(
                move |_: RequestContext, mut input: ServiceStream<process::StreamInputRequest>| {
                    let guard = Dropped(handler_dropped.clone());
                    let received = handler_received.clone();
                    async move {
                        let _guard = guard;
                        input.next().await.expect("first upload message").unwrap();
                        received.notify_one();
                        let unexpected = input.next().await;
                        Err::<Response<process::StreamInputResponse>, _>(
                            connectrpc::ConnectError::internal(format!(
                                "upload unexpectedly completed before cancellation: {unexpected:?}"
                            )),
                        )
                    }
                },
            ),
        );
        let (addr, server) = spawn(router).await;
        let (mut client, connection) =
            http1::handshake(TokioIo::new(TcpStream::connect(addr).await.unwrap()))
                .await
                .unwrap();
        let driver = tokio::spawn(connection);
        let (sender, receiver) = mpsc::channel::<Bytes>(2);
        let body = StreamBody::new(stream::unfold(receiver, |mut receiver| async move {
            receiver
                .recv()
                .await
                .map(|bytes| (Ok::<_, Infallible>(Frame::data(bytes)), receiver))
        }));
        let request = Request::post("/process.Process/StreamInput")
            .header("host", addr.to_string())
            .header("content-type", "application/connect+json")
            .header("connect-protocol-version", "1")
            .header("connect-timeout-ms", "500")
            .body(body)
            .unwrap();
        let response = tokio::spawn(async move { client.send_request(request).await.unwrap() });
        sender
            .send(envelope(br#"{"start":{"process":{"pid":17}}}"#))
            .await
            .unwrap();
        received.notified().await;
        assert!(!dropped.load(Ordering::SeqCst));
        // Keep sender alive: there is no EOF or EndStream to complete the handler.
        let response = response.await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body[0], 2);
        let length = u32::from_be_bytes(body[1..5].try_into().unwrap()) as usize;
        assert_eq!(body.len(), length + 5);
        let end: serde_json::Value = serde_json::from_slice(&body[5..]).unwrap();
        assert_eq!(end["error"]["code"], "deadline_exceeded");
        wait_for_drop(&dropped).await;
        drop(sender);
        driver.abort();
        server.abort();
    })
    .await;
}
