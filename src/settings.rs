use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::types::{ProviderConfig, Settings};

/// Presets the old build supported and this one dropped (Ollama, LM Studio): their entries are removed on load.
const REMOVED_PRESETS: [&str; 2] = ["ollama", "lmstudio"];

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

pub fn write_json_atomic(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&tmp, path)
}

pub struct SettingsStore {
    settings_path: PathBuf,
    providers_path: PathBuf,
    current: Mutex<Settings>,
    providers: Mutex<Vec<ProviderConfig>>,
}

impl SettingsStore {
    pub fn open(settings_path: PathBuf, providers_path: PathBuf) -> Self {
        let mut settings: Settings = read_json(&settings_path).unwrap_or_default();
        let mut providers: Vec<ProviderConfig> = read_json(&providers_path).unwrap_or_default();
        let removed: Vec<String> = providers.iter().filter(|p| REMOVED_PRESETS.contains(&p.preset.as_str())).map(|p| p.id.clone()).collect();
        if !removed.is_empty() {
            providers.retain(|p| !removed.contains(&p.id));
            for t in settings.roles.values_mut() {
                if t.as_ref().is_some_and(|t| removed.contains(&t.provider_id)) {
                    *t = None;
                }
            }
            let _ = write_json_atomic(&providers_path, &providers);
            let _ = write_json_atomic(&settings_path, &settings);
        }
        Self { settings_path, providers_path, current: Mutex::new(settings), providers: Mutex::new(providers) }
    }

    pub fn get(&self) -> Settings {
        self.current.lock().unwrap().clone()
    }

    /// Applies `f` and persists; returns (previous, current).
    pub fn update(&self, f: impl FnOnce(&mut Settings)) -> (Settings, Settings) {
        let mut cur = self.current.lock().unwrap();
        let prev = cur.clone();
        f(&mut cur);
        if *cur != prev {
            let _ = write_json_atomic(&self.settings_path, &*cur);
        }
        (prev, cur.clone())
    }

    pub fn providers(&self) -> Vec<ProviderConfig> {
        self.providers.lock().unwrap().clone()
    }

    pub fn save_providers(&self, list: Vec<ProviderConfig>) {
        let _ = write_json_atomic(&self.providers_path, &list);
        *self.providers.lock().unwrap() = list;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Lang, LangPref};

    #[test]
    fn old_russian_settings_load_as_ukrainian_and_drop_removed_presets() {
        let dir = std::env::temp_dir().join(format!("copilot-set-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("settings.json"),
            r#"{"uiLang":"ru","answerLang":"ru","asr":{"language":"ru","model":"small"},"roles":{"quick_answer":{"providerId":"o1","model":"qwen"}},"unknownField":1}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("providers.json"),
            r#"[{"id":"o1","label":"Ollama","preset":"ollama","protocol":"openai","baseUrl":"http://127.0.0.1:11434/v1","hasKey":false,"allowFallback":false,"timeoutMs":30000}]"#,
        )
        .unwrap();
        let s = SettingsStore::open(dir.join("settings.json"), dir.join("providers.json"));
        let st = s.get();
        assert_eq!(st.ui_lang, Lang::Uk);
        assert_eq!(st.answer_lang, LangPref::Uk);
        assert_eq!(st.asr.language, LangPref::Uk);
        assert_eq!(st.asr.model, "small");
        assert_eq!(st.asr.threads, 6, "missing fields fall back to defaults");
        assert!(s.providers().is_empty());
        assert!(st.role(crate::types::ModelRole::QuickAnswer).is_none());
    }
}
