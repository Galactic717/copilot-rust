use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::i18n::t;
use crate::net::Net;
use crate::types::{AsrBackend, Download};

pub struct ModelAsset {
    pub name: &'static str,
    pub file: &'static str,
    pub sha256: &'static str,
}

// Hashes measured in the original spike; a mismatch means a corrupted or substituted download.
pub const MODELS: &[ModelAsset] = &[
    ModelAsset { name: "base", file: "ggml-base.bin", sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe" },
    ModelAsset { name: "small", file: "ggml-small.bin", sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b" },
    ModelAsset { name: "turbo", file: "ggml-large-v3-turbo-q5_0.bin", sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2" },
];

pub fn model_label(name: &str) -> String {
    match name {
        "base" => t("Base — швидка, для CPU", "Base — fast, for CPU").into(),
        "small" => t("Small — точніша, повільніша на CPU", "Small — more accurate, slower on CPU").into(),
        "turbo" => t("Large v3 Turbo — найкраща точність, потрібна NVIDIA", "Large v3 Turbo — best accuracy, needs NVIDIA").into(),
        other => other.into(),
    }
}

const RELEASE: &str = "b5454";

struct Binary {
    url: String,
    sha256: &'static str,
}

// Digests copied from the GitHub release API for b5454 and pinned, so a failed API call never skips the check.
fn binary(b: AsrBackend) -> Binary {
    match b {
        AsrBackend::Cpu => Binary {
            url: format!("https://github.com/ggml-org/whisper.cpp/releases/download/{RELEASE}/whisper-bin-x64.zip"),
            sha256: "6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb",
        },
        AsrBackend::Cuda => Binary {
            url: format!("https://github.com/ggml-org/whisper.cpp/releases/download/{RELEASE}/whisper-bin-win-cuda-12.4.0-x64.zip"),
            sha256: "afef0b881c500958921c3f5523b50e59ee2ec9b6f5cbd25b324c51ed308a957a",
        },
    }
}

pub struct AsrAssets {
    root: PathBuf,
}

impl AsrAssets {
    pub fn new(root: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(root.join("models"));
        Self { root }
    }

    pub fn model_path(&self, name: &str) -> PathBuf {
        let file = MODELS.iter().find(|m| m.name == name).map(|m| m.file.to_string()).unwrap_or_else(|| format!("ggml-{name}.bin"));
        self.root.join("models").join(file)
    }

    pub fn has_model(&self, name: &str) -> bool {
        self.model_path(name).is_file()
    }

    pub fn server_path(&self, backend: AsrBackend) -> Option<PathBuf> {
        find_file(&self.root.join(backend.as_str()), "whisper-server.exe")
    }

    pub async fn download_model(&self, net: &Net, name: &str, progress: &(dyn Fn(Download) + Send + Sync)) -> Result<(), String> {
        let m = MODELS.iter().find(|m| m.name == name).ok_or_else(|| crate::tr!("Невідома модель {}", "Unknown model {}", name))?;
        let url = format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}", m.file);
        let tmp = self.model_path(name).with_extension("bin.part");
        let sha = download(net, &url, &tmp, &model_label(name), progress).await?;
        if sha != m.sha256 {
            let _ = std::fs::remove_file(&tmp);
            return Err(crate::tr!("Контрольна сума моделі не збіглася ({}…) — файл видалено", "Model checksum mismatch ({}…) — file deleted", &sha[..12]));
        }
        std::fs::rename(&tmp, self.model_path(name)).map_err(|e| e.to_string())
    }

    pub async fn download_binary(&self, net: &Net, backend: AsrBackend, progress: &(dyn Fn(Download) + Send + Sync)) -> Result<(), String> {
        let b = binary(backend);
        let zip = self.root.join(format!("{}.zip.part", backend.as_str()));
        let label = if backend == AsrBackend::Cuda { "whisper.cpp (NVIDIA CUDA)" } else { "whisper.cpp (CPU)" };
        let sha = download(net, &b.url, &zip, label, progress).await?;
        if sha != b.sha256 {
            let _ = std::fs::remove_file(&zip);
            return Err(crate::tr!("Контрольна сума архіву не збіглася ({}…) — файл видалено", "Archive checksum mismatch ({}…) — file deleted", &sha[..12]));
        }
        let dir = self.root.join(backend.as_str());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        unzip(&zip, &dir)?;
        let _ = std::fs::remove_file(&zip);
        if self.server_path(backend).is_none() {
            return Err(t("В архіві немає whisper-server.exe", "whisper-server.exe is missing from the archive").into());
        }
        Ok(())
    }
}

fn unzip(zip_path: &Path, dir: &Path) -> Result<(), String> {
    let f = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    for i in 0..z.len() {
        let mut entry = z.by_index(i).map_err(|e| e.to_string())?;
        // enclosed_name rejects absolute paths and `..` (zip-slip).
        let Some(rel) = entry.enclosed_name() else { continue };
        let out = dir.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        } else {
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
            }
            let mut w = std::fs::File::create(&out).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut w).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

async fn download(net: &Net, url: &str, dest: &Path, name: &str, progress: &(dyn Fn(Download) + Send + Sync)) -> Result<String, String> {
    let res = net.get(url).map_err(|e| e.line())?.send().await.map_err(|e| crate::net::transport_error(&e).line())?;
    if !res.status().is_success() {
        return Err(crate::tr!("Завантаження не вдалося: HTTP {}", "Download failed: HTTP {}", res.status().as_u16()));
    }
    let total = res.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(dest).await.map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut received = 0u64;
    let mut last = std::time::Instant::now();
    let mut body = res.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                drop(file);
                let _ = std::fs::remove_file(dest);
                return Err(crate::net::transport_error(&e).line());
            }
        };
        hash.update(&chunk);
        received += chunk.len() as u64;
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        if last.elapsed().as_millis() > 250 {
            last = std::time::Instant::now();
            progress(Download { name: name.into(), received, total });
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    progress(Download { name: name.into(), received, total: if total > 0 { total } else { received } });
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if entry.file_name().to_string_lossy().eq_ignore_ascii_case(name) {
            return Some(p);
        }
        if p.is_dir() {
            if let Some(r) = find_file(&p, name) {
                return Some(r);
            }
        }
    }
    None
}
