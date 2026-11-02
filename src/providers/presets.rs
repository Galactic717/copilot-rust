//! One catalog for every service. Anything that speaks the OpenAI Chat Completions API (OpenRouter, Groq,
//! DeepSeek, llama.cpp…) is the same adapter with a different base URL and key, so adding a service is one entry here.

use crate::i18n::t;
use crate::types::ProviderProtocol;

pub struct PresetInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub protocol: ProviderProtocol,
    pub base_url: &'static str,
    pub needs_key: bool,
    pub key_url: Option<&'static str>,
    pub local: bool,
    note: fn() -> &'static str,
}

impl PresetInfo {
    pub fn note(&self) -> &'static str {
        (self.note)()
    }
    pub fn default_timeout_ms(&self) -> u64 {
        if self.local || self.id == "custom" {
            30_000
        } else {
            12_000
        }
    }
}

const fn p(id: &'static str, label: &'static str, protocol: ProviderProtocol, base_url: &'static str, needs_key: bool, key_url: Option<&'static str>, note: fn() -> &'static str) -> PresetInfo {
    PresetInfo { id, label, protocol, base_url, needs_key, key_url, local: false, note }
}

use ProviderProtocol::{Anthropic, Gemini, Openai};

pub static PRESETS: &[PresetInfo] = &[
    PresetInfo {
        id: "llamacpp",
        label: "llama.cpp",
        protocol: Openai,
        base_url: "http://127.0.0.1:8080/v1",
        needs_key: false,
        key_url: None,
        local: true,
        note: || t("llama-server на цьому комп'ютері: GGUF-модель, працює офлайн. Сервер можна запускати прямо із застосунку.", "llama-server on this computer: a GGUF model, works offline. The app can start the server for you."),
    },
    p("openai", "OpenAI", Openai, "https://api.openai.com/v1", true, Some("https://platform.openai.com/api-keys"), || t("Текст і зображення. Дані йдуть в OpenAI.", "Text and images. Data goes to OpenAI.")),
    p("anthropic", "Anthropic (Claude)", Anthropic, "https://api.anthropic.com", true, Some("https://platform.claude.com/settings/keys"), || t("Текст і зображення. Аудіо не надсилається — лише розпізнаний текст.", "Text and images. Audio is never sent — only recognized text.")),
    p("gemini", "Google Gemini", Gemini, "https://generativelanguage.googleapis.com/v1beta", true, Some("https://aistudio.google.com/apikey"), || t("Текст і зображення. Дані йдуть у Google.", "Text and images. Data goes to Google.")),
    p("openrouter", "OpenRouter", Openai, "https://openrouter.ai/api/v1", true, Some("https://openrouter.ai/settings/keys"), || t("Один ключ — сотні моделей різних компаній. Шлюз пересилає запит обраній моделі.", "One key — hundreds of models from many vendors. The gateway forwards the request to the chosen model.")),
    p("groq", "Groq", Openai, "https://api.groq.com/openai/v1", true, Some("https://console.groq.com/keys"), || t("Дуже швидкі відкриті моделі (Llama, Qwen…).", "Very fast open models (Llama, Qwen…).")),
    p("deepseek", "DeepSeek", Openai, "https://api.deepseek.com/v1", true, Some("https://platform.deepseek.com/api_keys"), || t("Моделі DeepSeek. Дані йдуть у DeepSeek.", "DeepSeek models. Data goes to DeepSeek.")),
    p("mistral", "Mistral AI", Openai, "https://api.mistral.ai/v1", true, Some("https://console.mistral.ai/api-keys"), || t("Моделі Mistral (ЄС).", "Mistral models (EU).")),
    p("xai", "xAI (Grok)", Openai, "https://api.x.ai/v1", true, Some("https://console.x.ai"), || t("Моделі Grok. Дані йдуть у xAI.", "Grok models. Data goes to xAI.")),
    p("together", "Together AI", Openai, "https://api.together.xyz/v1", true, Some("https://api.together.ai/settings/api-keys"), || t("Відкриті моделі в хмарі.", "Open models in the cloud.")),
    p("fireworks", "Fireworks AI", Openai, "https://api.fireworks.ai/inference/v1", true, Some("https://fireworks.ai/account/api-keys"), || t("Відкриті моделі в хмарі.", "Open models in the cloud.")),
    p("cerebras", "Cerebras", Openai, "https://api.cerebras.ai/v1", true, Some("https://cloud.cerebras.ai"), || t("Дуже швидкий інференс відкритих моделей.", "Very fast inference of open models.")),
    p("anymodel", "AnyModel.org", Openai, "https://anymodel.org/v1", true, Some("https://anymodel.org"), || t("Шлюз — окремий одержувач даних: він пересилає запит обраній моделі.", "A gateway is a separate data recipient: it forwards the request to the chosen model.")),
    p("custom", "Інший сервіс (OpenAI-сумісний)", Openai, "http://127.0.0.1:8000/v1", false, None, || t("Ollama, LM Studio, vLLM, LiteLLM, LocalAI або будь-який сумісний сервер — з ключем або без. Перевірте, кому йдуть дані.", "Ollama, LM Studio, vLLM, LiteLLM, LocalAI or any compatible server — with or without a key. Check who receives the data.")),
];

pub fn preset(id: &str) -> &'static PresetInfo {
    PRESETS.iter().find(|p| p.id == id).unwrap_or_else(|| PRESETS.iter().find(|p| p.id == "custom").unwrap())
}

/// Display label: the catalog label is Ukrainian for "custom"; this localizes it.
pub fn label(p: &PresetInfo) -> &'static str {
    if p.id == "custom" {
        t("Інший сервіс (OpenAI-сумісний)", "Other service (OpenAI-compatible)")
    } else {
        p.label
    }
}
