use anyhow::Result;
use core_test_support::responses::{
    ev_completed, ev_response_created, mount_sse_once, mount_sse_sequence, sse,
};
use core_test_support::test_savfox::test_savfox;
use core_test_support::{responses, skip_if_no_network};
use pretty_assertions::assert_eq;
use savfox_core::features::Feature;
use wiremock::http::Method;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn chat_completions_sse(text: &str) -> String {
    format!(
        concat!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\n",
            "data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n",
            "data: [DONE]\n\n",
        ),
        text = text
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_fallback_switches_to_http_after_retries_exhausted() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let response_mock = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;

    let mut builder = test_savfox().with_config({
        let base_url = format!("{}/v1", server.uri());
        move |config| {
            config.model_provider.base_url = Some(base_url);
            config.model_provider.wire_api = savfox_core::WireApi::Responses;
            config.features.enable(Feature::ResponsesWebsockets);
            config.model_provider.stream_max_retries = Some(0);
            config.model_provider.request_max_retries = Some(0);
        }
    });
    let test = builder.build(&server).await?;

    test.submit_turn("hello").await?;

    let requests = server.received_requests().await.unwrap_or_default();
    let websocket_attempts = requests
        .iter()
        .filter(|req| req.method == Method::GET && req.url.path().ends_with("/responses"))
        .count();
    let http_attempts = requests
        .iter()
        .filter(|req| req.method == Method::POST && req.url.path().ends_with("/responses"))
        .count();

    assert_eq!(websocket_attempts, 1);
    assert_eq!(http_attempts, 1);
    assert_eq!(response_mock.requests().len(), 1);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_fallback_is_sticky_across_turns() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let response_mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
            sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
        ],
    )
    .await;

    let mut builder = test_savfox().with_config({
        let base_url = format!("{}/v1", server.uri());
        move |config| {
            config.model_provider.base_url = Some(base_url);
            config.model_provider.wire_api = savfox_core::WireApi::Responses;
            config.features.enable(Feature::ResponsesWebsockets);
            config.model_provider.stream_max_retries = Some(0);
            config.model_provider.request_max_retries = Some(0);
        }
    });
    let test = builder.build(&server).await?;

    test.submit_turn("first").await?;
    test.submit_turn("second").await?;

    let requests = server.received_requests().await.unwrap_or_default();
    let websocket_attempts = requests
        .iter()
        .filter(|req| req.method == Method::GET && req.url.path().ends_with("/responses"))
        .count();
    let http_attempts = requests
        .iter()
        .filter(|req| req.method == Method::POST && req.url.path().ends_with("/responses"))
        .count();

    assert_eq!(websocket_attempts, 1);
    assert_eq!(http_attempts, 2);
    assert_eq!(response_mock.requests().len(), 2);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_wire_codex_backend_tries_responses_before_chat() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/codex/responses"))
        .respond_with(
            ResponseTemplate::new(404)
                .insert_header("content-type", "application/json")
                .set_body_string(r#"{"detail":"Not Found"}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/codex/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(chat_completions_sse("ok")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut builder = test_savfox().with_config({
        let base_url = format!("{}/api/codex", server.uri());
        move |config| {
            config.model_provider.base_url = Some(base_url);
            config.model_provider.wire_api = savfox_core::WireApi::Chat;
            config.model_provider.supports_websockets = false;
            config.model_provider.stream_max_retries = Some(0);
            config.model_provider.request_max_retries = Some(0);
            config.features.disable(Feature::ResponsesWebsockets);
        }
    });
    let test = builder.build(&server).await?;

    test.submit_turn("hello").await?;

    let requests = server.received_requests().await.unwrap_or_default();
    let paths: Vec<String> = requests
        .iter()
        .filter(|req| req.method == Method::POST)
        .map(|req| req.url.path().to_string())
        .collect();

    assert_eq!(
        paths,
        vec![
            "/api/codex/responses".to_string(),
            "/api/codex/chat/completions".to_string(),
        ]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_wire_falls_back_to_chat_when_responses_endpoint_is_missing() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/codex/responses"))
        .respond_with(
            ResponseTemplate::new(404)
                .insert_header("content-type", "application/json")
                .set_body_string(r#"{"detail":"Not Found"}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/codex/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(chat_completions_sse("fallback")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut builder = test_savfox().with_config({
        let base_url = format!("{}/api/codex", server.uri());
        move |config| {
            config.model_provider.base_url = Some(base_url);
            config.model_provider.wire_api = savfox_core::WireApi::Responses;
            config.model_provider.supports_websockets = false;
            config.model_provider.stream_max_retries = Some(0);
            config.model_provider.request_max_retries = Some(0);
            config.features.disable(Feature::ResponsesWebsockets);
        }
    });
    let test = builder.build(&server).await?;

    test.submit_turn("hello").await?;

    let requests = server.received_requests().await.unwrap_or_default();
    let paths: Vec<String> = requests
        .iter()
        .filter(|req| req.method == Method::POST)
        .map(|req| req.url.path().to_string())
        .collect();

    assert_eq!(
        paths,
        vec![
            "/api/codex/responses".to_string(),
            "/api/codex/chat/completions".to_string(),
        ]
    );

    Ok(())
}
