use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use reqwest::{Client, Method, RequestBuilder, Url};

use crate::i18n::t;
use crate::types::{AppError, ErrorCode};

pub fn is_loopback_url(raw: &str) -> bool {
    Url::parse(raw).ok().and_then(|u| u.host_str().map(is_loopback_host)).unwrap_or(false)
}

fn is_loopback_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h == "localhost" || h == "::1" || h.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| ip.octets()[0] == 127)
}

pub struct UrlCheck {
    pub ok: bool,
    pub warning: Option<String>,
    pub error: Option<String>,
}

/// I allow HTTP outside loopback with a warning: LAN vLLM/LiteLLM/llama-server setups are a real use case.
pub fn check_endpoint(raw: &str) -> UrlCheck {
    let Ok(u) = Url::parse(raw) else {
        return UrlCheck { ok: false, warning: None, error: Some(t("Некоректна адреса", "Invalid address").into()) };
    };
    if u.scheme() != "https" && u.scheme() != "http" {
        return UrlCheck { ok: false, warning: None, error: Some(crate::tr!("Схема {}: заборонена", "Scheme {}: is not allowed", u.scheme())) };
    }
    if !u.username().is_empty() || u.password().is_some() {
        return UrlCheck {
            ok: false,
            warning: None,
            error: Some(t("Логін і пароль в адресі заборонені — використовуйте поле ключа", "User/password in the URL are not allowed — use the key field").into()),
        };
    }
    if u.scheme() == "http" && !is_loopback_url(raw) {
        return UrlCheck {
            ok: true,
            warning: Some(t("Незахищений HTTP поза localhost: ключ і дані йдуть відкритим текстом", "Plain HTTP outside localhost: key and data travel unencrypted").into()),
            error: None,
        };
    }
    UrlCheck { ok: true, warning: None, error: None }
}

/// Redirect hop rejected by policy; surfaced through reqwest's error chain.
#[derive(Debug)]
struct BlockedRedirect {
    offline: bool,
}

impl std::fmt::Display for BlockedRedirect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.offline { "redirect blocked by offline mode" } else { "redirect to a forbidden address" })
    }
}
impl std::error::Error for BlockedRedirect {}

/// Single choke point for every outgoing request.
pub struct Net {
    offline: Arc<AtomicBool>,
    client: Client,
}

impl Net {
    pub fn new() -> Arc<Net> {
        let offline = Arc::new(AtomicBool::new(false));
        let flag = offline.clone();
        // Every redirect hop is re-checked, so a loopback endpoint cannot bounce an offline request to the internet.
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.error("too many redirects");
            }
            let url = attempt.url().as_str().to_string();
            if !check_endpoint(&url).ok {
                return attempt.error(BlockedRedirect { offline: false });
            }
            if flag.load(Ordering::Relaxed) && !is_loopback_url(&url) {
                return attempt.error(BlockedRedirect { offline: true });
            }
            attempt.follow()
        });
        let client = Client::builder().redirect(policy).user_agent(concat!("Copilot/", env!("CARGO_PKG_VERSION"))).build().expect("http client");
        Arc::new(Net { offline, client })
    }

    pub fn set_offline(&self, v: bool) {
        self.offline.store(v, Ordering::Relaxed);
    }

    pub fn is_offline(&self) -> bool {
        self.offline.load(Ordering::Relaxed)
    }

    pub fn assert_allowed(&self, url: &str) -> Result<(), AppError> {
        let check = check_endpoint(url);
        if !check.ok {
            return Err(self.block(url, check.error.as_deref().unwrap_or("bad_url"), ErrorCode::BadRequest));
        }
        if self.is_offline() && !is_loopback_url(url) {
            return Err(self.block(url, "offline", ErrorCode::OfflineBlocked));
        }
        Ok(())
    }

    pub fn request(&self, method: Method, url: &str) -> Result<RequestBuilder, AppError> {
        self.assert_allowed(url)?;
        Ok(self.client.request(method, url))
    }

    pub fn get(&self, url: &str) -> Result<RequestBuilder, AppError> {
        self.request(Method::GET, url)
    }

    pub fn post(&self, url: &str) -> Result<RequestBuilder, AppError> {
        self.request(Method::POST, url)
    }

    fn block(&self, url: &str, reason: &str, code: ErrorCode) -> AppError {
        let safe = url.split('?').next().unwrap_or("").to_string();
        crate::db::diag("blocked", serde_json::json!({ "url": safe, "reason": reason }));
        AppError {
            code,
            message: if code == ErrorCode::OfflineBlocked {
                t("Заблоковано офлайн-режимом", "Blocked by offline mode").into()
            } else {
                t("Адресу заборонено політикою застосунку", "Address blocked by app policy").into()
            },
            detail: Some(safe),
            status: None,
            retry_after_ms: None,
        }
    }
}

/// Maps a transport error from reqwest into the app taxonomy.
pub fn transport_error(e: &reqwest::Error) -> AppError {
    let mut src: Option<&dyn std::error::Error> = Some(e);
    while let Some(s) = src {
        if let Some(b) = s.downcast_ref::<BlockedRedirect>() {
            return crate::providers::errors::app_error(if b.offline { ErrorCode::OfflineBlocked } else { ErrorCode::BadRequest }, Some(&b.to_string()), None);
        }
        src = s.source();
    }
    let mut detail = e.to_string();
    if let Some(s) = std::error::Error::source(e) {
        detail = format!("{detail}: {s}");
    }
    let code = if e.is_timeout() { ErrorCode::Timeout } else { ErrorCode::Network };
    crate::providers::errors::app_error(code, Some(&detail), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_and_endpoint_rules() {
        assert!(is_loopback_url("http://127.0.0.1:8080/v1"));
        assert!(is_loopback_url("http://localhost:8080"));
        assert!(is_loopback_url("http://[::1]:8080"));
        assert!(!is_loopback_url("https://api.openai.com/v1"));
        assert!(!check_endpoint("ftp://x").ok);
        assert!(!check_endpoint("https://user:pw@x.com").ok);
        assert!(check_endpoint("http://192.168.1.5:8000/v1").warning.is_some());
        let n = Net::new();
        n.set_offline(true);
        assert_eq!(n.assert_allowed("https://example.com").unwrap_err().code, ErrorCode::OfflineBlocked);
        assert!(n.assert_allowed("http://127.0.0.1:1/").is_ok());
    }
}
