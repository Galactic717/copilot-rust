use std::sync::Arc;
use std::time::Duration;

use futures_util::future::{BoxFuture, FutureExt};
use serde_json::{json, Value};

use super::errors::from_http;
use super::{http_error, join_url, sse_chunks, AdapterDeps, ChunkStream, Decoder, Out, ProviderAdapter, SseEvent};
use crate::net::Net;
use crate::types::{AppError, ChatRequest, ModelInfo, ProviderConfig, Role, StreamChunk};

struct Inner {
    cfg: ProviderConfig,
    key: Option<String>,
    net: Arc<Net>,
}

pub struct GeminiAdapter {
    inner: Arc<Inner>,
}

impl GeminiAdapter {
    pub fn new(d: AdapterDeps) -> Self {
        Self { inner: Arc::new(Inner { cfg: d.cfg, key: d.api_key, net: d.net }) }
    }
}

impl Inner {
    fn key(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        rb.header("x-goog-api-key", self.key.as_deref().unwrap_or(""))
    }

    async fn list(&self) -> Result<Vec<ModelInfo>, AppError> {
        let res = self
            .key(self.net.get(&join_url(&self.cfg.base_url, "models?pageSize=200"))?)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| crate::net::transport_error(&e))?;
        if !res.status().is_success() {
            return Err(http_error(res).await);
        }
        let j: Value = res.json().await.map_err(|e| crate::net::transport_error(&e))?;
        Ok(j.get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|m| m.get("supportedGenerationMethods").and_then(Value::as_array).is_some_and(|a| a.iter().any(|x| x == "generateContent")))
            .filter_map(|m| {
                let name = m.get("name")?.as_str()?;
                Some(ModelInfo {
                    id: name.trim_start_matches("models/").into(),
                    label: m.get("displayName").and_then(Value::as_str).map(String::from),
                    vision: if name.to_lowercase().contains("gemini") { Some(true) } else { None },
                })
            })
            .collect())
    }

    async fn post(&self, req: &ChatRequest) -> Result<reqwest::Response, AppError> {
        let system: Vec<&str> = req.messages.iter().filter(|m| m.role == Role::System).map(|m| m.content.as_str()).collect();
        let contents: Vec<Value> = req
            .messages
            .iter()
            .filter(|m| m.role != Role::System)
            .map(|m| {
                let mut parts: Vec<Value> = m.images.iter().map(|i| json!({ "inline_data": { "mime_type": i.mime, "data": i.base64 } })).collect();
                parts.push(json!({ "text": m.content }));
                json!({ "role": if m.role == Role::Assistant { "model" } else { "user" }, "parts": parts })
            })
            .collect();
        let mut body = json!({
            "contents": contents,
            "generationConfig": { "maxOutputTokens": req.max_tokens, "temperature": req.temperature },
        });
        if !system.is_empty() {
            body["systemInstruction"] = json!({ "parts": [{ "text": system.join("\n\n") }] });
        }
        let model: String = url_encode(&req.model);
        let url = join_url(&self.cfg.base_url, &format!("models/{model}:streamGenerateContent?alt=sse"));
        let res = self.key(self.net.post(&url)?).json(&body).send().await.map_err(|e| crate::net::transport_error(&e))?;
        if !res.status().is_success() {
            return Err(http_error(res).await);
        }
        Ok(res)
    }
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl ProviderAdapter for GeminiAdapter {
    fn list_models(&self) -> BoxFuture<'static, Result<Vec<ModelInfo>, AppError>> {
        let inner = self.inner.clone();
        async move { inner.list().await }.boxed()
    }

    fn stream(&self, req: ChatRequest) -> BoxFuture<'static, Result<ChunkStream, AppError>> {
        let inner = self.inner.clone();
        async move {
            let res = inner.post(&req).await?;
            Ok(sse_chunks(res, GeminiDecoder::default()))
        }
        .boxed()
    }
}

#[derive(Default)]
struct GeminiDecoder {
    usage: Option<(u64, u64)>,
}

impl Decoder for GeminiDecoder {
    fn on_event(&mut self, ev: &SseEvent, out: &mut Out) -> bool {
        let Ok(j) = serde_json::from_str::<Value>(&ev.data) else { return false };
        if j.get("error").is_some() {
            out.push_back(Err(from_http(500, &ev.data, None)));
            return true;
        }
        for p in j.pointer("/candidates/0/content/parts").and_then(Value::as_array).into_iter().flatten() {
            let thought = p.get("thought").and_then(Value::as_bool).unwrap_or(false);
            if let (Some(text), false) = (p.get("text").and_then(Value::as_str), thought) {
                out.push_back(Ok(StreamChunk::Text(text.into())));
            }
        }
        if let Some(u) = j.get("usageMetadata") {
            self.usage = Some((
                u.get("promptTokenCount").and_then(Value::as_u64).unwrap_or(0),
                u.get("candidatesTokenCount").and_then(Value::as_u64).unwrap_or(0),
            ));
        }
        false
    }

    fn on_end(&mut self, out: &mut Out) {
        if let Some((input, output)) = self.usage {
            out.push_back(Ok(StreamChunk::Usage { input, output }));
        }
    }
}
