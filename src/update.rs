//! Update check against GitHub Releases and a self-replace for the single exe. The check is one GET with no identifying
//! data; offline mode blocks it like any other external call.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::i18n::t;
use crate::net::Net;
use crate::tr;
use crate::types::Download;

pub const REPO: &str = "Galactic717/copilot-rust";
/// Tells the new process to wait for the old one to release the single-instance lock.
pub const RELAUNCH_ARG: &str = "--relaunch";

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub page: String,
}

/// `1.10.0` > `1.9.3`. A pre-release tag on either side is never an update: those are for people who picked them by hand.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| -> Option<Vec<u32>> { v.trim_start_matches('v').split('.').map(|p| p.parse().ok()).collect() };
    match (parts(candidate), parts(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// The small exe is what an update fetches: the speech model and the servers are already unpacked on this machine.
pub fn parse_release(j: &Value, current: &str) -> Option<Release> {
    if j.get("prerelease").and_then(Value::as_bool) == Some(true) || j.get("draft").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let tag = j.get("tag_name")?.as_str()?;
    if !is_newer(tag, current) {
        return None;
    }
    let asset = j.get("assets")?.as_array()?.iter().find(|a| a.get("name").and_then(Value::as_str).is_some_and(|n| n.ends_with("-lite.exe")))?;
    let sha256 = asset.get("digest")?.as_str()?.strip_prefix("sha256:")?.to_string();
    Some(Release {
        version: tag.trim_start_matches('v').to_string(),
        url: asset.get("browser_download_url")?.as_str()?.to_string(),
        sha256,
        page: j.get("html_url").and_then(Value::as_str).unwrap_or("").to_string(),
    })
}

pub async fn latest(net: &Net) -> Result<Option<Release>, String> {
    latest_after(net, env!("CARGO_PKG_VERSION")).await
}

async fn latest_after(net: &Net, current: &str) -> Result<Option<Release>, String> {
    let res = net
        .get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .map_err(|e| e.line())?
        .header("Accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| crate::net::transport_error(&e).line())?;
    if res.status().as_u16() == 404 {
        return Ok(None);
    }
    if !res.status().is_success() {
        return Err(format!("GitHub HTTP {}", res.status().as_u16()));
    }
    let j: Value = res.json().await.map_err(|e| e.to_string())?;
    Ok(parse_release(&j, current))
}

fn old_path(exe: &Path) -> PathBuf {
    exe.with_extension("old.exe")
}

/// A running exe cannot be overwritten but can be renamed, so: new file beside it, swap, and the old one goes on the next start.
pub async fn install(net: &Net, rel: &Release, progress: &(dyn Fn(Download) + Send + Sync)) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    install_over(net, rel, &exe, progress).await?;
    Ok(exe)
}

async fn install_over(net: &Net, rel: &Release, exe: &Path, progress: &(dyn Fn(Download) + Send + Sync)) -> Result<(), String> {
    let part = exe.with_extension("update.part");
    let sha = crate::asr::assets::download(net, &rel.url, &part, &tr!("Copilot {}", "Copilot {}", rel.version), progress).await?;
    if sha != rel.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(t("Контрольна сума оновлення не збіглася — файл видалено", "Update checksum mismatch — file deleted").into());
    }
    swap(exe, &part)
}

fn swap(exe: &Path, new_file: &Path) -> Result<(), String> {
    let old = old_path(exe);
    let _ = std::fs::remove_file(&old);
    let denied = |e: std::io::Error| tr!("Не вдалося замінити файл застосунку ({}). Перемістіть Copilot у теку, куди дозволено запис.", "Could not replace the app file ({}). Move Copilot to a folder you can write to.", e);
    std::fs::rename(exe, &old).map_err(denied)?;
    if let Err(e) = std::fs::rename(new_file, exe) {
        // The app must never be left without its exe.
        let _ = std::fs::rename(&old, exe);
        return Err(denied(e));
    }
    Ok(())
}

/// Called on every start: the renamed exe of the previous version can be deleted once that process is gone.
pub fn clean_old() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_path(&exe));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn version_order() {
        assert!(is_newer("v1.10.0", "1.9.3"));
        assert!(is_newer("0.4.1", "0.4.0"));
        assert!(!is_newer("v0.4.0", "0.4.0"));
        assert!(!is_newer("0.3.9", "0.4.0"));
        assert!(!is_newer("1.0.0-rc1", "0.4.0"), "pre-release tags are never offered");
        assert!(!is_newer("nightly", "0.4.0"));
    }

    #[test]
    fn picks_the_small_exe_and_requires_a_digest() {
        let rel = |digest: Value| json!({
            "tag_name": "v9.0.0", "html_url": "https://github.com/x/y/releases/v9.0.0", "prerelease": false,
            "assets": [
                {"name":"Copilot-9.0.0-windows-x64.exe","browser_download_url":"https://x/full.exe","digest":"sha256:full"},
                {"name":"Copilot-9.0.0-windows-x64-lite.exe","browser_download_url":"https://x/lite.exe","digest":digest},
            ]
        });
        let r = parse_release(&rel(json!("sha256:abc")), "0.4.0").unwrap();
        assert_eq!((r.version.as_str(), r.url.as_str(), r.sha256.as_str()), ("9.0.0", "https://x/lite.exe", "abc"));
        assert_eq!(parse_release(&rel(Value::Null), "0.4.0"), None, "no digest, no update");
        assert_eq!(parse_release(&rel(json!("sha256:abc")), "9.0.0"), None, "same version");
        let mut pre = rel(json!("sha256:abc"));
        pre["prerelease"] = json!(true);
        assert_eq!(parse_release(&pre, "0.4.0"), None);
    }

    /// Live check against the real latest release: `cargo test live_update -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn live_update_from_github() {
        let net = Net::new();
        let rel = latest_after(&net, "0.0.1").await.expect("GitHub answers").expect("a release exists");
        println!("{rel:?}");
        let d = std::env::temp_dir().join(format!("copilot-upd-live-{}", crate::types::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        let exe = d.join("Copilot.exe");
        std::fs::write(&exe, b"the old version").unwrap();
        install_over(&net, &rel, &exe, &|_| {}).await.expect("downloads, verifies and swaps");
        assert_eq!(std::fs::read(old_path(&exe)).unwrap(), b"the old version");
        let new = std::fs::read(&exe).unwrap();
        assert_eq!(&new[..2], b"MZ", "the new file is an executable");
        println!("{} MB", new.len() / 1_048_576);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn swap_keeps_the_exe_when_the_new_file_cannot_move() {
        let d = std::env::temp_dir().join(format!("copilot-upd-{}", crate::types::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        let exe = d.join("Copilot.exe");
        std::fs::write(&exe, b"old").unwrap();
        // The new file does not exist, so the second rename fails: the old exe must come back.
        assert!(swap(&exe, &d.join("missing.part")).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"old");
        std::fs::write(d.join("new.part"), b"new").unwrap();
        swap(&exe, &d.join("new.part")).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(old_path(&exe)).unwrap(), b"old");
        let _ = std::fs::remove_dir_all(&d);
    }
}
