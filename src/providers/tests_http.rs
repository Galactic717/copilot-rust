//! Adapters against a local mock HTTP server: real reqwest + SSE decoding, no network.

use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{create_adapter, AdapterDeps};
use crate::net::Net;
use crate::types::*;

/// Serves `responses` in order (one per connection) and records each raw request.
async fn mock(responses: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    tokio::spawn(async move {
        for resp in responses {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let mut buf = vec![0u8; 65536];
            let mut req = Vec::new();
            // Read headers, then the body by Content-Length.
            loop {
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req);
                if let Some(h) = text.find("\r\n\r\n") {
                    let len = text[..h].lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                    if req.len() >= h + 4 + len {
                        break;
                    }
                }
            }
            s2.lock().unwrap().push(String::from_utf8_lossy(&req).into_owned());
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), seen)
}

fn sse(events: &[&str]) -> String {
    let body: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
    format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

fn cfg(preset: &str, protocol: ProviderProtocol, base: String) -> ProviderConfig {
    ProviderConfig { id: "t".into(), label: "T".into(), preset: preset.into(), protocol, base_url: base, has_key: true, allow_fallback: false, timeout_ms: 5000, llama: None }
}

fn req() -> ChatRequest {
    ChatRequest {
        model: "m".into(),
        messages: vec![
            ChatMessage { role: Role::System, content: "sys".into(), images: vec![] },
            ChatMessage { role: Role::User, content: "Привіт".into(), images: vec![ChatImage { mime: "image/png".into(), base64: "AAAA".into() }] },
        ],
        max_tokens: 100,
        temperature: 0.3,
    }
}

async fn collect(a: Arc<dyn super::ProviderAdapter>) -> (String, Option<(u64, u64)>) {
    let mut s = a.stream(req()).await.expect("stream");
    let (mut text, mut usage) = (String::new(), None);
    while let Some(c) = s.next().await {
        match c.expect("chunk") {
            StreamChunk::Text(t) => text.push_str(&t),
            StreamChunk::Usage { input, output } => usage = Some((input, output)),
        }
    }
    (text, usage)
}

#[tokio::test]
async fn openai_compatible_stream_with_key_and_usage() {
    let (base, seen) = mock(vec![sse(&[
        r#"{"choices":[{"delta":{"content":"При"}}]}"#,
        r#"{"choices":[{"delta":{"content":"віт"}}]}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":3}}"#,
        "[DONE]",
    ])])
    .await;
    let a = create_adapter(AdapterDeps { cfg: cfg("openrouter", ProviderProtocol::Openai, format!("{base}/api/v1")), api_key: Some("sk-or-v1-test".into()), net: Net::new() });
    assert_eq!(collect(a).await, ("Привіт".into(), Some((12, 3))));
    let r = seen.lock().unwrap()[0].clone();
    assert!(r.starts_with("POST /api/v1/chat/completions"));
    assert!(r.to_lowercase().contains("authorization: bearer sk-or-v1-test"));
    assert!(r.contains("\"image_url\"") && r.contains("data:image/png;base64,AAAA"));
    assert!(r.contains("\"max_tokens\":100"));
}

#[tokio::test]
async fn openai_drops_rejected_optional_param_and_retries() {
    let bad = r#"{"error":{"message":"Unsupported parameter: 'temperature' is not supported with this model."}}"#;
    let (base, seen) = mock(vec![
        format!("HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{bad}", bad.len()),
        sse(&[r#"{"choices":[{"delta":{"content":"ok"}}]}"#, "[DONE]"]),
    ])
    .await;
    let a = create_adapter(AdapterDeps { cfg: cfg("custom", ProviderProtocol::Openai, base), api_key: None, net: Net::new() });
    assert_eq!(collect(a).await.0, "ok");
    let reqs = seen.lock().unwrap().clone();
    assert!(reqs[0].contains("\"temperature\""));
    assert!(!reqs[1].contains("\"temperature\""));
}

#[tokio::test]
async fn anthropic_stream_headers_and_usage() {
    let (base, seen) = mock(vec![sse(&[
        r#"{"type":"message_start","message":{"usage":{"input_tokens":20}}}"#,
        r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}"#,
        r#"{"type":"message_stop"}"#,
    ])])
    .await;
    let a = create_adapter(AdapterDeps { cfg: cfg("anthropic", ProviderProtocol::Anthropic, base), api_key: Some("sk-ant-test".into()), net: Net::new() });
    assert_eq!(collect(a).await, ("Hi".into(), Some((20, 4))));
    let r = seen.lock().unwrap()[0].to_lowercase();
    assert!(r.starts_with("post /v1/messages"));
    assert!(r.contains("x-api-key: sk-ant-test") && r.contains("anthropic-version: 2023-06-01"));
    assert!(r.contains("\"system\":\"sys\""));
}

#[tokio::test]
async fn anthropic_refusal_is_an_error() {
    let (base, _) = mock(vec![sse(&[r#"{"type":"message_delta","delta":{"stop_reason":"refusal"},"usage":{"output_tokens":1}}"#])]).await;
    let a = create_adapter(AdapterDeps { cfg: cfg("anthropic", ProviderProtocol::Anthropic, base), api_key: Some("k".into()), net: Net::new() });
    let mut s = a.stream(req()).await.unwrap();
    assert_eq!(s.next().await.unwrap().unwrap_err().code, ErrorCode::Unsupported);
}

#[tokio::test]
async fn gemini_stream_skips_thoughts() {
    let (base, seen) = mock(vec![sse(&[
        r#"{"candidates":[{"content":{"parts":[{"text":"hmm","thought":true},{"text":"Answer"}]}}]}"#,
        r#"{"candidates":[{"content":{"parts":[{"text":" here"}]}}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":2}}"#,
    ])])
    .await;
    let a = create_adapter(AdapterDeps { cfg: cfg("gemini", ProviderProtocol::Gemini, base), api_key: Some("AIzaTest".into()), net: Net::new() });
    assert_eq!(collect(a).await, ("Answer here".into(), Some((7, 2))));
    let r = seen.lock().unwrap()[0].clone();
    assert!(r.starts_with("POST /models/m:streamGenerateContent?alt=sse"));
    assert!(r.to_lowercase().contains("x-goog-api-key: aizatest"));
}

#[tokio::test]
async fn http_errors_map_and_offline_blocks_redirect_to_internet() {
    let (base, _) = mock(vec!["HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".into()]).await;
    let a = create_adapter(AdapterDeps { cfg: cfg("openai", ProviderProtocol::Openai, base), api_key: Some("bad".into()), net: Net::new() });
    assert_eq!(a.stream(req()).await.err().unwrap().code, ErrorCode::Auth);

    let (base, _) = mock(vec!["HTTP/1.1 302 Found\r\nLocation: https://example.com/v1/chat/completions\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()]).await;
    let net = Net::new();
    net.set_offline(true);
    let a = create_adapter(AdapterDeps { cfg: cfg("custom", ProviderProtocol::Openai, base), api_key: None, net });
    assert_eq!(a.stream(req()).await.err().unwrap().code, ErrorCode::OfflineBlocked);
}

#[tokio::test]
async fn llama_models_report_vision_from_props() {
    let models = r#"{"data":[{"id":"qwen2.5-vl-7b.gguf"}]}"#;
    let props = r#"{"modalities":{"vision":true}}"#;
    let ok = |b: &str| format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}", b.len());
    let (base, _) = mock(vec![ok(models), ok(props)]).await;
    let a = create_adapter(AdapterDeps { cfg: cfg("llamacpp", ProviderProtocol::Openai, format!("{base}/v1")), api_key: None, net: Net::new() });
    let m = a.list_models().await.unwrap();
    assert_eq!(m[0].id, "qwen2.5-vl-7b.gguf");
    assert_eq!(m[0].vision, Some(true));
}
