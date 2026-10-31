use std::sync::LazyLock;

use regex::Regex;

use crate::i18n::t;
use crate::types::{AppError, ErrorCode};

pub fn message(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Auth => t("Перевірте API-ключ", "Check the API key"),
        ErrorCode::Quota => t("Закінчився баланс або ліміт у провайдера", "Provider balance or quota is exhausted"),
        ErrorCode::RateLimit => t("Забагато запитів — провайдер просить зачекати", "Too many requests — the provider asks to wait"),
        ErrorCode::Server => t("Збій на боці провайдера", "Provider-side failure"),
        ErrorCode::Timeout => t("Провайдер не відповів вчасно", "The provider did not respond in time"),
        ErrorCode::Network => t("Мережа недоступна", "Network unavailable"),
        ErrorCode::OfflineBlocked => t("Заблоковано офлайн-режимом", "Blocked by offline mode"),
        ErrorCode::BudgetBlocked => t("Досягнуто ліміту бюджету", "Budget limit reached"),
        ErrorCode::BadRequest => t("Провайдер відхилив запит", "The provider rejected the request"),
        ErrorCode::NotFound => t("Модель або адресу не знайдено", "Model or address not found"),
        ErrorCode::Cancelled => t("Скасовано", "Cancelled"),
        ErrorCode::Unsupported => t("Функція не підтримується моделлю", "Not supported by the model"),
        ErrorCode::Unknown => t("Невідома помилка", "Unknown error"),
    }
}

pub fn app_error(code: ErrorCode, detail: Option<&str>, status: Option<u16>) -> AppError {
    // Providers echo the rejected key in 401 bodies; detail reaches the UI and the screen may be shared, so I redact it.
    AppError {
        code,
        message: message(code).to_string(),
        detail: detail.map(|d| crate::redact::redact(d).chars().take(400).collect()),
        status,
        retry_after_ms: None,
    }
}

pub fn err(code: ErrorCode) -> AppError {
    app_error(code, None, None)
}

static QUOTA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"insufficient_quota|credit balance|billing|quota exceeded|insufficient credits").unwrap());

pub fn from_http(status: u16, body: &str, retry_after: Option<&str>) -> AppError {
    let lower = body.to_lowercase();
    let code = match status {
        401 | 403 => ErrorCode::Auth,
        402 => ErrorCode::Quota,
        _ if QUOTA.is_match(&lower) => ErrorCode::Quota,
        429 => ErrorCode::RateLimit,
        404 => ErrorCode::NotFound,
        408 => ErrorCode::Timeout,
        s if s >= 500 => ErrorCode::Server,
        s if s >= 400 => ErrorCode::BadRequest,
        _ => ErrorCode::Unknown,
    };
    let mut e = app_error(code, Some(&extract_message(body)), Some(status));
    e.retry_after_ms = retry_after.and_then(retry_after_ms);
    e
}

/// Retry-After is either delta-seconds or an HTTP date (RFC 9110 §10.2.3).
fn retry_after_ms(v: &str) -> Option<u64> {
    if let Ok(secs) = v.trim().parse::<f64>() {
        return Some((secs.max(0.0) * 1000.0) as u64);
    }
    let at = chrono::DateTime::parse_from_rfc2822(v.trim()).ok()?;
    Some((at.timestamp_millis() - crate::types::now_ms()).max(0) as u64)
}

fn extract_message(body: &str) -> String {
    let Ok(j) = serde_json::from_str::<serde_json::Value>(body) else { return body.to_string() };
    let e = if j.is_array() { j[0].get("error") } else { j.get("error") };
    e.and_then(|e| e.get("message").or(Some(e)))
        .or_else(|| j.get("message"))
        .map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string()))
        .unwrap_or_else(|| body.to_string())
}

pub fn is_retryable(e: &AppError) -> bool {
    matches!(e.code, ErrorCode::RateLimit | ErrorCode::Server | ErrorCode::Network)
}

/// Parameter-rejection 400s I can recover from by dropping an optional field.
pub fn rejects_param(e: &AppError, param: &str) -> bool {
    e.code == ErrorCode::BadRequest && e.detail.as_deref().unwrap_or("").to_lowercase().contains(param)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_mapping() {
        assert_eq!(from_http(401, "{}", None).code, ErrorCode::Auth);
        assert_eq!(from_http(400, r#"{"error":{"message":"insufficient_quota"}}"#, None).code, ErrorCode::Quota);
        let e = from_http(429, "{}", Some("2"));
        assert_eq!(e.code, ErrorCode::RateLimit);
        assert_eq!(e.retry_after_ms, Some(2000));
        assert_eq!(from_http(503, "x", None).code, ErrorCode::Server);
        assert_eq!(from_http(400, r#"{"error":{"message":"Unsupported parameter: temperature"}}"#, None).detail.as_deref(), Some("Unsupported parameter: temperature"));
    }
}
