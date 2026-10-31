pub mod anthropic;
pub mod errors;
pub mod gemini;
pub mod openai;
pub mod presets;
pub mod registry;
pub mod runner;

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use futures_util::stream::{BoxStream, Stream, StreamExt};

use crate::net::Net;
use crate::types::{AppError, ChatRequest, ModelInfo, ProviderConfig, ProviderProtocol, StreamChunk};

pub type ChunkStream = Pin<Box<dyn Stream<Item = Result<StreamChunk, AppError>> + Send>>;

pub trait ProviderAdapter: Send + Sync {
    fn list_models(&self) -> BoxFuture<'static, Result<Vec<ModelInfo>, AppError>>;
    /// Errors are AppError. I don't retry here — the runner owns retries so they never happen after the first token.
    fn stream(&self, req: ChatRequest) -> BoxFuture<'static, Result<ChunkStream, AppError>>;
}

pub struct AdapterDeps {
    pub cfg: ProviderConfig,
    pub api_key: Option<String>,
    pub net: Arc<Net>,
}

pub fn create_adapter(deps: AdapterDeps) -> Arc<dyn ProviderAdapter> {
    match deps.cfg.protocol {
        ProviderProtocol::Anthropic => Arc::new(anthropic::AnthropicAdapter::new(deps)),
        ProviderProtocol::Gemini => Arc::new(gemini::GeminiAdapter::new(deps)),
        ProviderProtocol::Openai => Arc::new(openai::OpenAiAdapter::new(deps)),
    }
}

pub fn join_url(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

/// Reads a non-2xx response into an AppError.
pub async fn http_error(res: reqwest::Response) -> AppError {
    let status = res.status().as_u16();
    let ra = res.headers().get("retry-after").and_then(|v| v.to_str().ok()).map(String::from);
    let body = res.text().await.unwrap_or_default();
    errors::from_http(status, &body, ra.as_deref())
}

#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Incremental SSE parser. Lines split on '\n' (ASCII), so a multi-byte char cut between chunks is decoded only once complete.
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=nl).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line);
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(SseEvent { event: self.event.take(), data: self.data.join("\n") });
                }
                self.event = None;
                self.data.clear();
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, val) = match line.find(':') {
                Some(i) => (&line[..i], line[i + 1..].strip_prefix(' ').unwrap_or(&line[i + 1..])),
                None => (&line[..], ""),
            };
            match field {
                "data" => self.data.push(val.to_string()),
                "event" => self.event = Some(val.to_string()),
                _ => {}
            }
        }
        out
    }

    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut out = self.push(b"\n");
        if !self.data.is_empty() {
            out.push(SseEvent { event: self.event.take(), data: self.data.join("\n") });
            self.data.clear();
        }
        out
    }
}

pub type Out = VecDeque<Result<StreamChunk, AppError>>;

/// Protocol-specific SSE event decoding.
pub trait Decoder: Send + 'static {
    /// Returns true when the stream is complete.
    fn on_event(&mut self, ev: &SseEvent, out: &mut Out) -> bool;
    fn on_end(&mut self, _out: &mut Out) {}
}

struct SseState<D> {
    body: BoxStream<'static, reqwest::Result<bytes::Bytes>>,
    parser: SseParser,
    queue: Out,
    decoder: D,
    done: bool,
}

pub fn sse_chunks<D: Decoder>(res: reqwest::Response, decoder: D) -> ChunkStream {
    let st = SseState { body: res.bytes_stream().boxed(), parser: SseParser::default(), queue: VecDeque::new(), decoder, done: false };
    Box::pin(futures_util::stream::unfold(st, |mut st| async move {
        loop {
            if let Some(item) = st.queue.pop_front() {
                if item.is_err() {
                    st.done = true;
                    st.queue.clear();
                }
                return Some((item, st));
            }
            if st.done {
                return None;
            }
            match st.body.next().await {
                Some(Ok(bytes)) => {
                    for ev in st.parser.push(&bytes) {
                        if st.decoder.on_event(&ev, &mut st.queue) {
                            st.decoder.on_end(&mut st.queue);
                            st.done = true;
                            break;
                        }
                    }
                }
                Some(Err(e)) => {
                    st.queue.push_back(Err(crate::net::transport_error(&e)));
                }
                None => {
                    for ev in st.parser.finish() {
                        if st.decoder.on_event(&ev, &mut st.queue) {
                            break;
                        }
                    }
                    st.decoder.on_end(&mut st.queue);
                    st.done = true;
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parser_handles_split_chunks_and_crlf() {
        let mut p = SseParser::default();
        let mut all = p.push("event: x\r\ndata: {\"a\":".as_bytes());
        assert!(all.is_empty());
        all.extend(p.push("1}\r\n\r\n: comment\ndata: Привіт".as_bytes()));
        all.extend(p.push(b"\n\n"));
        assert_eq!(all, vec![SseEvent { event: Some("x".into()), data: "{\"a\":1}".into() }, SseEvent { event: None, data: "Привіт".into() }]);
        let mut p = SseParser::default();
        let bytes = "data: Привіт\n\n".as_bytes();
        let mut got = p.push(&bytes[..8]);
        got.extend(p.push(&bytes[8..]));
        assert_eq!(got[0].data, "Привіт");
    }
}

#[cfg(test)]
mod tests_http;
