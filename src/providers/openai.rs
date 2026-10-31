use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use futures_util::future::{BoxFuture, FutureExt};
use regex::Regex;
use serde_json::{json, Value};

use super::errors::{app_error, from_http, rejects_param};
use super::{http_error, join_url, sse_chunks, AdapterDeps, ChunkStream, Decoder, Out, ProviderAdapter, SseEvent};
use crate::net::Net;
use crate::types::{AppError, ChatMessage, ChatRequest, ErrorCode, ModelInfo, ProviderConfig, StreamChunk};

static NON_CHAT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)embed|whisper|tts|dall-e|moderation|transcribe|realtime|audio|image|search|computer-use|sora").unwrap());
const OPTIONAL_PARAMS: [&str; 4] = ["temperature", "stream_options", "max_completion_tokens", "max_tokens"];

struct Inner {
    cfg: ProviderConfig,
    key: Option<String>,
    net: Arc<Net>,
    dropped: Mutex<HashSet<&'static str>>,
}

/// OpenAI Chat Completions and every OpenAI-compatible server (OpenRouter, Groq, llama.cpp, vLLM, LiteLLM…).
pub struct OpenAiAdapter {
    inner: Arc<Inner>,
}

impl OpenAiAdapter {
    pub fn new(d: AdapterDeps) -> Self {
        Self { inner: Arc::new(Inner { cfg: d.cfg, key: d.api_key, net: d.net, dropped: Mutex::new(HashSet::new()) }) }
    }
}

impl Inner {
    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let rb = match &self.key {
            Some(k) => rb.bearer_auth(k),
            None => rb,
        };
        // OpenRouter attributes traffic by these optional headers.
        if self.cfg.preset == "openrouter" {
            rb.header("HTTP-Referer", "https://github.com/Magerko/copilot").header("X-Title", "Copilot")
        } else {
            rb
        }
    }

    fn body(&self, req: &ChatRequest) -> Value {
        let is_openai = self.cfg.preset == "openai";
        let mut b = json!({
            "model": req.model,
            "stream": true,
            "messages": req.messages.iter().map(to_openai_message).collect::<Vec<_>>(),
        });
        // OpenAI reasoning models only accept max_completion_tokens and the default temperature.
        b[if is_openai { "max_completion_tokens" } else { "max_tokens" }] = json!(req.max_tokens);
        if !is_openai {
            b["temperature"] = json!(req.temperature);
        }
        if matches!(self.cfg.preset.as_str(), "openai" | "anymodel" | "openrouter" | "groq" | "deepseek" | "together" | "fireworks" | "cerebras" | "llamacpp") {
            b["stream_options"] = json!({ "include_usage": true });
        }
        for p in self.dropped.lock().unwrap().iter() {
            b.as_object_mut().unwrap().remove(*p);
        }
        b
    }

    /// Some models/servers reject optional fields (temperature on reasoning models, stream_options on old servers), so I drop and retry once per field.
    async fn post(&self, req: &ChatRequest) -> Result<reqwest::Response, AppError> {
        let url = join_url(&self.cfg.base_url, "chat/completions");
        for _ in 0..=OPTIONAL_PARAMS.len() {
            let body = self.body(req);
            let res = self.auth(self.net.post(&url)?).json(&body).send().await.map_err(|e| crate::net::transport_error(&e))?;
            if res.status().is_success() {
                return Ok(res);
            }
            let err = http_error(res).await;
            let culprit = OPTIONAL_PARAMS.iter().find(|p| body.get(**p).is_some() && rejects_param(&err, p));
            match culprit {
                Some(p) => {
                    self.dropped.lock().unwrap().insert(p);
                }
                None => return Err(err),
            }
        }
        Err(app_error(ErrorCode::BadRequest, None, None))
    }

    async fn list(&self) -> Result<Vec<ModelInfo>, AppError> {
        let res = self
            .auth(self.net.get(&join_url(&self.cfg.base_url, "models"))?)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| crate::net::transport_error(&e))?;
        if !res.status().is_success() {
            return Err(http_error(res).await);
        }
        let j: Value = res.json().await.map_err(|e| crate::net::transport_error(&e))?;
        let items = j.get("data").or_else(|| j.get("models")).and_then(Value::as_array).cloned().unwrap_or_default();
        let mut ids: Vec<String> = items
            .iter()
            .filter_map(|m| m.get("id").or_else(|| m.get("name")).and_then(Value::as_str).map(String::from))
            .filter(|id| self.cfg.preset != "openai" || !NON_CHAT.is_match(id))
            .collect();
        ids.sort();
        ids.dedup();
        let vision = if self.cfg.preset == "llamacpp" { self.llama_vision().await } else { None };
        Ok(ids.into_iter().map(|id| ModelInfo { id, label: None, vision }).collect())
    }

    /// llama-server reports what the loaded model can take: `GET /props` → `modalities.vision`.
    async fn llama_vision(&self) -> Option<bool> {
        let root = self.cfg.base_url.trim_end_matches('/').trim_end_matches("/v1");
        let res = self.net.get(&join_url(root, "props")).ok()?.timeout(Duration::from_secs(5)).send().await.ok()?;
        let j: Value = res.json().await.ok()?;
        j.pointer("/modalities/vision").and_then(Value::as_bool)
    }
}

impl ProviderAdapter for OpenAiAdapter {
    fn list_models(&self) -> BoxFuture<'static, Result<Vec<ModelInfo>, AppError>> {
        let inner = self.inner.clone();
        async move { inner.list().await }.boxed()
    }

    fn stream(&self, req: ChatRequest) -> BoxFuture<'static, Result<ChunkStream, AppError>> {
        let inner = self.inner.clone();
        async move {
            let res = inner.post(&req).await?;
            Ok(sse_chunks(res, OpenAiDecoder))
        }
        .boxed()
    }
}

struct OpenAiDecoder;

impl Decoder for OpenAiDecoder {
    fn on_event(&mut self, ev: &SseEvent, out: &mut Out) -> bool {
        if ev.data == "[DONE]" {
            return true;
        }
        let Ok(j) = serde_json::from_str::<Value>(&ev.data) else { return false };
        if j.get("error").is_some_and(|e| !e.is_null()) {
            out.push_back(Err(from_http(500, &ev.data, None)));
            return true;
        }
        if let Some(text) = j.pointer("/choices/0/delta/content").and_then(Value::as_str) {
            if !text.is_empty() {
                out.push_back(Ok(StreamChunk::Text(text.to_string())));
            }
        }
        if let Some(u) = j.get("usage").filter(|u| u.is_object()) {
            let input = u.get("prompt_tokens").and_then(Value::as_u64);
            let output = u.get("completion_tokens").and_then(Value::as_u64);
            if input.is_some() || output.is_some() {
                out.push_back(Ok(StreamChunk::Usage { input: input.unwrap_or(0), output: output.unwrap_or(0) }));
            }
        }
        false
    }
}

fn to_openai_message(m: &ChatMessage) -> Value {
    if m.images.is_empty() {
        return json!({ "role": m.role.as_str(), "content": m.content });
    }
    let mut parts = vec![json!({ "type": "text", "text": m.content })];
    for img in &m.images {
        parts.push(json!({ "type": "image_url", "image_url": { "url": format!("data:{};base64,{}", img.mime, img.base64) } }));
    }
    json!({ "role": m.role.as_str(), "content": parts })
}
