use std::io::Read;
use std::path::Path;

use crate::i18n::t;

const MAX_BYTES: u64 = 15 * 1024 * 1024;

/// I extract text locally only: nothing leaves the machine during import.
pub fn extract_text(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_BYTES {
        return Err(t("Файл більший за 15 МБ", "File is larger than 15 MB").into());
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    match ext.as_str() {
        "txt" | "md" | "markdown" => {
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            Ok(String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}').to_string())
        }
        "docx" => extract_docx(path),
        "pdf" => extract_pdf(&std::fs::read(path).map_err(|e| e.to_string())?),
        _ => Err(crate::tr!(
            "Формат {} не підтримується — використовуйте PDF, DOCX, TXT або MD",
            "Format {} is not supported — use PDF, DOCX, TXT or MD",
            if ext.is_empty() { t("без розширення", "without extension").to_string() } else { format!(".{ext}") }
        )),
    }
}

fn extract_docx(path: &Path) -> Result<String, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    let mut xml = String::new();
    zip.by_name("word/document.xml").map_err(|e| e.to_string())?.read_to_string(&mut xml).map_err(|e| e.to_string())?;
    Ok(docx_xml_to_text(&xml))
}

/// WordprocessingML → plain text: `<w:t>` runs, paragraph ends as newlines, tabs and breaks kept.
pub fn docx_xml_to_text(xml: &str) -> String {
    let mut out = String::new();
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        let Some(gt) = rest[lt..].find('>') else { break };
        let tag = &rest[lt + 1..lt + gt];
        rest = &rest[lt + gt + 1..];
        let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
        match (name, tag.starts_with('/')) {
            ("w:t", false) if !tag.ends_with('/') => {
                let end = rest.find("</w:t>").unwrap_or(rest.len());
                out.push_str(&unescape(&rest[..end]));
                rest = &rest[end..];
            }
            ("w:p", true) => out.push('\n'),
            ("w:tab", false) => out.push('\t'),
            ("w:br" | "w:cr", false) => out.push('\n'),
            _ => {}
        }
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn extract_pdf(data: &[u8]) -> Result<String, String> {
    // pdf-extract panics on some malformed files; a bad CV must not take the app down.
    let pages = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(data))
        .map_err(|_| t("Не вдалося прочитати PDF", "Could not read the PDF").to_string())?
        .map_err(|e| e.to_string())?;
    let label = t("Сторінка", "Page");
    Ok(pages
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let lines: Vec<&str> = p.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
            format!("## {label} {}\n{}", i + 1, lines.join("\n"))
        })
        .collect::<Vec<_>>()
        .join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
    }

    #[test]
    fn docx_and_pdf_fixtures_yield_text() {
        let docx = extract_text(&fixture("cv.docx")).unwrap();
        assert!(docx.trim().len() > 20, "{docx}");
        let pdf = extract_text(&fixture("cv.pdf")).unwrap();
        assert!(pdf.trim().len() > 20, "{pdf}");
    }

    #[test]
    fn docx_xml_parsing() {
        let xml = r#"<w:body><w:p><w:r><w:t xml:space="preserve">Hello &amp; </w:t></w:r><w:r><w:tab/><w:t>world</w:t></w:r></w:p><w:p><w:r><w:t>Next</w:t></w:r></w:p></w:body>"#;
        assert_eq!(docx_xml_to_text(xml), "Hello & \tworld\nNext\n");
    }
}
