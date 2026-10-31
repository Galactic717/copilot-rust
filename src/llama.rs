//! llama.cpp support beyond "point at a running server": the app can start `llama-server` itself with a GGUF model,
//! wait until the model is loaded (`/health` → 200) and stop it on exit.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::i18n::t;
use crate::net::Net;
use crate::types::{LlamaLaunch, ProviderConfig};

#[derive(Debug, Clone, PartialEq)]
pub enum LlamaState {
    Stopped,
    Starting,
    Running,
    Failed(String),
}

struct Proc {
    child: Child,
    state: Arc<Mutex<LlamaState>>,
    log: Arc<Mutex<Vec<String>>>,
}

#[derive(Default)]
pub struct LlamaManager {
    procs: Mutex<HashMap<String, Proc>>,
    failures: Mutex<HashMap<String, String>>,
}

fn port_of(base_url: &str) -> Option<u16> {
    reqwest::Url::parse(base_url).ok()?.port_or_known_default()
}

fn root_of(base_url: &str) -> String {
    base_url.trim_end_matches('/').trim_end_matches("/v1").to_string()
}

impl LlamaManager {
    pub fn state(&self, id: &str) -> LlamaState {
        let mut procs = self.procs.lock().unwrap();
        if let Some(p) = procs.get_mut(id) {
            if let Ok(Some(code)) = p.child.try_wait() {
                let tail = p.log.lock().unwrap().iter().rev().take(3).cloned().collect::<Vec<_>>().join(" | ");
                let msg = crate::tr!("llama-server завершився (код {}): {}", "llama-server exited (code {}): {}", code.code().unwrap_or(-1), tail);
                procs.remove(id);
                self.failures.lock().unwrap().insert(id.to_string(), msg.clone());
                return LlamaState::Failed(msg);
            }
            return p.state.lock().unwrap().clone();
        }
        match self.failures.lock().unwrap().get(id) {
            Some(m) => LlamaState::Failed(m.clone()),
            None => LlamaState::Stopped,
        }
    }

    pub fn stop(&self, id: &str) {
        if let Some(mut p) = self.procs.lock().unwrap().remove(id) {
            let _ = p.child.kill();
            let _ = p.child.wait();
        }
        self.failures.lock().unwrap().remove(id);
    }

    pub fn stop_all(&self) {
        let ids: Vec<String> = self.procs.lock().unwrap().keys().cloned().collect();
        for id in ids {
            self.stop(&id);
        }
    }

    /// Starts llama-server for `cfg` (if configured and not already up) and waits until the model is loaded.
    pub async fn ensure(&self, cfg: &ProviderConfig, net: &Net) -> Result<(), String> {
        let Some(launch) = cfg.llama.as_ref().filter(|l| !l.server_path.trim().is_empty()) else { return Ok(()) };
        if matches!(self.state(&cfg.id), LlamaState::Running) {
            return Ok(());
        }
        // Someone (the user, another app) may already serve this port.
        if healthy(net, &cfg.base_url).await {
            return Ok(());
        }
        if !matches!(self.state(&cfg.id), LlamaState::Starting) {
            self.spawn(cfg, launch)?;
        }
        let state = self.procs.lock().unwrap().get(&cfg.id).map(|p| p.state.clone());
        let Some(state) = state else { return Err(t("llama-server не запущено", "llama-server is not running").into()) };
        // A 30B GGUF from a spinning disk can take minutes to map; I wait up to 5 minutes.
        let deadline = Instant::now() + Duration::from_secs(300);
        while Instant::now() < deadline {
            if let LlamaState::Failed(m) = self.state(&cfg.id) {
                return Err(m);
            }
            if healthy(net, &cfg.base_url).await {
                *state.lock().unwrap() = LlamaState::Running;
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        self.stop(&cfg.id);
        Err(t("llama-server не завантажив модель за 5 хв", "llama-server did not load the model within 5 min").into())
    }

    fn spawn(&self, cfg: &ProviderConfig, l: &LlamaLaunch) -> Result<(), String> {
        let server = std::path::Path::new(l.server_path.trim());
        if !server.is_file() {
            return Err(crate::tr!("Не знайдено llama-server: {}", "llama-server not found: {}", server.display()));
        }
        if !std::path::Path::new(l.model_path.trim()).is_file() {
            return Err(crate::tr!("Не знайдено модель GGUF: {}", "GGUF model not found: {}", l.model_path));
        }
        let port = port_of(&cfg.base_url).ok_or_else(|| t("В адресі немає порту", "The address has no port").to_string())?;
        let mut cmd = Command::new(server);
        cmd.args(["-m", l.model_path.trim(), "--host", "127.0.0.1", "--port", &port.to_string()]);
        if l.ctx_size > 0 {
            cmd.args(["-c", &l.ctx_size.to_string()]);
        }
        if l.gpu_layers != 0 {
            cmd.args(["-ngl", &l.gpu_layers.to_string()]);
        }
        if let Some(dir) = server.parent() {
            cmd.current_dir(dir);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = crate::proc::spawn_hidden(&mut cmd).map_err(|e| e.to_string())?;
        let log = Arc::new(Mutex::new(Vec::new()));
        for pipe in [child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>), child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>)]
            .into_iter()
            .flatten()
        {
            let log = log.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    let mut l = log.lock().unwrap();
                    l.push(line.chars().take(300).collect());
                    if l.len() > 200 {
                        l.remove(0);
                    }
                }
            });
        }
        self.failures.lock().unwrap().remove(&cfg.id);
        self.procs.lock().unwrap().insert(cfg.id.clone(), Proc { child, state: Arc::new(Mutex::new(LlamaState::Starting)), log });
        crate::db::diag("llama_start", serde_json::json!({ "port": port }));
        Ok(())
    }
}

async fn healthy(net: &Net, base_url: &str) -> bool {
    let Ok(rb) = net.get(&format!("{}/health", root_of(base_url))) else { return false };
    matches!(rb.timeout(Duration::from_secs(2)).send().await, Ok(r) if r.status().is_success())
}
