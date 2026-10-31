use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use tokio_util::sync::CancellationToken;

use super::errors::app_error;
use super::runner::{run_stream, RunOptions};
use super::{create_adapter, AdapterDeps, ProviderAdapter};
use crate::i18n::t;
use crate::net::{is_loopback_url, Net};
use crate::secrets::SecretStore;
use crate::settings::SettingsStore;
use crate::types::*;

/// A loopback proxy may still forward "…-cloud"/"…:cloud" models to a remote service, so the URL alone does not make them local.
pub fn is_local_target(base_url: &str, model: &str) -> bool {
    let m = model.to_lowercase();
    is_loopback_url(base_url) && !m.ends_with(":cloud") && !m.ends_with("-cloud")
}

#[derive(Clone)]
pub struct ResolvedTarget {
    pub cfg: ProviderConfig,
    pub model: String,
    pub adapter: Arc<dyn ProviderAdapter>,
    pub is_local: bool,
}

#[derive(Debug, Clone)]
pub struct TextCheck {
    pub ok: bool,
    pub ms: Option<u64>,
    pub sample: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct VisionCheck {
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProviderTestReport {
    pub models: Result<(u64, usize), AppError>,
    pub text: Option<TextCheck>,
    pub vision: Option<VisionCheck>,
}

const MODEL_TTL: Duration = Duration::from_secs(600);

static RED_PNG: LazyLock<String> = LazyLock::new(|| {
    let img = image::RgbaImage::from_pixel(32, 32, image::Rgba([220, 20, 20, 255]));
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).expect("png");
    base64::engine::general_purpose::STANDARD.encode(buf.into_inner())
});

pub struct ProviderRegistry {
    store: Arc<SettingsStore>,
    secrets: Arc<SecretStore>,
    net: Arc<Net>,
    adapters: Mutex<HashMap<String, Arc<dyn ProviderAdapter>>>,
    model_cache: Mutex<HashMap<String, (Instant, Vec<ModelInfo>)>>,
    pub vision_ok: Mutex<HashMap<String, bool>>,
    /// Last real outcome per provider/model (test or live answer): the LLM status is green only after one succeeded.
    pub text_ok: Mutex<HashMap<String, bool>>,
}

impl ProviderRegistry {
    pub fn new(store: Arc<SettingsStore>, secrets: Arc<SecretStore>, net: Arc<Net>) -> Self {
        Self {
            store,
            secrets,
            net,
            adapters: Mutex::new(HashMap::new()),
            model_cache: Mutex::new(HashMap::new()),
            vision_ok: Mutex::new(HashMap::new()),
            text_ok: Mutex::new(HashMap::new()),
        }
    }

    pub fn list(&self) -> Vec<ProviderConfig> {
        self.store.providers().into_iter().map(|mut p| {
            p.has_key = self.secrets.has(&format!("provider:{}", p.id));
            p
        }).collect()
    }

    pub fn get(&self, id: &str) -> Option<ProviderConfig> {
        self.list().into_iter().find(|p| p.id == id)
    }

    pub fn upsert(&self, cfg: ProviderConfig, api_key: Option<&str>) -> Result<ProviderConfig, String> {
        if let Some(k) = api_key.map(str::trim).filter(|k| !k.is_empty()) {
            self.secrets.set(&format!("provider:{}", cfg.id), k)?;
        }
        let mut list: Vec<ProviderConfig> = self.store.providers().into_iter().filter(|p| p.id != cfg.id).collect();
        let saved = ProviderConfig { has_key: self.secrets.has(&format!("provider:{}", cfg.id)), ..cfg };
        list.push(saved.clone());
        self.store.save_providers(list);
        self.forget(&saved.id);
        Ok(saved)
    }

    pub fn remove_key(&self, id: &str) {
        self.secrets.delete(&format!("provider:{id}"));
        self.forget(id);
    }

    pub fn remove(&self, id: &str) {
        self.secrets.delete(&format!("provider:{id}"));
        self.store.save_providers(self.store.providers().into_iter().filter(|p| p.id != id).collect());
        self.forget(id);
        self.store.update(|s| {
            for t in s.roles.values_mut() {
                if t.as_ref().is_some_and(|t| t.provider_id == id) {
                    *t = None;
                }
            }
        });
    }

    fn forget(&self, id: &str) {
        self.adapters.lock().unwrap().remove(id);
        self.model_cache.lock().unwrap().remove(id);
    }

    pub fn adapter(&self, id: &str) -> Option<Arc<dyn ProviderAdapter>> {
        let cfg = self.get(id)?;
        let mut cache = self.adapters.lock().unwrap();
        Some(
            cache
                .entry(id.to_string())
                .or_insert_with(|| create_adapter(AdapterDeps { cfg, api_key: self.secrets.get(&format!("provider:{id}")), net: self.net.clone() }))
                .clone(),
        )
    }

    pub async fn models(&self, id: &str, refresh: bool) -> Result<Vec<ModelInfo>, AppError> {
        if !refresh {
            if let Some((at, m)) = self.model_cache.lock().unwrap().get(id) {
                if at.elapsed() < MODEL_TTL {
                    return Ok(m.clone());
                }
            }
        }
        let Some(a) = self.adapter(id) else { return Ok(Vec::new()) };
        let models = a.list_models().await?;
        self.model_cache.lock().unwrap().insert(id.to_string(), (Instant::now(), models.clone()));
        Ok(models)
    }

    pub fn resolve(&self, role: ModelRole, settings: &Settings) -> Option<ResolvedTarget> {
        let t = settings.role(role)?;
        let cfg = self.get(&t.provider_id)?;
        let adapter = self.adapter(&cfg.id)?;
        Some(ResolvedTarget { is_local: is_local_target(&cfg.base_url, &t.model), model: t.model.clone(), cfg, adapter })
    }

    /// I use only providers the user explicitly marked as allowed fallbacks.
    pub fn fallback(&self, role: ModelRole, exclude: &str, settings: &Settings) -> Option<ResolvedTarget> {
        for r in [role, ModelRole::QuickAnswer, ModelRole::DeepAnswer] {
            let Some(t) = settings.role(r) else { continue };
            if t.provider_id == exclude {
                continue;
            }
            if let Some(cfg) = self.get(&t.provider_id).filter(|c| c.allow_fallback) {
                if let Some(adapter) = self.adapter(&cfg.id) {
                    return Some(ResolvedTarget { is_local: is_local_target(&cfg.base_url, &t.model), model: t.model.clone(), cfg, adapter });
                }
            }
        }
        None
    }

    pub async fn test(&self, id: &str, model: Option<&str>) -> ProviderTestReport {
        let (Some(a), Some(cfg)) = (self.adapter(id), self.get(id)) else {
            return ProviderTestReport {
                models: Err(app_error(ErrorCode::NotFound, Some(t("Провайдера не знайдено", "Provider not found")), None)),
                text: None,
                vision: None,
            };
        };
        let t0 = Instant::now();
        let list = match self.models(id, true).await {
            Ok(l) => l,
            Err(e) => return ProviderTestReport { models: Err(e), text: None, vision: None },
        };
        let models = Ok((t0.elapsed().as_millis() as u64, list.len()));
        let Some(model) = model.filter(|m| !m.is_empty()) else { return ProviderTestReport { models, text: None, vision: None } };
        // Cold-loading a local model into VRAM routinely takes 20–60 s on the first request.
        let budget = if is_loopback_url(&cfg.base_url) { 120_000 } else { 30_000 };
        let opts = RunOptions { timeout_ms: cfg.timeout_ms.max(20_000).min(budget), ..RunOptions::new(0) };
        let req = ChatRequest {
            model: model.into(),
            messages: vec![ChatMessage { role: Role::User, content: "Reply with exactly: OK".into(), images: vec![] }],
            max_tokens: 64,
            temperature: 0.0,
        };
        let text = match run_stream(a.as_ref(), &req, &CancellationToken::new(), |_| {}, opts.retries(1)).await {
            Ok(r) => TextCheck { ok: !r.text.is_empty(), ms: r.first_token_ms, sample: r.text.chars().take(60).collect(), error: None },
            Err(e) => TextCheck { ok: false, ms: None, sample: String::new(), error: Some(e.line()) },
        };
        let key = format!("{id}/{model}");
        self.text_ok.lock().unwrap().insert(key.clone(), text.ok);
        if list.iter().find(|m| m.id == model).and_then(|m| m.vision) == Some(false) {
            self.vision_ok.lock().unwrap().insert(key, false);
            return ProviderTestReport {
                models,
                text: Some(text),
                vision: Some(VisionCheck { ok: false, error: Some(t("Модель без підтримки зображень — для екрана буде OCR", "The model has no image support — screenshots will use OCR").into()) }),
            };
        }
        let vreq = ChatRequest {
            model: model.into(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: "What is the main color of this image? One word.".into(),
                images: vec![ChatImage { mime: "image/png".into(), base64: RED_PNG.clone() }],
            }],
            max_tokens: 64,
            temperature: 0.0,
        };
        let vision = match run_stream(a.as_ref(), &vreq, &CancellationToken::new(), |_| {}, opts.retries(0)).await {
            Ok(r) => {
                let l = r.text.to_lowercase();
                let ok = l.contains("red") || l.contains("червон");
                VisionCheck {
                    ok,
                    error: (!ok).then(|| {
                        crate::tr!("Модель не розпізнала тестове зображення («{}»)", "The model did not recognize the test image (\"{}\")", r.text.chars().take(40).collect::<String>())
                    }),
                }
            }
            Err(e) => VisionCheck { ok: false, error: Some(e.message) },
        };
        self.vision_ok.lock().unwrap().insert(key, vision.ok);
        ProviderTestReport { models, text: Some(text), vision: Some(vision) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_suffixed_models_are_not_local_on_loopback() {
        assert!(is_local_target("http://127.0.0.1:8080/v1", "qwen2.5-14b-instruct-q5_k_m.gguf"));
        assert!(!is_local_target("http://127.0.0.1:4000/v1", "gpt-oss:120b-cloud"));
        assert!(!is_local_target("http://127.0.0.1:4000/v1", "deepseek-v3.1:cloud"));
        assert!(!is_local_target("https://api.openai.com/v1", "gpt-5"));
    }
}
