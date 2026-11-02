//! The one-file edition. A release appends a zip of the sidecars (whisper-server, the base speech model, llama-server)
//! to `Copilot.exe`; a zip is read from its end, so the exe still runs and the same file still opens as an archive.
//! On start I unpack whatever is missing into the data folder. A plain exe has no zip and simply downloads on demand.

use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::Path;

use crate::types::Download;

const CHUNK: usize = 256 * 1024;

/// Returns true when a payload was present. Already-unpacked files of the same size are left alone, so a restart costs nothing.
pub fn unpack(archive: &Path, into: &Path, progress: &dyn Fn(Download)) -> Result<bool, String> {
    let f = File::open(archive).map_err(|e| e.to_string())?;
    let Ok(mut z) = zip::ZipArchive::new(BufReader::new(f)) else { return Ok(false) };
    let mut todo = Vec::new();
    let mut total = 0u64;
    for i in 0..z.len() {
        let e = z.by_index(i).map_err(|e| e.to_string())?;
        // enclosed_name rejects absolute paths and `..` (zip-slip).
        let Some(rel) = e.enclosed_name().filter(|_| !e.is_dir()) else { continue };
        let up_to_date = std::fs::metadata(into.join(&rel)).is_ok_and(|m| m.len() == e.size());
        if !up_to_date {
            total += e.size();
            todo.push((i, rel));
        }
    }
    let name = crate::i18n::t("вбудовані компоненти", "built-in components");
    let mut done = 0u64;
    let mut buf = vec![0u8; CHUNK];
    for (i, rel) in todo {
        let mut e = z.by_index(i).map_err(|e| e.to_string())?;
        let dest = into.join(&rel);
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        // Written beside and renamed, so a crash never leaves a truncated file that looks installed.
        let tmp = dest.with_extension("unpack");
        let mut w = File::create(&tmp).map_err(|e| e.to_string())?;
        loop {
            let n = e.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            w.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            done += n as u64;
            progress(Download { name: name.into(), received: done, total });
        }
        drop(w);
        std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use zip::write::SimpleFileOptions;

    use super::*;

    /// The packaging script has its own copy of the pins, because it runs before there is an app.
    #[test]
    fn pins_match_the_app() {
        let script = include_str!("../scripts/package.py");
        assert!(script.contains(crate::discover::LLAMA_ZIP.0) && script.contains(crate::discover::LLAMA_ZIP.1));
        assert!(script.contains(crate::asr::assets::RELEASE));
        assert!(script.contains(crate::asr::assets::MODELS[0].sha256));
        assert!(script.contains(crate::asr::assets::CPU_BINARY_SHA256));
    }

    #[test]
    fn unpacks_a_zip_appended_to_an_exe_and_skips_what_is_there() {
        let dir = std::env::temp_dir().join(format!("copilot-bundle-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("Copilot.exe");
        let mut payload = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut payload);
            let o = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            w.start_file("asr/models/ggml-base.bin", o).unwrap();
            w.write_all(&[7u8; 300_000]).unwrap();
            w.start_file("llama/llama-server.exe", o).unwrap();
            w.write_all(b"server").unwrap();
            w.finish().unwrap();
        }
        let mut bytes = vec![b'M'; 40_000];
        bytes.extend_from_slice(payload.get_ref());
        std::fs::write(&exe, bytes).unwrap();

        let out = dir.join("data");
        let seen = std::sync::Mutex::new(0u64);
        assert!(unpack(&exe, &out, &|d| *seen.lock().unwrap() = d.received).unwrap());
        assert_eq!(std::fs::read(out.join("asr/models/ggml-base.bin")).unwrap().len(), 300_000);
        assert_eq!(std::fs::read(out.join("llama/llama-server.exe")).unwrap(), b"server");
        assert_eq!(*seen.lock().unwrap(), 300_006);

        // A second run writes nothing.
        *seen.lock().unwrap() = 0;
        assert!(unpack(&exe, &out, &|d| *seen.lock().unwrap() = d.received).unwrap());
        assert_eq!(*seen.lock().unwrap(), 0);

        // An exe without a payload is not an error.
        let plain = dir.join("plain.exe");
        std::fs::write(&plain, vec![b'M'; 70_000]).unwrap();
        assert!(!unpack(&plain, &out, &|_| {}).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
