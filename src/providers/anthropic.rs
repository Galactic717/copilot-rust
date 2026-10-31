use std::sync::{Arc, LazyLock};
use std::time::Duration;

use futures_util::future::{BoxFuture, FutureExt};
use regex::Regex;
use serde_json::{json, Value};

use super::errors::{app_error, from_http};
use super::{http_error, join_url, sse_chunks, AdapterDeps, ChunkStream, Decoder, Out, ProviderAdapter, SseEvent};
use crate::i18n::t;
use crate::net::Net;
use crate::types::{AppError, ChatRequest, ErrorCode, ModelInfo, ProviderConfig, Role, StreamChunk};

// Models that accept the server-side refusal fallback (`fallbacks: "default"`).
static FALLBACK_MODELS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^claude-(fable-5-1|opus-5-5|opus-5|sonnet-5-5)$").unwrap());
// Effort is rejected by Haiku 4.5 / Sonnet 4.5 and older; those simply run without it.
static NO_EFFORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"claude-(3|haiku-4-5|sonnet-4-5|opus-4-1|opus-4-0|sonnet-4-0)").unwrap());

struct Inner {
    cfg: ProviderConfig,
    key: Option<String>,
    net: Arc<Net>,
}

pub struct AnthropicAdapter {
    inner: Arc<Inner>,
}

impl AnthropicAdapter {
    pub fn new(d: AdapterDeps) -> Self {
        Self { inner: Arc::new(Inner { cfg: d.cfg, key: d.api_key, net: d.net }) }
    }
}

impl Inner {
    fn root(&self) -> String {
        self.cfg.base_url.trim_end_matches('/').trim_end_matches("/v1").to_string()
    }

    fn headers(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        rb.header("x-api-key", self.key.as_deref().unwrap_or("missing")).header("anthropic-version", "2023-06-01")
    }

    async fn list(&self) -> Result<Vec<ModelInfo>, AppError> {
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..10 {
            let mut url = join_url(&self.root(), "v1/models?limit=100");
            if let Some(a) = &after {
                url.push_str(&format!("&after_id={a}"));
            }
            let res = self.headers(self.net.get(&url)?).timeout(Duration::from_secs(15)).send().await.map_err(|e| crate::net::transport_error(&e))?;
            if !res.status().is_success() {
                return Err(http_error(res).await);
            }
            let j: Value = res.json().await.map_err(|e| crate::net::transport_error(&e))?;
            for m in j.get("data").and_then(Value::as_array).into_iter().flatten() {
                if let Some(id) = m.get("id").and_then(Value::as_str) {
                    out.push(ModelInfo { id: id.into(), label: m.get("display_name").and_then(Value::as_str).map(String::from), vision: Some(true) });
                }
            }
            if j.get("has_more").and_then(Value::as_bool) != Some(true) {
                break;
            }
            after = j.get("last_id").and_then(Value::as_str).map(String::from);
            if after.is_none() {
                break;
            }
        }
        Ok(out)
    }

    async fn post(&self, req: &ChatRequest) -> Result<reqwest::Response, AppError> {
        let system: Vec<&str> = req.messages.iter().filter(|m| m.role == Role::System).map(|m| m.content.as_str()).collect();
        let messages: Vec<Value> = req
            .messages
            .iter()
            .filter(|m| m.role != Role::System)
            .map(|m| {
                if m.images.is_empty() {
                    json!({ "role": m.role.as_str(), "content": m.content })
                } else {
                    let mut parts: Vec<Value> = m
                        .images
                        .iter()
                        .map(|img| json!({ "type": "image", "source": { "type": "base64", "media_type": img.mime, "data": img.base64 } }))
                        .collect();
                    parts.push(json!({ "type": "text", "text": m.content }));
                    json!({ "role": m.role.as_str(), "content": parts })
                }
            })
            .collect();
        let mut body = json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "system": system.join("\n\n"),
            "messages": messages,
            "stream": true,
        });
        // Quick answers are latency-critical, so I keep thinking short with low effort on models where it can't be disabled.
        if !NO_EFFORT.is_match(&req.model) {
            body["output_config"] = json!({ "effort": if req.max_tokens <= 400 { "low" } else { "medium" } });
        }
        let mut rb = self.headers(self.net.post(&join_url(&self.root(), "v1/messages"))?);
        if FALLBACK_MODELS.is_match(&req.model) {
            rb = rb.header("anthropic-beta", "server-side-fallback-2026-07-01");
            body["fallbacks"] = json!("default");
        }
        let res = rb.json(&body).send().await.map_err(|e| crate::net::transport_error(&e))?;
        if !res.status().is_success() {
            return Err(http_error(res).await);
        }
        Ok(res)
    }
}

impl ProviderAdapter for AnthropicAdapter {
    fn list_models(&self) -> BoxFuture<'static, Result<Vec<ModelInfo>, AppError>> {
        let inner = self.inner.clone();
        async move { inner.list().await }.boxed()
    }

    fn stream(&self, req: ChatRequest) -> BoxFuture<'static, Result<ChunkStream, AppError>> {
        let inner = self.inner.clone();
        async move {
            let res = inner.post(&req).await?;
            Ok(sse_chunks(res, AnthropicDecoder::default()))
        }
        .boxed()
    }
}

#[derive(Default)]
struct AnthropicDecoder {
    input: u64,
    output: u64,
}

impl Decoder for AnthropicDecoder {
    fn on_event(&mut self, ev: &SseEvent, out: &mut Out) -> bool {
        let Ok(j) = serde_json::from_str::<Value>(&ev.data) else { return false };
        match j.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => self.input = j.pointer("/message/usage/input_tokens").and_then(Value::as_u64).unwrap_or(0),
            "content_block_delta" => {
                if j.pointer("/delta/type").and_then(Value::as_str) == Some("text_delta") {
                    if let Some(text) = j.pointer("/delta/text").and_then(Value::as_str) {
                        out.push_back(Ok(StreamChunk::Text(text.into())));
                    }
                }
            }
            "message_delta" => {
                self.output = j.pointer("/usage/output_tokens").and_then(Value::as_u64).unwrap_or(self.output);
                if j.pointer("/delta/stop_reason").and_then(Value::as_str) == Some("refusal") {
                    out.push_back(Err(app_error(ErrorCode::Unsupported, Some(t("Модель відмовилася відповідати (refusal)", "The model refused to answer (refusal)")), None)));
                    return true;
                }
            }
            "message_stop" => return true,
            "error" => {
                let status = match j.pointer("/error/type").and_then(Value::as_str) {
                    Some("overloaded_error") => 529,
                    Some("rate_limit_error") => 429,
                    Some("authentication_error") => 401,
                    Some("invalid_request_error") => 400,
                    _ => 500,
                };
                out.push_back(Err(from_http(status, &ev.data, None)));
                return true;
            }
            _ => {}
        }
        false
    }

    fn on_end(&mut self, out: &mut Out) {
        out.push_back(Ok(StreamChunk::Usage { input: self.input, output: self.output }));
    }
}
