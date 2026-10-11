//! Integration tests for the `web_fetch` / `web_search` tools.
//!
//! All HTTP traffic is served by the loop-test-support fake server on
//! 127.0.0.1 — which is exactly what the default (strict) SSRF policy must
//! refuse to talk to. Tests that need to exercise the happy path opt into
//! `allowPrivateAddresses`, mirroring the documented intranet-mirror setting.

use loop_agent::harness::{create_web_fetch_tool, create_web_search_tool};
use loop_agent::types::AgentTool;
use loop_test_support::{FakeHttpServer, FakeResponse};
use serde_json::json;

/// `AgentTool` is not `Debug`, so `expect_err` is unavailable.
fn expect_creation_error(result: Result<AgentTool, String>, ctx: &str) -> String {
    match result {
        Ok(_) => panic!("{ctx}: expected an error"),
        Err(e) => e,
    }
}

fn lenient_fetch() -> AgentTool {
    create_web_fetch_tool(loop_agent::harness::WebFetchConfig {
        allow_private_addresses: true,
    })
    .expect("web_fetch tool")
}

async fn run(
    tool: &AgentTool,
    args: serde_json::Value,
) -> Result<loop_agent::types::AgentToolResult, String> {
    (tool.execute)("test".into(), args, None, None).await
}

fn text_of(result: &loop_agent::types::AgentToolResult) -> String {
    match result.content.first() {
        Some(loop_ai::ToolResultContent::Text(t)) => t.text.clone(),
        _ => String::new(),
    }
}

#[tokio::test]
async fn fetch_converts_html_to_text() {
    let server = FakeHttpServer::always(FakeResponse::new(
        200,
        "text/html; charset=utf-8",
        r#"<html><head><script>alert(1)</script></head>
           <body><h1>Hello</h1><p>World &amp; more</p><ul><li>a</li><li>b</li></ul></body></html>"#,
    ));
    let tool = lenient_fetch();
    let result = run(&tool, json!({"url": format!("{}/page", server.base_url())}))
        .await
        .expect("fetch ok");
    let text = text_of(&result);
    assert!(text.contains("Hello"));
    assert!(text.contains("World & more"));
    assert!(text.contains("- a"));
    assert!(!text.contains("alert"), "script bodies must be dropped");
    assert_eq!(result.details["status"], 200);
    assert_eq!(result.details["contentType"], "text/html");
}

#[tokio::test]
async fn fetch_passes_json_through() {
    let server = FakeHttpServer::always(FakeResponse::json(
        r#"{"models": ["m1", "m2"], "object": "list"}"#,
    ));
    let tool = lenient_fetch();
    let result = run(&tool, json!({"url": server.base_url()}))
        .await
        .expect("fetch ok");
    assert_eq!(
        text_of(&result),
        r#"{"models": ["m1", "m2"], "object": "list"}"#
    );
}

#[tokio::test]
async fn fetch_follows_redirects_and_reports_origin() {
    let server = FakeHttpServer::start(|req| match req.path.as_str() {
        "/start" => FakeResponse::new(302, "text/plain", "").with_header("Location", "/final"),
        "/final" => FakeResponse::new(200, "text/plain", "landed"),
        _ => FakeResponse::new(404, "text/plain", "nope"),
    });
    let tool = lenient_fetch();
    let result = run(
        &tool,
        json!({"url": format!("{}/start", server.base_url())}),
    )
    .await
    .expect("fetch ok");
    assert_eq!(text_of(&result), "landed");
    assert_eq!(
        result.details["url"],
        json!(format!("{}/final", server.base_url()))
    );
    assert_eq!(
        result.details["redirectedFrom"],
        json!(format!("{}/start", server.base_url()))
    );
    // Two requests: the redirect hop and the target.
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn fetch_reports_http_errors() {
    let server = FakeHttpServer::always(FakeResponse::new(404, "text/plain", "missing"));
    let tool = lenient_fetch();
    let err = run(&tool, json!({"url": server.base_url()}))
        .await
        .expect_err("404 must be an error");
    assert!(err.contains("404"), "unexpected error: {err}");
}

#[tokio::test]
async fn fetch_rejects_private_addresses_by_default() {
    let server = FakeHttpServer::always(FakeResponse::new(200, "text/plain", "secret"));
    let tool = create_web_fetch_tool(Default::default()).expect("web_fetch tool");
    let err = run(&tool, json!({"url": server.base_url()}))
        .await
        .expect_err("strict policy must refuse loopback");
    assert!(err.contains("refusing"), "unexpected error: {err}");
    // Nothing should have reached the server.
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn fetch_rejects_non_http_schemes_and_credentials() {
    let tool = lenient_fetch();
    for url in [
        "file:///etc/passwd",
        "ftp://example.com/pub",
        "https://user:pass@example.com/",
    ] {
        let err = match run(&tool, json!({"url": url})).await {
            Err(err) => err,
            Ok(_) => panic!("{url} must be rejected"),
        };
        assert!(!err.is_empty(), "{url} must produce an error message");
    }
}

#[tokio::test]
async fn fetch_truncates_large_bodies() {
    let body = "word ".repeat(40_000); // 200 KB > the 50 KB model cap
    let server = FakeHttpServer::always(FakeResponse::new(200, "text/plain", body));
    let tool = lenient_fetch();
    let result = run(&tool, json!({"url": server.base_url()}))
        .await
        .expect("fetch ok");
    assert_eq!(result.details["truncated"], true);
    assert!(text_of(&result).len() <= 50_000);
}

#[tokio::test]
async fn fetch_rejects_unsupported_content_types() {
    let server = FakeHttpServer::always(FakeResponse::new(200, "image/png", vec![1, 2, 3]));
    let tool = lenient_fetch();
    let err = run(&tool, json!({"url": server.base_url()}))
        .await
        .expect_err("images must be rejected");
    assert!(err.contains("image/png"), "unexpected error: {err}");
}

fn search_config(
    server: &FakeHttpServer,
    key_env: &str,
    provider: &str,
) -> loop_agent::harness::WebSearchConfig {
    loop_agent::harness::WebSearchConfig {
        provider: provider.into(),
        api_key_env: vec![key_env.into()],
        base_url: Some(server.base_url().to_string()),
        allow_private_addresses: true,
    }
}

#[tokio::test]
async fn search_tavily_backend() {
    std::env::set_var("LOOP_TEST_TAVILY_KEY", "tvly-test");
    let server = FakeHttpServer::always(FakeResponse::json(
        r#"{"results":[
            {"title": "Rust", "url": "https://rust-lang.org", "content": "Systems language"},
            {"title": "Docs", "url": "https://doc.rust-lang.org", "content": "Book"}
        ]}"#,
    ));
    let tool = create_web_search_tool(
        search_config(&server, "LOOP_TEST_TAVILY_KEY", "tavily"),
        None,
    )
    .expect("web_search tool");
    let result = run(&tool, json!({"query": "rust language", "max_results": 2}))
        .await
        .expect("search ok");
    let text = text_of(&result);
    assert!(text.contains("1. Rust"));
    assert!(text.contains("https://rust-lang.org"));
    assert!(text.contains("Systems language"));

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/search");
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer tvly-test")
    );
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["query"], "rust language");
    assert_eq!(body["max_results"], 2);
    assert_eq!(result.details["provider"], "tavily");
}

#[tokio::test]
async fn search_brave_backend() {
    std::env::set_var("LOOP_TEST_BRAVE_KEY", "brv-test");
    let server = FakeHttpServer::always(FakeResponse::json(
        r#"{"web":{"results":[
            {"title": "Rust", "url": "https://rust-lang.org", "description": "Systems language"}
        ]}}"#,
    ));
    let tool = create_web_search_tool(search_config(&server, "LOOP_TEST_BRAVE_KEY", "brave"), None)
        .expect("web_search tool");
    let result = run(&tool, json!({"query": "rust"}))
        .await
        .expect("search ok");
    assert!(text_of(&result).contains("https://rust-lang.org"));

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert!(requests[0].path.starts_with("/res/v1/web/search?"));
    assert!(requests[0].path.contains("q=rust"));
    assert!(requests[0].path.contains("count=5"));
    assert_eq!(requests[0].header("x-subscription-token"), Some("brv-test"));
}

#[tokio::test]
async fn search_requires_an_api_key() {
    std::env::remove_var("LOOP_TEST_MISSING_KEY");
    let err = expect_creation_error(
        create_web_search_tool(
            loop_agent::harness::WebSearchConfig {
                provider: "tavily".into(),
                api_key_env: vec!["LOOP_TEST_MISSING_KEY".into()],
                ..Default::default()
            },
            None,
        ),
        "missing key must fail tool creation",
    );
    assert!(err.contains("API key"), "unexpected error: {err}");
}

#[tokio::test]
async fn search_rejects_unknown_provider() {
    std::env::set_var("LOOP_TEST_ANY_KEY", "k");
    let err = expect_creation_error(
        create_web_search_tool(
            loop_agent::harness::WebSearchConfig {
                provider: "altavista".into(),
                api_key_env: vec!["LOOP_TEST_ANY_KEY".into()],
                ..Default::default()
            },
            None,
        ),
        "unknown provider must fail tool creation",
    );
    assert!(err.contains("altavista"), "unexpected error: {err}");
}

#[tokio::test]
async fn search_rejects_private_base_urls_by_default() {
    let server = FakeHttpServer::always(FakeResponse::json("{}"));
    std::env::set_var("LOOP_TEST_STRICT_KEY", "k");
    let err = expect_creation_error(
        create_web_search_tool(
            loop_agent::harness::WebSearchConfig {
                provider: "tavily".into(),
                api_key_env: vec!["LOOP_TEST_STRICT_KEY".into()],
                base_url: Some(server.base_url().to_string()),
                ..Default::default()
            },
            None,
        ),
        "strict policy must refuse loopback base URL",
    );
    assert!(err.contains("refusing"), "unexpected error: {err}");
}

#[tokio::test]
async fn search_clamps_max_results() {
    std::env::set_var("LOOP_TEST_CLAMP_KEY", "k");
    let server = FakeHttpServer::always(FakeResponse::json(
        r#"{"results":[{"title":"a","url":"https://a","content":"c"}]}"#,
    ));
    let tool = create_web_search_tool(
        search_config(&server, "LOOP_TEST_CLAMP_KEY", "tavily"),
        None,
    )
    .expect("web_search tool");
    run(&tool, json!({"query": "x", "max_results": 99}))
        .await
        .expect("search ok");
    let body: serde_json::Value = serde_json::from_slice(&server.requests()[0].body).unwrap();
    assert_eq!(body["max_results"], 10);
}

#[tokio::test]
async fn search_uses_the_stored_login_key() {
    std::env::remove_var("TAVILY_API_KEY");
    std::env::remove_var("LOOP_TEST_STORED_KEY");
    let server = FakeHttpServer::always(FakeResponse::json(
        r#"{"results":[{"title":"t","url":"https://t","content":"c"}]}"#,
    ));
    // No env keys configured at all — the key comes from the `/login` store.
    let mut config = search_config(&server, "LOOP_TEST_STORED_KEY", "tavily");
    config.api_key_env = vec![];
    let tool = create_web_search_tool(config, Some("tvly-stored")).expect("web_search tool");
    run(&tool, json!({"query": "q"})).await.expect("search ok");
    assert_eq!(
        server.requests()[0].header("authorization"),
        Some("Bearer tvly-stored")
    );
}

#[tokio::test]
async fn search_env_key_overrides_the_stored_key() {
    std::env::set_var("LOOP_TEST_OVERRIDE_KEY", "tvly-from-env");
    let server = FakeHttpServer::always(FakeResponse::json(
        r#"{"results":[{"title":"t","url":"https://t","content":"c"}]}"#,
    ));
    let tool = create_web_search_tool(
        search_config(&server, "LOOP_TEST_OVERRIDE_KEY", "tavily"),
        Some("tvly-stored"),
    )
    .expect("web_search tool");
    run(&tool, json!({"query": "q"})).await.expect("search ok");
    assert_eq!(
        server.requests()[0].header("authorization"),
        Some("Bearer tvly-from-env")
    );
}

#[tokio::test]
async fn key_verification_probes_and_reports_rejections() {
    use loop_agent::harness::verify_web_search_key;

    let ok = FakeHttpServer::always(FakeResponse::json(
        r#"{"results":[{"title":"t","url":"https://t","content":"c"}]}"#,
    ));
    let config = search_config(&ok, "LOOP_TEST_VERIFY_KEY", "tavily");
    verify_web_search_key(&config, "tvly-good")
        .await
        .expect("key accepted");

    let rejected = FakeHttpServer::always(FakeResponse::new(401, "application/json", "{}"));
    let config = search_config(&rejected, "LOOP_TEST_VERIFY_KEY", "tavily");
    let err = verify_web_search_key(&config, "tvly-bad")
        .await
        .expect_err("401 must fail verification");
    assert!(err.contains("rejected"), "unexpected error: {err}");
}
