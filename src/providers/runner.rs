use std::time::Duration;

use futures_util::StreamExt;
use tokio::time::{sleep_until, Instant};
use tokio_util::sync::CancellationToken;

use super::errors::{app_error, err, is_retryable};
use super::ProviderAdapter;
use crate::types::{AppError, ChatRequest, ErrorCode, StreamChunk, Usage};

pub enum RunEvent<'a> {
    FirstToken(u64),
    Text(&'a str),
}

#[derive(Debug)]
pub struct RunResult {
    pub text: String,
    pub first_token_ms: Option<u64>,
    pub latency_ms: u64,
    pub usage: Usage,
}

#[derive(Clone, Copy)]
pub struct RunOptions {
    pub timeout_ms: u64,
    pub retries: u32,
    pub base_delay_ms: u64,
    pub idle_ms: Option<u64>,
}

impl RunOptions {
    pub fn new(timeout_ms: u64) -> Self {
        Self { timeout_ms, retries: 2, base_delay_ms: 600, idle_ms: None }
    }
    pub fn retries(mut self, n: u32) -> Self {
        self.retries = n;
        self
    }
}

/// I stream one request with bounded retries. I retry only before the first token:
/// replaying after partial output would show the user two different answers.
pub async fn run_stream(
    adapter: &dyn ProviderAdapter,
    req: &ChatRequest,
    cancel: &CancellationToken,
    mut cb: impl FnMut(RunEvent<'_>),
    opts: RunOptions,
) -> Result<RunResult, AppError> {
    let started = Instant::now();
    let timeout = Duration::from_millis(opts.timeout_ms);
    // After the first token I turn the deadline into a stall timer: a stream that goes silent must not hold its slot forever.
    let idle = Duration::from_millis(opts.idle_ms.unwrap_or(opts.timeout_ms.max(20_000)));
    let mut last_err = err(ErrorCode::Unknown);

    for attempt in 0..=opts.retries {
        if cancel.is_cancelled() {
            return Err(err(ErrorCode::Cancelled));
        }
        let mut deadline = Instant::now() + timeout;
        let mut text = String::new();
        let mut first: Option<u64> = None;
        let mut usage: Option<Usage> = None;
        let mut filter = ThinkFilter::default();

        let outcome: Result<(), AppError> = 'attempt: {
            let mut stream = tokio::select! {
                r = adapter.stream(req.clone()) => match r { Ok(s) => s, Err(e) => break 'attempt Err(e) },
                _ = cancel.cancelled() => break 'attempt Err(err(ErrorCode::Cancelled)),
                _ = sleep_until(deadline) => break 'attempt Err(timeout_err(first, opts.timeout_ms, idle)),
            };
            loop {
                let next = tokio::select! {
                    n = stream.next() => n,
                    _ = cancel.cancelled() => break 'attempt Err(err(ErrorCode::Cancelled)),
                    _ = sleep_until(deadline) => break 'attempt Err(timeout_err(first, opts.timeout_ms, idle)),
                };
                match next {
                    None => break 'attempt Ok(()),
                    Some(Err(e)) => break 'attempt Err(e),
                    Some(Ok(StreamChunk::Usage { input, output })) => {
                        if first.is_some() {
                            deadline = Instant::now() + idle;
                        }
                        usage = Some(Usage { input_tokens: input, output_tokens: output, exact: true });
                    }
                    Some(Ok(StreamChunk::Text(t))) => {
                        if first.is_some() {
                            deadline = Instant::now() + idle;
                        }
                        let visible = filter.push(&t);
                        if visible.is_empty() {
                            continue;
                        }
                        if first.is_none() {
                            let ms = started.elapsed().as_millis() as u64;
                            first = Some(ms);
                            deadline = Instant::now() + idle;
                            cb(RunEvent::FirstToken(ms));
                        }
                        text.push_str(&visible);
                        cb(RunEvent::Text(&visible));
                    }
                }
            }
        };

        match outcome {
            Ok(()) => {
                let usage = usage.unwrap_or(Usage { input_tokens: estimate_tokens(req), output_tokens: (text.chars().count() as f64 / 3.5).ceil() as u64, exact: false });
                return Ok(RunResult { text, first_token_ms: first, latency_ms: started.elapsed().as_millis() as u64, usage });
            }
            Err(e) => {
                if cancel.is_cancelled() || e.code == ErrorCode::Cancelled {
                    return Err(err(ErrorCode::Cancelled));
                }
                if first.is_some() {
                    let prefix = crate::i18n::t("обірвано після початку відповіді", "cut off after the answer started");
                    let mut e2 = e.clone();
                    e2.detail = Some(format!("{prefix}: {}", e.detail.unwrap_or_default()));
                    return Err(e2);
                }
                if !is_retryable(&e) || attempt == opts.retries {
                    return Err(e);
                }
                let wait = e.retry_after_ms.unwrap_or(opts.base_delay_ms * 2u64.pow(attempt)).min(5000);
                last_err = e;
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(wait)) => {}
                    _ = cancel.cancelled() => return Err(err(ErrorCode::Cancelled)),
                }
            }
        }
    }
    Err(last_err)
}

fn timeout_err(first: Option<u64>, timeout_ms: u64, idle: Duration) -> AppError {
    let detail = if first.is_none() {
        crate::tr!("немає відповіді за {} мс", "no response within {} ms", timeout_ms)
    } else {
        crate::tr!("відповідь завмерла на {} мс", "the answer stalled for {} ms", idle.as_millis())
    };
    app_error(ErrorCode::Timeout, Some(&detail), None)
}

pub fn estimate_tokens(req: &ChatRequest) -> u64 {
    (req.messages.iter().map(|m| m.content.chars().count()).sum::<usize>() as f64 / 3.5).ceil() as u64
}

/// Local reasoning models (qwen3, DeepSeek-R1 via llama.cpp…) may inline <think>…</think>; I never show it as an answer.
#[derive(Default)]
pub struct ThinkFilter {
    in_think: bool,
    pending: String,
}

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

impl ThinkFilter {
    pub fn push(&mut self, chunk: &str) -> String {
        let mut s = std::mem::take(&mut self.pending) + chunk;
        let mut out = String::new();
        while !s.is_empty() {
            if self.in_think {
                match s.find(CLOSE) {
                    None => {
                        self.pending = tail_prefix(&s, CLOSE).to_string();
                        return out;
                    }
                    Some(end) => {
                        s = s[end + CLOSE.len()..].trim_start().to_string();
                        self.in_think = false;
                    }
                }
            } else {
                match s.find(OPEN) {
                    None => {
                        let keep = tail_prefix(&s, OPEN).len();
                        out.push_str(&s[..s.len() - keep]);
                        self.pending = s[s.len() - keep..].to_string();
                        return out;
                    }
                    Some(start) => {
                        out.push_str(&s[..start]);
                        s = s[start + OPEN.len()..].to_string();
                        self.in_think = true;
                    }
                }
            }
        }
        out
    }
}

/// Longest suffix of `s` that is a proper prefix of `tag` (a tag may be split across chunks).
fn tail_prefix<'a>(s: &'a str, tag: &str) -> &'a str {
    let max = (tag.len() - 1).min(s.len());
    for n in (1..=max).rev() {
        if s.is_char_boundary(s.len() - n) && tag.starts_with(&s[s.len() - n..]) {
            return &s[s.len() - n..];
        }
    }
    ""
}

#[cfg(test)]
pub mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use futures_util::future::{BoxFuture, FutureExt};

    use super::*;
    use crate::providers::errors::from_http;
    use crate::providers::ChunkStream;
    use crate::types::{ChatMessage, ModelInfo, Role};

    pub fn req() -> ChatRequest {
        ChatRequest { model: "m".into(), messages: vec![ChatMessage { role: Role::User, content: "q".into(), images: vec![] }], max_tokens: 10, temperature: 0.0 }
    }

    struct Scripted<F: Fn(usize) -> Result<ChunkStream, AppError> + Send + Sync> {
        calls: AtomicUsize,
        f: F,
    }

    impl<F: Fn(usize) -> Result<ChunkStream, AppError> + Send + Sync> ProviderAdapter for Scripted<F> {
        fn list_models(&self) -> BoxFuture<'static, Result<Vec<ModelInfo>, AppError>> {
            async { Ok(vec![]) }.boxed()
        }
        fn stream(&self, _req: ChatRequest) -> BoxFuture<'static, Result<ChunkStream, AppError>> {
            let r = (self.f)(self.calls.fetch_add(1, Ordering::SeqCst));
            async move { r }.boxed()
        }
    }

    fn text_stream(parts: &[&str]) -> ChunkStream {
        let items: Vec<Result<StreamChunk, AppError>> = parts.iter().map(|p| Ok(StreamChunk::Text(p.to_string()))).collect();
        Box::pin(futures_util::stream::iter(items))
    }

    #[tokio::test(start_paused = true)]
    async fn waits_retry_after_before_retrying_429() {
        let a = Scripted { calls: AtomicUsize::new(0), f: |n| if n == 0 { Err(from_http(429, "{}", Some("2"))) } else { Ok(text_stream(&["ok"])) } };
        let t0 = Instant::now();
        let r = run_stream(&a, &req(), &CancellationToken::new(), |_| {}, RunOptions::new(1000)).await.unwrap();
        assert_eq!(r.text, "ok");
        assert_eq!(t0.elapsed().as_millis() / 100, 20, "slept the Retry-After 2 s");
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_stream_after_first_token_times_out() {
        let a = Scripted {
            calls: AtomicUsize::new(0),
            f: |_| {
                let s = futures_util::stream::iter(vec![Ok(StreamChunk::Text("Початок".into()))]).chain(futures_util::stream::pending());
                Ok(Box::pin(s) as ChunkStream)
            },
        };
        let opts = RunOptions { idle_ms: Some(50), ..RunOptions::new(1000) };
        let e = run_stream(&a, &req(), &CancellationToken::new(), |_| {}, opts).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::Timeout);
    }

    #[tokio::test]
    async fn no_retry_after_first_token_and_cancel_is_cancelled() {
        let a = Arc::new(Scripted {
            calls: AtomicUsize::new(0),
            f: |_| Ok(Box::pin(futures_util::stream::iter(vec![Ok(StreamChunk::Text("a".into())), Err(from_http(503, "x", None))])) as ChunkStream),
        });
        let e = run_stream(a.as_ref(), &req(), &CancellationToken::new(), |_| {}, RunOptions::new(1000)).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::Server);
        assert_eq!(a.calls.load(Ordering::SeqCst), 1);
        let c = CancellationToken::new();
        c.cancel();
        assert_eq!(run_stream(a.as_ref(), &req(), &c, |_| {}, RunOptions::new(1000)).await.unwrap_err().code, ErrorCode::Cancelled);
    }

    #[test]
    fn think_filter_hides_reasoning_across_chunks() {
        let mut f = ThinkFilter::default();
        let mut out = String::new();
        for c in ["<thi", "nk>hidden", " stuff</th", "ink>  Відповідь", " <", "b>ok"] {
            out += &f.push(c);
        }
        assert_eq!(out, "Відповідь <b>ok");
    }
}
