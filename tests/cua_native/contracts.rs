use super::*;

#[tokio::test]
async fn comma_in_installed_token_is_rejected_by_cua_without_changing_s0_auth() {
    let fixture = Fixture::new();
    const COMMA_TOKEN: &str = "first,second";
    fixture.install(COMMA_TOKEN).await;
    let polls = Arc::new(AtomicUsize::new(0));
    let response = send(
        &fixture.router,
        request("GET", "/cua/tools", Some(COMMA_TOKEN), unread_body(&polls)),
    )
    .await;
    error(response, StatusCode::UNAUTHORIZED).await;
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    assert!(fixture.tools.calls().is_empty());
    let response = send(
        &fixture.router,
        request("GET", "/envs", Some(COMMA_TOKEN), Body::empty()),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "S0 still accepts its exact installed token"
    );
}

#[test]
fn public_fake_api_is_send_sync_and_windows_context_selection_is_pure() {
    fn assert_send_sync<T: Send + Sync>() {}
    fn assert_send<T: Send>(_: T) {}
    assert_send_sync::<CuaService>();
    assert_send_sync::<FakeTools>();
    let tools = FakeTools::default();
    assert_send(tools.invoke("probe", json!({"text": "not polled"})));
    assert!(tools.calls().is_empty());
    let defaults = ToolDescription::default();
    assert!(!defaults.destructive);
    assert!(!defaults.open_world);
    for (attached, session) in [
        (true, Some(7)),
        (true, None),
        (false, Some(0)),
        (false, None),
    ] {
        assert!(matches!(
            DesktopContext::for_windows(attached, session),
            DesktopContext::Unavailable
        ));
    }
    assert!(matches!(
        DesktopContext::for_windows(false, Some(7)),
        DesktopContext::DesktopSession
    ));
}

#[tokio::test]
async fn shared_cors_preflight_never_reads_or_executes_a_tool() {
    let fixture = Fixture::new();
    let polls = Arc::new(AtomicUsize::new(0));
    let preflight = Request::builder()
        .method("OPTIONS")
        .uri("/cua/call")
        .header("origin", "https://fixture.invalid")
        .header("access-control-request-method", "POST")
        .header(
            "access-control-request-headers",
            "content-type,x-access-token",
        )
        .body(unread_body(&polls))
        .unwrap();
    let response = send(&fixture.router, preflight).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()["access-control-allow-methods"], "POST");
    assert_eq!(response.headers()["access-control-allow-origin"], "*");
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    assert!(fixture.tools.calls().is_empty());
}

#[tokio::test]
async fn tools_and_calls_use_native_schema_and_content_without_a_handshake() {
    let fixture = Fixture::new();
    fixture.install(TOKEN).await;
    let response = send(
        &fixture.router,
        request("GET", "/cua/tools", Some(TOKEN), Body::empty()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let tools = document(response).await;
    assert_eq!(tools.as_object().unwrap().len(), 1);
    let entries = tools["tools"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "probe");
    assert_eq!(entries[0]["description"], "Echo isolated fixture input");
    assert_eq!(entries[0]["input_schema"]["required"], json!(["text"]));
    assert!(entries[0].get("inputSchema").is_none());
    let arguments = json!({"text": "雪 '); DROP TABLE fixture; --", "count": 2});
    let response = send(&fixture.router, call(Some(TOKEN), arguments.clone())).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        document(response).await,
        json!({
            "success": true,
            "content": [
                {"type": "text", "text": arguments["text"]},
                {"type": "image", "data": "AA==", "mime_type": "image/png"}
            ],
            "data": arguments
        })
    );
    assert_eq!(fixture.tools.calls(), vec![("probe".into(), arguments)]);
}

#[tokio::test]
async fn tool_reported_failure_is_a_native_result_with_http_200() {
    let fixture = Fixture::new();
    fixture.install(TOKEN).await;
    let response = send(&fixture.router, call(Some(TOKEN), json!({"text": "fail"}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let value = document(response).await;
    assert_eq!(value["success"], false);
    assert_eq!(
        value["content"],
        json!([{"type": "text", "text": "fixture refused action"}])
    );
    assert!(value["data"].is_null());
    for legacy in ["jsonrpc", "id", "result", "isError", "structuredContent"] {
        assert!(value.get(legacy).is_none(), "legacy field {legacy}");
    }
    assert_eq!(fixture.tools.calls().len(), 1);
}

#[tokio::test]
async fn malformed_native_bodies_and_schema_violations_never_reach_tools() {
    let fixture = Fixture::new();
    fixture.install(TOKEN).await;
    for body in [
        "",
        "{",
        "null",
        "[]",
        "42",
        "{}",
        r#"{"name":42,"arguments":{}}"#,
        r#"{"name":"","arguments":{}}"#,
        r#"{"name":"probe"}"#,
        r#"{"name":"probe","arguments":null}"#,
        r#"{"name":"probe","arguments":[]}"#,
        r#"{"name":"probe","arguments":{}}"#,
        r#"{"name":"probe","arguments":{"text":42}}"#,
        r#"{"name":"probe","arguments":{"text":""}}"#,
        r#"{"name":"probe","arguments":{"text":"x","count":-1}}"#,
        r#"{"name":"probe","arguments":{"text":"x","count":3}}"#,
        r#"{"name":"probe","arguments":{"text":"x","count":0.5}}"#,
        r#"{"name":"probe","arguments":{"text":"x","extra":true}}"#,
        r#"{"name":"probe","arguments":{"text":"x"},"extra":true}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"probe","arguments":{"text":"x"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"[{"jsonrpc":"2.0","id":1,"method":"tools/list"}]"#,
        r#"{"name":"probe","arguments":{"text":"x"},"jsonrpc":"2.0","id":1}"#,
    ] {
        let response = send(
            &fixture.router,
            request("POST", "/cua/call", Some(TOKEN), Body::from(body)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "body {body}");
        error(response, StatusCode::BAD_REQUEST).await;
    }
    error(
        send(
            &fixture.router,
            request(
                "POST",
                "/cua/call",
                Some(TOKEN),
                Body::from(r#"{"name":"missing-tool","arguments":{}}"#),
            ),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;
    assert!(fixture.tools.calls().is_empty());
}

#[tokio::test]
async fn wrong_methods_media_types_and_removed_routes_use_s0_errors() {
    let fixture = Fixture::new();
    fixture.install(TOKEN).await;
    for (method, path) in [
        ("GET", "/cua/call"),
        ("POST", "/cua/tools"),
        ("POST", "/cua/health"),
        ("POST", "/cua/metrics"),
    ] {
        error(
            send(
                &fixture.router,
                request(method, path, Some(TOKEN), Body::from("{")),
            )
            .await,
            StatusCode::METHOD_NOT_ALLOWED,
        )
        .await;
    }
    for media_type in [None, Some("text/plain"), Some("application/json-rpc")] {
        let mut denied = call(Some(TOKEN), json!({"text": "not executed"}));
        denied.headers_mut().remove(header::CONTENT_TYPE);
        if let Some(media_type) = media_type {
            denied
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type));
        }
        error(
            send(&fixture.router, denied).await,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        )
        .await;
    }
    for path in ["/cua/mcp", "/cua/info", "/cua/unknown"] {
        error(
            send(
                &fixture.router,
                request("POST", path, Some(TOKEN), Body::from("{")),
            )
            .await,
            StatusCode::NOT_FOUND,
        )
        .await;
    }
    assert!(fixture.tools.calls().is_empty());
}

#[tokio::test]
async fn request_limit_is_inclusive_and_serialized_responses_are_bounded() {
    let fixture = Fixture::new();
    fixture.install(TOKEN).await;
    let overhead = json!({"name": "probe", "arguments": {"text": ""}})
        .to_string()
        .len();
    let exact =
        json!({"name": "probe", "arguments": {"text": "x".repeat(REQUEST_LIMIT - overhead)}})
            .to_string();
    assert_eq!(exact.len(), REQUEST_LIMIT);
    let over = format!("{exact} "); // Still valid JSON, one byte beyond the cap.
    error(
        send(
            &fixture.router,
            request("POST", "/cua/call", Some(TOKEN), Body::from(over)),
        )
        .await,
        StatusCode::PAYLOAD_TOO_LARGE,
    )
    .await;
    assert!(fixture.tools.calls().is_empty());
    let response = send(
        &fixture.router,
        request("POST", "/cua/call", Some(TOKEN), Body::from(exact)),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        document(response).await["data"]["text"]
            .as_str()
            .unwrap()
            .len(),
        REQUEST_LIMIT - overhead
    );
    error(
        send(
            &fixture.router,
            call(Some(TOKEN), json!({"text": "oversized"})),
        )
        .await,
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await;
    assert_eq!(fixture.tools.calls().len(), 2);
}

#[tokio::test]
async fn health_and_metrics_are_passive_native_observations() {
    let fixture = Fixture::new();
    fixture.install(TOKEN).await;
    let response = send(
        &fixture.router,
        call(Some(TOKEN), json!({"text": "metric fixture"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let descriptions = fixture.tools.descriptions.load(Ordering::SeqCst);
    for _ in 0..2 {
        let response = send(
            &fixture.router,
            request("GET", "/cua/health", Some(TOKEN), Body::empty()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let health = document(response).await;
        assert_eq!(health["runtime_available"], true);
        for legacy in [
            "jsonrpc",
            "mcp_url",
            "proxy_path",
            "helper_transport_auth",
            "mode",
        ] {
            assert!(health.get(legacy).is_none());
        }
        let response = send(
            &fixture.router,
            request("GET", "/cua/metrics", Some(TOKEN), Body::empty()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/plain"));
        let metrics = text(response).await;
        assert!(metrics
            .contains("coorworker_envd_cua_tool_calls_total{tool=\"probe\",status=\"ok\"} 1"));
        assert!(metrics.contains("coorworker_envd_cua_tool_latency_seconds_sum "));
        assert!(metrics.contains("coorworker_envd_cua_ready 1"));
        assert!(
            !metrics.contains("metric fixture"),
            "no argument-derived labels"
        );
        assert!(!metrics.contains("coorworker_envd_cua_worker_"));
    }
    assert_eq!(fixture.tools.calls().len(), 1);
    assert_eq!(
        fixture.tools.descriptions.load(Ordering::SeqCst),
        descriptions
    );
}

#[tokio::test]
async fn cua_namespace_ends_at_a_segment_and_only_cua_bypasses_basic_username_lookup() {
    let fixture = Fixture::new();
    let response = send(
        &fixture.router,
        request("GET", "/cuathing", None, Body::empty()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(text(response).await, "404 page not found\n");
    fixture.install(TOKEN).await;
    error(
        send(
            &fixture.router,
            request("GET", "/cuathing", None, Body::empty()),
        )
        .await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let response = send(
        &fixture.router,
        request("GET", "/cuathing", Some(TOKEN), Body::empty()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(text(response).await, "404 page not found\n");
    const INVALID_BASIC: &str = "Basic X19lbnZkX21pc3NpbmdfdXNlcjp4";
    for path in ["/envs", "/cuathing"] {
        let mut denied = request("GET", path, Some(TOKEN), Body::empty());
        denied.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static(INVALID_BASIC),
        );
        let response = send(&fixture.router, denied).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let value = document(response).await;
        assert_eq!(value["code"], "unauthenticated");
        assert_eq!(value["message"], "invalid username: '__envd_missing_user'");
    }
    let mut admitted = call(
        Some(TOKEN),
        json!({"text": "CUA uses only its access token"}),
    );
    admitted.headers_mut().insert(
        header::AUTHORIZATION,
        HeaderValue::from_static(INVALID_BASIC),
    );
    let response = send(&fixture.router, admitted).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(document(response).await["success"], true);
    assert_eq!(fixture.tools.calls().len(), 1);
}
