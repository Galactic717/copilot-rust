//! Finds local LLMs without being told about them: servers already running on their default ports, runtimes that are
//! installed but stopped, and GGUF files left by LM Studio, Hugging Face, GPT4All, Jan or Ollama. A file needs no
//! running app: the bundled llama-server can serve it. Nothing here leaves the machine: only loopback is probed.

use std::collections::HashSet;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::net::Net;

/// Default ports of the runtimes that speak the OpenAI API. Order decides whose name a shared port gets.
const PORTS: &[(u16, &str)] = &[
    (11434, "Ollama"),
    (1234, "LM Studio"),
    (1337, "Jan"),
    (4891, "GPT4All"),
    (8080, "llama.cpp"),
    (9931, "llama.cpp"),
    (5001, "KoboldCpp"),
    (5000, "text-generation-webui"),
    (8000, "vLLM"),
    (4000, "LiteLLM"),
    (39281, "Cortex"),
    (10000, "Msty"),
];

const MIN_GGUF_BYTES: u64 = 50 * 1024 * 1024;
const MAX_WALK_ENTRIES: usize = 40_000;
const MAX_DEPTH: usize = 7;

/// Architectures that load fine but cannot chat.
const NOT_CHAT: &[&str] = &["clip", "bert", "nomic-bert", "jina-bert-v2", "jina-bert-v3", "new", "t5encoder", "wavtokenizer-dec", "whisper"];

#[derive(Debug, Clone, PartialEq)]
pub struct GgufModel {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    /// The projector beside the model; with one, the model can read screenshots.
    pub mmproj: Option<PathBuf>,
    /// Where it was found: "LM Studio", "Hugging Face", …
    pub origin: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    /// Answering right now.
    Server { runtime: &'static str, base_url: String, models: Vec<String> },
    /// Installed but stopped; the app can start it.
    Stopped { runtime: &'static str, exe: PathBuf, base_url: String, models: Vec<String> },
    Gguf(GgufModel),
}

/// The folders to look in. A struct so tests can point it at a temp dir instead of the real home.
#[derive(Clone)]
pub struct Places {
    pub home: PathBuf,
    pub local_app_data: PathBuf,
    pub app_data: PathBuf,
    pub path_env: String,
    pub hf_home: Option<PathBuf>,
    pub ollama_models: Option<PathBuf>,
    /// This app's own data folder: models it downloaded and the llama-server it unpacked.
    pub data_dir: PathBuf,
}

impl Places {
    pub fn from_env(data_dir: &Path) -> Self {
        let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
        Self {
            home: var("USERPROFILE").unwrap_or_else(|| PathBuf::from(".")),
            local_app_data: var("LOCALAPPDATA").unwrap_or_else(|| PathBuf::from(".")),
            app_data: var("APPDATA").unwrap_or_else(|| PathBuf::from(".")),
            path_env: std::env::var("PATH").unwrap_or_default(),
            hf_home: var("HF_HOME").or_else(|| var("HF_HUB_CACHE")),
            ollama_models: var("OLLAMA_MODELS"),
            data_dir: data_dir.to_path_buf(),
        }
    }

    fn gguf_roots(&self) -> Vec<(PathBuf, &'static str)> {
        let h = &self.home;
        let mut r = vec![
            (self.data_dir.join("models"), "Copilot"),
            (h.join(".lmstudio").join("models"), "LM Studio"),
            (h.join(".cache").join("lm-studio").join("models"), "LM Studio"),
            (h.join(".cache").join("huggingface").join("hub"), "Hugging Face"),
            (h.join(".cache").join("llama.cpp"), "llama.cpp"),
            (self.local_app_data.join("llama.cpp"), "llama.cpp"),
            (self.local_app_data.join("nomic.ai").join("GPT4All"), "GPT4All"),
            (self.app_data.join("Jan").join("data").join("llamacpp").join("models"), "Jan"),
            (self.app_data.join("Jan").join("data").join("models"), "Jan"),
            (h.join("jan").join("models"), "Jan"),
            (h.join("models"), "~/models"),
            (h.join("Downloads"), "Downloads"),
        ];
        if let Some(hf) = &self.hf_home {
            r.push((hf.join("hub"), "Hugging Face"));
            r.push((hf.clone(), "Hugging Face"));
        }
        r
    }

    fn on_path(&self, exe: &str) -> Option<PathBuf> {
        std::env::split_paths(&self.path_env).map(|d| d.join(exe)).find(|p| p.is_file())
    }

    fn ollama_dir(&self) -> PathBuf {
        self.ollama_models.clone().unwrap_or_else(|| self.home.join(".ollama").join("models"))
    }

    fn ollama_exe(&self) -> Option<PathBuf> {
        self.on_path("ollama.exe").or_else(|| Some(self.local_app_data.join("Programs").join("Ollama").join("ollama.exe")).filter(|p| p.is_file()))
    }

    /// The user's own llama-server first (it may be a CUDA build); the bundled Vulkan one otherwise.
    pub fn llama_server(&self) -> Option<PathBuf> {
        self.on_path("llama-server.exe")
            .or_else(|| find_file_shallow(&self.local_app_data.join("Microsoft").join("WinGet").join("Packages"), "llama-server.exe", "ggml.llamacpp"))
            .or_else(|| Some(self.home.join("scoop").join("apps").join("llama.cpp").join("current").join("llama-server.exe")).filter(|p| p.is_file()))
            .or_else(|| Some(self.data_dir.join("llama").join("llama-server.exe")).filter(|p| p.is_file()))
    }
}

/// WinGet unpacks into `Packages/<id>_<source>/…`: only folders whose name starts with `prefix` are entered.
fn find_file_shallow(root: &Path, name: &str, prefix: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(root).ok()?.flatten() {
        if e.file_name().to_string_lossy().to_lowercase().starts_with(&prefix.to_lowercase()) {
            let p = e.path().join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Names of the models a server offers, without the ones that cannot answer: embeddings, rerankers, and `:cloud` proxies.
fn chat_models(j: &Value) -> Option<Vec<String>> {
    let data = j.get("data")?.as_array()?;
    Some(
        data.iter()
            .filter_map(|m| m.get("id")?.as_str().map(String::from))
            .filter(|id| {
                let l = id.to_lowercase();
                !l.contains("embed") && !l.contains("rerank") && !l.contains("bge-") && !l.contains("whisper") && !l.ends_with(":cloud") && !l.ends_with("-cloud")
            })
            .collect(),
    )
}

async fn probe(net: &Net, port: u16, runtime: &'static str) -> Option<Found> {
    let base_url = format!("http://127.0.0.1:{port}/v1");
    let res = net.get(&format!("{base_url}/models")).ok()?.timeout(Duration::from_millis(900)).send().await.ok()?;
    if !res.status().is_success() {
        return None;
    }
    // Any web server may sit on 5000 or 8080: only a real model list counts.
    let models = chat_models(&res.json::<Value>().await.ok()?)?;
    Some(Found::Server { runtime, base_url, models })
}

pub async fn scan_servers(net: &Net, ports: &[(u16, &'static str)]) -> Vec<Found> {
    futures_util::future::join_all(ports.iter().map(|(p, r)| probe(net, *p, r))).await.into_iter().flatten().collect()
}

// ---- GGUF ----

fn read_u32(r: &mut impl Read) -> Option<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).ok()?;
    Some(u32::from_le_bytes(b))
}

fn read_u64(r: &mut impl Read) -> Option<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b).ok()?;
    Some(u64::from_le_bytes(b))
}

fn read_str(r: &mut impl Read) -> Option<String> {
    let n = read_u64(r)?;
    if n > 1 << 20 {
        return None;
    }
    let mut b = vec![0u8; n as usize];
    r.read_exact(&mut b).ok()?;
    Some(String::from_utf8_lossy(&b).into_owned())
}

/// Bytes of a fixed-size GGUF value type; None for string and array.
fn scalar_size(t: u32) -> Option<u64> {
    Some(match t {
        0 | 1 | 7 => 1,
        2..=3 => 2,
        4..=6 => 4,
        10..=12 => 8,
        _ => return None,
    })
}

fn skip_value(r: &mut impl Read, t: u32) -> Option<()> {
    match (t, scalar_size(t)) {
        (_, Some(n)) => {
            std::io::copy(&mut r.by_ref().take(n), &mut std::io::sink()).ok()?;
        }
        (8, _) => {
            read_str(r)?;
        }
        (9, _) => {
            let (inner, n) = (read_u32(r)?, read_u64(r)?);
            match scalar_size(inner) {
                Some(s) => {
                    std::io::copy(&mut r.by_ref().take(s.checked_mul(n)?), &mut std::io::sink()).ok()?;
                }
                None => {
                    for _ in 0..n {
                        skip_value(r, inner)?;
                    }
                }
            }
        }
        _ => return None,
    }
    Some(())
}

#[derive(Debug, Default, PartialEq)]
pub struct GgufMeta {
    pub architecture: String,
    pub name: Option<String>,
}

/// Reads only the header: magic, then key/values until the architecture and the name are known.
pub fn read_gguf_meta(path: &Path) -> Option<GgufMeta> {
    let mut r = BufReader::new(std::fs::File::open(path).ok()?);
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic).ok()?;
    if &magic != b"GGUF" {
        return None;
    }
    read_u32(&mut r)?;
    read_u64(&mut r)?;
    let kv = read_u64(&mut r)?;
    let mut meta = GgufMeta::default();
    // The fields I want sit at the very start of real files; the tokenizer arrays after them are megabytes I never need.
    for _ in 0..kv.min(64) {
        let key = read_str(&mut r)?;
        let t = read_u32(&mut r)?;
        match (key.as_str(), t) {
            ("general.architecture", 8) => meta.architecture = read_str(&mut r)?,
            ("general.name", 8) => meta.name = Some(read_str(&mut r)?),
            _ if key.starts_with("tokenizer.") => break,
            _ => skip_value(&mut r, t)?,
        }
        if !meta.architecture.is_empty() && meta.name.is_some() {
            break;
        }
    }
    (!meta.architecture.is_empty()).then_some(meta)
}

fn walk(dir: &Path, depth: usize, budget: &mut usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let p = e.path();
        // metadata() follows links: the Hugging Face cache keeps its files behind symlinks.
        let Ok(m) = std::fs::metadata(&p) else { continue };
        if m.is_dir() {
            if depth > 0 {
                walk(&p, depth - 1, budget, out);
            }
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("gguf")) {
            out.push(p);
        }
    }
}

fn is_mmproj(p: &Path) -> bool {
    p.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().contains("mmproj"))
}

/// `model-00002-of-00003.gguf` is a continuation; only part 1 is loadable.
fn is_later_split(p: &Path) -> bool {
    let n = p.file_stem().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    match n.rfind("-of-") {
        Some(i) if i >= 5 => n[i - 5..i].chars().all(|c| c.is_ascii_digit()) && &n[i - 5..i] != "00001",
        _ => false,
    }
}

fn describe(path: &Path, origin: &'static str, display_name: Option<String>) -> Option<GgufModel> {
    let size = std::fs::metadata(path).ok()?.len();
    if size < MIN_GGUF_BYTES {
        return None;
    }
    let meta = read_gguf_meta(path)?;
    if NOT_CHAT.contains(&meta.architecture.as_str()) {
        return None;
    }
    let stem = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name = display_name.or(Some(stem).filter(|s| !s.starts_with("sha256"))).or(meta.name).unwrap_or_default();
    let mmproj = path.parent().and_then(|d| std::fs::read_dir(d).ok()).and_then(|rd| rd.flatten().map(|e| e.path()).find(|p| is_mmproj(p) && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("gguf"))));
    Some(GgufModel { path: path.to_path_buf(), name, size, mmproj, origin })
}

/// Models Ollama has pulled, as (`name:tag`, path of the GGUF blob). The manifest says which blob holds the weights.
fn ollama_library(models_dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let root = models_dir.join("manifests").join("registry.ollama.ai");
    let Ok(namespaces) = std::fs::read_dir(&root) else { return out };
    for ns in namespaces.flatten() {
        let Ok(models) = std::fs::read_dir(ns.path()) else { continue };
        for m in models.flatten() {
            let Ok(tags) = std::fs::read_dir(m.path()) else { continue };
            for tag in tags.flatten() {
                let Some(j) = std::fs::read(tag.path()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { continue };
                let weights = j.get("layers").and_then(Value::as_array).and_then(|l| l.iter().find(|x| x.get("mediaType").and_then(Value::as_str) == Some("application/vnd.ollama.image.model")));
                let Some(digest) = weights.and_then(|w| w.get("digest")).and_then(Value::as_str) else { continue };
                let ns_name = ns.file_name().to_string_lossy().into_owned();
                let prefix = if ns_name == "library" { String::new() } else { format!("{ns_name}/") };
                out.push((format!("{prefix}{}:{}", m.file_name().to_string_lossy(), tag.file_name().to_string_lossy()), models_dir.join("blobs").join(digest.replace(':', "-"))));
            }
        }
    }
    out
}

pub fn scan_files(places: &Places) -> Vec<Found> {
    let mut found = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let library = ollama_library(&places.ollama_dir());
    if let Some(exe) = places.ollama_exe() {
        let models = library.iter().map(|(n, _)| n.clone()).filter(|n| !n.ends_with(":cloud") && !n.to_lowercase().contains("embed")).collect();
        found.push(Found::Stopped { runtime: "Ollama", exe, base_url: "http://127.0.0.1:11434/v1".into(), models });
    } else {
        // Ollama is gone but its weights are not: they are ordinary GGUF files.
        for (name, blob) in &library {
            if seen.insert(blob.clone()) {
                if let Some(m) = describe(blob, "Ollama", Some(name.clone())) {
                    found.push(Found::Gguf(m));
                }
            }
        }
    }
    for (root, origin) in places.gguf_roots() {
        let mut files = Vec::new();
        let mut budget = MAX_WALK_ENTRIES;
        // Downloads gets a look at the top level only: it can be huge and is not a model folder.
        let depth = if origin == "Downloads" { 0 } else { MAX_DEPTH };
        walk(&root, depth, &mut budget, &mut files);
        for p in files {
            if is_mmproj(&p) || is_later_split(&p) || !seen.insert(p.clone()) {
                continue;
            }
            if let Some(m) = describe(&p, origin, None) {
                found.push(Found::Gguf(m));
            }
        }
    }
    found
}

/// Running servers first, then what can be started, then files that fit in memory (biggest first: usually the best one that still runs).
pub fn rank(mut found: Vec<Found>, ram_bytes: u64) -> Vec<Found> {
    let score = |f: &Found| -> (u8, u64) {
        match f {
            Found::Server { models, .. } if !models.is_empty() => (0, 0),
            Found::Server { .. } => (4, 0),
            Found::Stopped { models, .. } if !models.is_empty() => (1, 0),
            Found::Stopped { .. } => (4, 0),
            Found::Gguf(m) if fits(m.size, ram_bytes) => (2, u64::MAX - m.size),
            Found::Gguf(m) => (3, m.size),
        }
    };
    found.sort_by_key(score);
    found
}

/// Default-port scan plus the disk, in the order worth offering.
pub async fn scan(net: &Net, places: &Places, ram_bytes: u64) -> Vec<Found> {
    let mut found = scan_servers(net, PORTS).await;
    let p = places.clone();
    let files = tokio::task::spawn_blocking(move || scan_files(&p)).await.unwrap_or_default();
    // A runtime that answered on its port is not also "installed but stopped".
    let running: Vec<&'static str> = found.iter().filter_map(|f| if let Found::Server { runtime, .. } = f { Some(*runtime) } else { None }).collect();
    found.extend(files.into_iter().filter(|f| !matches!(f, Found::Stopped { runtime, .. } if running.contains(runtime))));
    rank(found, ram_bytes)
}

/// A model needs its weights plus roughly a fifth more for context and runtime.
pub fn fits(size: u64, ram_bytes: u64) -> bool {
    ram_bytes == 0 || size + size / 5 < ram_bytes
}

pub struct ModelOffer {
    pub id: &'static str,
    pub title: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    pub min_ram_gb: u64,
}

/// Apache-2.0, multilingual, good on a laptop. Pinned to a revision and a digest, like the speech models.
pub const OFFERS: &[ModelOffer] = &[
    ModelOffer {
        id: "qwen3.5-2b",
        title: "Qwen 3.5 · 2B",
        file: "Qwen3.5-2B-Q4_K_M.gguf",
        url: "https://huggingface.co/unsloth/Qwen3.5-2B-GGUF/resolve/f6d5376be1edb4d416d56da11e5397a961aca8ae/Qwen3.5-2B-Q4_K_M.gguf",
        sha256: "aaf42c8b7c3cab2bf3d69c355048d4a0ee9973d48f16c731c0520ee914699223",
        bytes: 1_280_000_000,
        min_ram_gb: 8,
    },
    ModelOffer {
        id: "qwen3.5-4b",
        title: "Qwen 3.5 · 4B",
        file: "Qwen3.5-4B-Q4_K_M.gguf",
        url: "https://huggingface.co/unsloth/Qwen3.5-4B-GGUF/resolve/e87f176479d0855a907a41277aca2f8ee7a09523/Qwen3.5-4B-Q4_K_M.gguf",
        sha256: "00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4",
        bytes: 2_740_000_000,
        min_ram_gb: 16,
    },
];

/// Pinned, so a new llama.cpp release can never change what users run without a new app version.
pub const LLAMA_ZIP: (&str, &str) = (
    "https://github.com/ggml-org/llama.cpp/releases/download/b12052/llama-b12052-bin-win-vulkan-x64.zip",
    "9806d110501576d9aa4a58de996209a89ffca3c0634e4bda54b0019dfafc3be5",
);

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("copilot-disc-{}", crate::types::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn places(root: &Path) -> Places {
        Places {
            home: root.join("home"),
            local_app_data: root.join("local"),
            app_data: root.join("roaming"),
            path_env: String::new(),
            hf_home: None,
            ollama_models: None,
            data_dir: root.join("data"),
        }
    }

    fn gstr(v: &mut Vec<u8>, s: &str) {
        v.extend_from_slice(&(s.len() as u64).to_le_bytes());
        v.extend_from_slice(s.as_bytes());
    }

    /// A header with a scalar before the fields of interest, padded past the "too small to be a model" limit.
    fn gguf(arch: &str, name: &str) -> Vec<u8> {
        let mut v = b"GGUF".to_vec();
        v.extend_from_slice(&3u32.to_le_bytes());
        v.extend_from_slice(&0u64.to_le_bytes());
        v.extend_from_slice(&3u64.to_le_bytes());
        gstr(&mut v, "general.quantization_version");
        v.extend_from_slice(&4u32.to_le_bytes());
        v.extend_from_slice(&2u32.to_le_bytes());
        gstr(&mut v, "general.architecture");
        v.extend_from_slice(&8u32.to_le_bytes());
        gstr(&mut v, arch);
        gstr(&mut v, "general.name");
        v.extend_from_slice(&8u32.to_le_bytes());
        gstr(&mut v, name);
        v
    }

    fn write_model(path: &Path, arch: &str, name: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let f = std::fs::File::create(path).unwrap();
        use std::io::Write;
        (&f).write_all(&gguf(arch, name)).unwrap();
        f.set_len(MIN_GGUF_BYTES + 1).unwrap();
    }

    #[test]
    fn reads_gguf_header_and_rejects_other_files() {
        let d = tmp();
        let p = d.join("m.gguf");
        std::fs::write(&p, gguf("qwen3", "Qwen 3")).unwrap();
        assert_eq!(read_gguf_meta(&p), Some(GgufMeta { architecture: "qwen3".into(), name: Some("Qwen 3".into()) }));
        std::fs::write(&p, b"not a model").unwrap();
        assert_eq!(read_gguf_meta(&p), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn finds_lm_studio_and_hugging_face_models_and_skips_what_cannot_chat() {
        let d = tmp();
        let p = places(&d);
        let lm = p.home.join(".lmstudio/models/org/repo");
        write_model(&lm.join("chat-Q4.gguf"), "qwen3", "Chat");
        write_model(&lm.join("mmproj-F16.gguf"), "clip", "Projector");
        write_model(&p.home.join(".cache/huggingface/hub/models--x/snapshots/abc/nomic-embed.gguf"), "nomic-bert", "Embed");
        write_model(&p.home.join(".cache/huggingface/hub/models--y/snapshots/abc/big-00001-of-00002.gguf"), "llama", "Big");
        write_model(&p.home.join(".cache/huggingface/hub/models--y/snapshots/abc/big-00002-of-00002.gguf"), "llama", "Big");
        let found = scan_files(&p);
        let names: Vec<String> = found.iter().map(|f| if let Found::Gguf(m) = f { m.name.clone() } else { String::new() }).collect();
        assert_eq!(found.len(), 2, "{names:?}");
        assert!(names.contains(&"chat-Q4".to_string()));
        assert!(names.contains(&"big-00001-of-00002".to_string()));
        let chat = found.iter().find_map(|f| if let Found::Gguf(m) = f { (m.name == "chat-Q4").then_some(m) } else { None }).unwrap();
        assert_eq!(chat.origin, "LM Studio");
        assert_eq!(chat.mmproj.as_deref(), Some(lm.join("mmproj-F16.gguf").as_path()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ollama_manifests_point_at_their_weights() {
        let d = tmp();
        let p = places(&d);
        let dir = p.ollama_dir();
        let manifest = dir.join("manifests/registry.ollama.ai/library/qwen3/8b");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(&manifest, r#"{"layers":[{"mediaType":"application/vnd.ollama.image.template","digest":"sha256:aaa"},{"mediaType":"application/vnd.ollama.image.model","digest":"sha256:bbb"}]}"#).unwrap();
        write_model(&dir.join("blobs/sha256-bbb"), "qwen3", "Qwen");
        // Without ollama.exe the blob is still usable, under its friendly name.
        let found = scan_files(&p);
        assert!(matches!(&found[0], Found::Gguf(m) if m.name == "qwen3:8b" && m.origin == "Ollama"), "{found:?}");
        // With ollama.exe the runtime itself is offered.
        let exe = p.local_app_data.join("Programs/Ollama/ollama.exe");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"").unwrap();
        let found = scan_files(&p);
        assert!(matches!(&found[0], Found::Stopped { runtime: "Ollama", models, .. } if models == &vec!["qwen3:8b".to_string()]), "{found:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ranks_running_first_and_what_fits_memory_next() {
        let gb = 1_073_741_824u64;
        let model = |name: &str, size: u64| Found::Gguf(GgufModel { path: name.into(), name: name.into(), size, mmproj: None, origin: "x" });
        let server = Found::Server { runtime: "Ollama", base_url: "u".into(), models: vec!["m".into()] };
        let ranked = rank(vec![model("huge", 40 * gb), model("small", 2 * gb), model("mid", 6 * gb), server.clone()], 16 * gb);
        assert_eq!(ranked[0], server);
        let names: Vec<String> = ranked[1..].iter().map(|f| if let Found::Gguf(m) = f { m.name.clone() } else { String::new() }).collect();
        assert_eq!(names, ["mid", "small", "huge"]);
        assert!(!fits(14 * gb, 16 * gb));
    }

    #[test]
    fn server_list_filters_non_chat_models() {
        let j: Value = serde_json::json!({"data":[{"id":"qwen3:8b"},{"id":"nomic-embed-text"},{"id":"gpt-oss:cloud"},{"id":"bge-m3"}]});
        assert_eq!(chat_models(&j), Some(vec!["qwen3:8b".to_string()]));
        assert_eq!(chat_models(&serde_json::json!({"hello":"world"})), None, "not a model list");
    }

    /// One real HTTP round trip against a fake server on an ephemeral port; a closed port just yields nothing.
    #[tokio::test]
    async fn probes_servers_over_http() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let body = r#"{"data":[{"id":"local-model"}]}"#;
            let _ = sock.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
        });
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let net = Net::new();
        // Offline mode must not stop loopback probes.
        net.set_offline(true);
        let found = scan_servers(&net, &[(port, "Test"), (closed, "Closed")]).await;
        assert_eq!(found, vec![Found::Server { runtime: "Test", base_url: format!("http://127.0.0.1:{port}/v1"), models: vec!["local-model".into()] }]);
    }
}
