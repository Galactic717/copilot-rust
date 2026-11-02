//! Screen capture of one monitor (GDI) and local OCR via the built-in Windows OCR engine (Windows.Media.Ocr):
//! nothing to download, works offline, languages come from Windows language packs.

use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::Win32::Graphics::Gdi::*;

use crate::i18n::t;
use crate::win::Rect;

pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Grabs `r` (physical px) from the desktop. Windows excluded from capture (WDA_EXCLUDEFROMCAPTURE) stay out of it.
pub fn capture(r: Rect) -> Result<Shot, String> {
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return Err("GetDC".into());
        }
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, r.w, r.h);
        let old = SelectObject(mem, bmp.into());
        let ok = BitBlt(mem, 0, 0, r.w, r.h, Some(screen), r.x, r.y, SRCCOPY | CAPTUREBLT).is_ok();
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: r.w,
                biHeight: -r.h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = vec![0u8; (r.w * r.h * 4) as usize];
        let lines = GetDIBits(mem, bmp, 0, r.h as u32, Some(buf.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if !ok || lines == 0 {
            return Err(t("Не вдалося отримати зображення екрана", "Could not capture the screen").into());
        }
        for px in buf.as_chunks_mut::<4>().0 {
            px.swap(0, 2); // BGRA → RGBA
            px[3] = 255;
        }
        Ok(Shot { width: r.w as u32, height: r.h as u32, rgba: buf })
    }
}

fn engine(tag: &str) -> Option<OcrEngine> {
    let lang = Language::CreateLanguage(&HSTRING::from(tag)).ok()?;
    if !OcrEngine::IsLanguageSupported(&lang).ok()? {
        return None;
    }
    OcrEngine::TryCreateFromLanguage(&lang).ok()
}

/// Which of the app's languages the Windows OCR engine can read on this machine.
pub fn ocr_languages() -> Vec<&'static str> {
    with_winrt(|| ["uk", "en-US"].into_iter().filter(|l| engine(l).is_some()).map(|l| if l == "uk" { "uk" } else { "en" }).collect())
}

pub fn ocr_installed() -> bool {
    !ocr_languages().is_empty()
}

/// WinRT needs an MTA thread; the UI thread is STA (OLE drag-and-drop), so every call runs on its own thread.
/// The MTA must outlive those threads: windows-rs caches activation factories, and if the apartment dies with the thread
/// the next call dereferences a dead factory and the whole process goes down with an access violation.
fn with_winrt<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    static MTA: std::sync::Once = std::sync::Once::new();
    MTA.call_once(|| {
        // The cookie is dropped on purpose: the apartment stays up until the process exits.
        let _ = unsafe { windows::Win32::System::Com::CoIncrementMTAUsage() };
    });
    std::thread::scope(|s| {
        s.spawn(|| {
            unsafe {
                let _ = windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED);
            }
            f()
        })
        .join()
        .expect("winrt thread")
    })
}

/// OCR of an RGBA image. With both packs installed I keep the Ukrainian result when it is mostly Cyrillic.
pub fn recognize(rgba: &[u8], width: u32, height: u32) -> Result<String, String> {
    with_winrt(|| {
        let max = OcrEngine::MaxImageDimension().unwrap_or(10_000);
        if width > max || height > max {
            return Err(t("Область завелика для OCR", "Area is too large for OCR").into());
        }
        let mut bgra = rgba.to_vec();
        for px in bgra.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        let bitmap = (|| -> windows::core::Result<SoftwareBitmap> {
            let w = DataWriter::new()?;
            w.WriteBytes(&bgra)?;
            let buf = w.DetachBuffer()?;
            SoftwareBitmap::CreateCopyWithAlphaFromBuffer(&buf, BitmapPixelFormat::Bgra8, width as i32, height as i32, BitmapAlphaMode::Premultiplied)
        })()
        .map_err(|e| e.to_string())?;
        let run = |e: &OcrEngine| -> Option<String> { e.RecognizeAsync(&bitmap).ok()?.join().ok()?.Text().ok().map(|s| s.to_string()) };
        let uk = engine("uk").and_then(|e| run(&e));
        let en = engine("en-US").and_then(|e| run(&e));
        match (uk, en) {
            (Some(u), Some(e)) => {
                let letters = u.chars().filter(|c| c.is_alphabetic()).count().max(1);
                let cyr = u.chars().filter(|c| ('\u{0400}'..='\u{04FF}').contains(c)).count();
                Ok(if cyr * 5 >= letters { u } else { e })
            }
            (Some(u), None) => Ok(u),
            (None, Some(e)) => Ok(e),
            (None, None) => Err(t(
                "OCR недоступний — додайте мовний пакет (українська або англійська) з розпізнаванням тексту в параметрах Windows",
                "OCR unavailable — add a Windows language pack (Ukrainian or English) with optical character recognition",
            )
            .into()),
        }
    })
}

/// Crops the picker selection, greys out blurred areas, scales to ≤1600 px wide and encodes PNG.
pub fn crop_png(shot: &Shot, sel: Rect, blurs: &[Rect]) -> Result<(Vec<u8>, image::RgbaImage), String> {
    let img = image::RgbaImage::from_raw(shot.width, shot.height, shot.rgba.clone()).ok_or("bad image")?;
    let x = sel.x.clamp(0, shot.width as i32 - 1) as u32;
    let y = sel.y.clamp(0, shot.height as i32 - 1) as u32;
    let w = (sel.w as u32).min(shot.width - x).max(1);
    let h = (sel.h as u32).min(shot.height - y).max(1);
    let mut crop = image::imageops::crop_imm(&img, x, y, w, h).to_image();
    for b in blurs {
        for py in b.y.max(sel.y)..(b.y + b.h).min(sel.y + h as i32) {
            for px in b.x.max(sel.x)..(b.x + b.w).min(sel.x + w as i32) {
                crop.put_pixel((px - sel.x) as u32, (py - sel.y) as u32, image::Rgba([0x7a, 0x7a, 0x7a, 255]));
            }
        }
    }
    if crop.width() > 1600 {
        let nh = (crop.height() as f64 * 1600.0 / crop.width() as f64).round() as u32;
        crop = image::imageops::resize(&crop, 1600, nh.max(1), image::imageops::FilterType::Lanczos3);
    }
    let mut out = std::io::Cursor::new(Vec::new());
    crop.write_to(&mut out, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok((out.into_inner(), crop))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Used to crash the process on the second call.
    #[test]
    fn ocr_can_be_asked_more_than_once() {
        let first = ocr_languages();
        assert_eq!(ocr_languages(), first);
        assert_eq!(ocr_installed(), !first.is_empty());
    }
}
