use super::*;

#[tokio::test]
async fn every_cua_path_requires_both_completed_init_and_a_nonempty_current_token() {
    let fixture = Fixture::new();
    let polls = Arc::new(AtomicUsize::new(0));
    for phase in ["fresh", "token before init", "tokenless init"] {
        match phase {
            "token before init" => fixture
                .daemon
                .configuration()
                .lock()
                .await
                .access
                .set_token(Some(TOKEN.into()))
                .unwrap(),
            "tokenless init" => {
                fixture
                    .daemon
                    .configuration()
                    .lock()
                    .await
                    .access
                    .set_token(None)
                    .unwrap();
                init(&fixture.router, json!({})).await;
                assert!(fixture.daemon.initialized().await);
            }
            _ => {}
        }
        for (method, path) in CUA_PATHS {
            let response = send(
                &fixture.router,
                request(method, path, Some(TOKEN), unread_body(&polls)),
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{phase}: {path}"
            );
            error(response, StatusCode::UNAUTHORIZED).await;
        }
    }
    assert_eq!(
        polls.load(Ordering::SeqCst),
        0,
        "auth must precede body reads"
    );
    assert!(fixture.tools.calls().is_empty());
    let response = send(
        &fixture.router,
        request("GET", "/health", None, Body::empty()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn authenticated_daemon_still_protects_all_cua_paths_before_decoding() {
    let fixture = Fixture::new();
    fixture.install(TOKEN).await;
    let polls = Arc::new(AtomicUsize::new(0));
    for (method, path) in CUA_PATHS {
        let response = send(
            &fixture.router,
            request(method, path, None, unread_body(&polls)),
        )
        .await;
        error(response, StatusCode::UNAUTHORIZED).await;
    }
    let mut denied = request("POST", "/cua/call", Some("wrong"), unread_body(&polls));
    denied
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    denied.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&(REQUEST_LIMIT + 1).to_string()).unwrap(),
    );
    error(
        send(&fixture.router, denied).await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    assert!(fixture.tools.calls().is_empty());
}

#[tokio::test]
async fn only_one_unambiguous_x_access_token_authenticates_native_calls() {
    let fixture = Fixture::new();
    fixture.install(TOKEN).await;
    let signature = fixture
        .daemon
        .configuration()
        .lock()
        .await
        .access
        .signature("/cua/call", ReadWrite::Write, "", None)
        .unwrap();
    let polls = Arc::new(AtomicUsize::new(0));
    let credentials: Vec<Vec<(&str, HeaderValue)>> = vec![
        vec![(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {TOKEN}")).unwrap(),
        )],
        vec![("authorization", HeaderValue::from_static("Basic cm9vdDp4"))],
        vec![("x-access-token", HeaderValue::from_static(""))],
        vec![("x-access-token", HeaderValue::from_static("wrong"))],
        vec![("x-access-token", HeaderValue::from_bytes(&[0xff]).unwrap())],
        vec![(
            "x-access-token",
            HeaderValue::from_str(&format!("{TOKEN}, {TOKEN}")).unwrap(),
        )],
        vec![
            ("x-access-token", HeaderValue::from_static(TOKEN)),
            ("x-access-token", HeaderValue::from_static(TOKEN)),
        ],
        vec![
            ("x-access-token", HeaderValue::from_static(TOKEN)),
            ("x-access-token", HeaderValue::from_static("wrong")),
        ],
        vec![
            ("x-access-token", HeaderValue::from_static("wrong")),
            ("x-access-token", HeaderValue::from_static(TOKEN)),
        ],
        vec![], // A genuine file signature cannot replace the CUA token.
    ];
    for headers in credentials {
        let mut denied = request(
            "POST",
            &format!("/cua/call?signature={signature}"),
            None,
            unread_body(&polls),
        );
        for (name, value) in headers {
            denied.headers_mut().append(name, value);
        }
        error(
            send(&fixture.router, denied).await,
            StatusCode::UNAUTHORIZED,
        )
        .await;
    }
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    assert!(fixture.tools.calls().is_empty());
    let response = send(
        &fixture.router,
        call(Some(TOKEN), json!({"text": "accepted"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.tools.calls().len(), 1);
}

#[tokio::test]
async fn rotation_and_reset_gate_new_requests_without_cancelling_an_admitted_action() {
    let fixture = Fixture::new();
    fixture.install(TOKEN).await;
    fixture.tools.block_next.store(true, Ordering::SeqCst);
    let router = fixture.router.clone();
    let admitted = tokio::spawn(async move {
        send(
            &router,
            call(Some(TOKEN), json!({"text": "admitted before rotation"})),
        )
        .await
    });
    tokio::time::timeout(WAIT, fixture.tools.entered.notified())
        .await
        .expect("tool never entered");
    fixture.install("replacement").await;
    error(
        send(&fixture.router, call(Some(TOKEN), json!({"text": "stale"}))).await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    error(
        send(
            &fixture.router,
            call(Some("replacement"), json!({"text": "busy"})),
        )
        .await,
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await;
    fixture.source.set(""); // The SHA-512 of empty authorizes the ordinary /init reset.
    init(&fixture.router, json!({})).await;
    for token in [None, Some(TOKEN), Some("replacement")] {
        error(
            send(&fixture.router, call(token, json!({"text": "after reset"}))).await,
            StatusCode::UNAUTHORIZED,
        )
        .await;
    }
    let response = send(
        &fixture.router,
        request("GET", "/health", None, Body::empty()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    fixture.tools.release.notify_one();
    let response = tokio::time::timeout(WAIT, admitted).await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(document(response).await["success"], true);
    assert_eq!(fixture.tools.calls().len(), 1);
    fixture.install("after-reset").await;
    let response = send(
        &fixture.router,
        call(
            Some("after-reset"),
            json!({"text": "slot released", "count": 0}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.tools.calls().len(), 2);
}

#[tokio::test]
async fn failed_and_metadata_authorized_stale_init_preserve_current_cua_credentials() {
    let fixture = Fixture::new();
    fixture.source.set("first");
    init(
        &fixture.router,
        json!({"accessToken": "first", "timestamp": "2026-11-04T00:00:00Z"}),
    )
    .await;
    fixture.admission(Some("first"), StatusCode::OK).await;
    for (body, expected) in [
        (json!({"accessToken": "wrong"}), StatusCode::UNAUTHORIZED),
        (json!({}), StatusCode::UNAUTHORIZED),
        (json!({"accessToken": ""}), StatusCode::BAD_REQUEST),
        (json!({"accessToken": 42}), StatusCode::BAD_REQUEST),
    ] {
        let before = fixture.tools.calls().len();
        let response = send(
            &fixture.router,
            request("POST", "/init", None, Body::from(body.to_string())),
        )
        .await;
        assert_eq!(response.status(), expected, "failed init {body}");
        assert_eq!(fixture.tools.calls().len(), before);
        assert!(fixture.daemon.initialized().await);
        fixture.admission(Some("first"), StatusCode::OK).await;
        fixture
            .admission(Some("wrong"), StatusCode::UNAUTHORIZED)
            .await;
    }
    fixture.source.set("second");
    init(
        &fixture.router,
        json!({"accessToken": "second", "timestamp": "2026-11-04T01:00:00Z"}),
    )
    .await;
    fixture
        .admission(Some("first"), StatusCode::UNAUTHORIZED)
        .await;
    fixture.admission(Some("second"), StatusCode::OK).await;
    // The metadata source authorizes first, but its older timestamp must skip SetData.
    fixture.source.set("first");
    init(
        &fixture.router,
        json!({"accessToken": "first", "timestamp": "2026-11-04T00:00:00Z"}),
    )
    .await;
    fixture
        .admission(Some("first"), StatusCode::UNAUTHORIZED)
        .await;
    fixture.admission(Some("second"), StatusCode::OK).await;
    fixture.source.set("");
    init(&fixture.router, json!({})).await;
    for token in [None, Some(""), Some("first"), Some("second")] {
        fixture.admission(token, StatusCode::UNAUTHORIZED).await;
    }
    let response = send(
        &fixture.router,
        request("GET", "/envs", None, Body::empty()),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "S0 keeps its tokenless reset behavior"
    );
}

#[tokio::test]
async fn handover_requires_token_installation_even_after_a_successful_tokenless_init() {
    let fixture = Fixture::handover().await;
    assert!(!fixture.daemon.initialized().await);
    for token in [None, Some(""), Some("uninstalled")] {
        fixture.admission(token, StatusCode::UNAUTHORIZED).await;
    }
    let response = send(
        &fixture.router,
        request("GET", "/health", None, Body::empty()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    error(
        send(
            &fixture.router,
            request("GET", "/envs", None, Body::empty()),
        )
        .await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    fixture.source.set("");
    init(&fixture.router, json!({})).await;
    assert!(fixture.daemon.initialized().await);
    for token in [None, Some("uninstalled")] {
        fixture.admission(token, StatusCode::UNAUTHORIZED).await;
    }
    assert!(fixture.tools.calls().is_empty());
    fixture.install("installed").await;
    fixture
        .admission(Some("uninstalled"), StatusCode::UNAUTHORIZED)
        .await;
    fixture.admission(Some("installed"), StatusCode::OK).await;
    assert_eq!(fixture.tools.calls().len(), 1);
}

#[tokio::test]
async fn service_and_unknown_sessions_remain_unavailable_while_s0_stays_usable() {
    let mut services = vec![Arc::new(CuaService::unavailable("isolated base fixture"))];
    for (attached, session) in [
        (true, Some(9)),
        (true, Some(0)),
        (false, Some(0)),
        (false, None),
    ] {
        services.push(Arc::new(CuaService::for_context(
            DesktopContext::for_windows(attached, session),
        )));
    }
    #[cfg(not(feature = "cua-desktop"))]
    services.push(Arc::new(CuaService::for_context(
        DesktopContext::DesktopSession,
    )));
    for (index, service) in services.into_iter().enumerate() {
        let router = app(Arc::new(
            daemon_with_tokens(Arc::new(TokenSource::default())).with_cua(service),
        ));
        error(
            send(&router, call(None, json!({"text": "no desktop"}))).await,
            StatusCode::UNAUTHORIZED,
        )
        .await;
        init(&router, json!({"accessToken": TOKEN})).await;
        for path in ["/cua/tools", "/cua/call"] {
            let method = if path.ends_with("call") {
                "POST"
            } else {
                "GET"
            };
            let body = Body::from(r#"{"name":"probe","arguments":{"text":"no desktop"}}"#);
            error(
                send(&router, request(method, path, Some(TOKEN), body)).await,
                StatusCode::SERVICE_UNAVAILABLE,
            )
            .await;
        }
        let response = send(
            &router,
            request("GET", "/cua/health", Some(TOKEN), Body::empty()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let health = document(response).await;
        assert_eq!(health["runtime_available"], false);
        assert!(!health["reason"].as_str().unwrap().is_empty());
        if index == 0 {
            assert!(health["reason"]
                .as_str()
                .unwrap()
                .contains("isolated base fixture"));
        }
        let response = send(
            &router,
            request("GET", "/cua/metrics", Some(TOKEN), Body::empty()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(text(response).await.contains("coorworker_envd_cua_ready 0"));
        let response = send(&router, request("GET", "/health", None, Body::empty())).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let response = send(&router, request("GET", "/envs", Some(TOKEN), Body::empty())).await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}
