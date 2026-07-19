//! Text extraction from resume files. Pure/offline: PDF via `pdf-extract`,
//! DOCX via `zip` + `quick-xml`, DOC/RTF via macOS `textutil`, TXT direct.
//! PDFs that yield little/no text are flagged for the Gemini vision fallback.

use anyhow::{Context, Result, anyhow};
use std::io::Read;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Pdf,
    Docx,
    Doc,
    Rtf,
    Txt,
}

/// Result of extracting one resume file.
pub struct Extracted {
    pub text: String,
    pub kind: SourceKind,
    /// True when a PDF produced too little text (likely scanned/image-only) and
    /// should be sent to Gemini's native PDF vision instead.
    pub needs_vision: bool,
    /// Original PDF bytes, retained only when `needs_vision` is set.
    pub pdf_bytes: Option<Vec<u8>>,
}

/// File extensions we attempt to parse.
pub const SUPPORTED_EXTS: [&str; 6] = ["pdf", "docx", "doc", "rtf", "txt", "md"];

pub fn is_supported(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(e) => SUPPORTED_EXTS.contains(&e.to_ascii_lowercase().as_str()),
        None => false,
    }
}

/// Extract text from a resume file. `min_text_chars` is the threshold below
/// which a PDF is considered scanned.
pub fn extract(path: &Path, min_text_chars: usize) -> Result<Extracted> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "pdf" => extract_pdf(path, min_text_chars),
        "docx" => Ok(plain(extract_docx(path)?, SourceKind::Docx)),
        "doc" => Ok(plain(textutil(path)?, SourceKind::Doc)),
        "rtf" => Ok(plain(textutil(path)?, SourceKind::Rtf)),
        "txt" | "md" | "text" => Ok(plain(
            std::fs::read_to_string(path).context("reading text file")?,
            SourceKind::Txt,
        )),
        other => Err(anyhow!("unsupported file type: .{other}")),
    }
}

fn plain(text: String, kind: SourceKind) -> Extracted {
    Extracted {
        text,
        kind,
        needs_vision: false,
        pdf_bytes: None,
    }
}

fn extract_pdf(path: &Path, min_text_chars: usize) -> Result<Extracted> {
    let bytes = std::fs::read(path).context("reading pdf")?;
    // pdf-extract can both error AND panic on unusual PDFs (e.g. malformed
    // unicode maps). Contain both so one bad file can't kill the batch; an empty
    // result routes the PDF to the Gemini vision fallback.
    let text = extract_pdf_text_safe(&bytes);
    let needs_vision = text.trim().chars().count() < min_text_chars;
    Ok(Extracted {
        text,
        kind: SourceKind::Pdf,
        needs_vision,
        pdf_bytes: if needs_vision { Some(bytes) } else { None },
    })
}

/// Extract PDF text, containing any panic from the parser.
fn extract_pdf_text_safe(bytes: &[u8]) -> String {
    let bytes = bytes.to_vec();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        pdf_extract::extract_text_from_mem(&bytes).unwrap_or_default()
    }))
    .unwrap_or_default()
}

fn extract_docx(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path).context("opening docx")?;
    let mut archive = zip::ZipArchive::new(file).context("reading docx zip")?;
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")
        .context("docx missing word/document.xml")?
        .read_to_string(&mut xml)?;
    Ok(docx_xml_to_text(&xml))
}

/// Pull readable text out of a WordprocessingML document body, inserting
/// newlines at paragraph boundaries and tabs for tab elements.
fn docx_xml_to_text(xml: &str) -> String {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    let mut reader = Reader::from_reader(xml.as_bytes());
    let mut out = String::new();
    let mut buf = Vec::new();
    let mut in_text = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if e.name().as_ref() == b"w:t" {
                    in_text = true;
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"w:t" => in_text = false,
                b"w:p" => out.push('\n'),
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"w:br" | b"w:cr" => out.push('\n'),
                b"w:tab" => out.push('\t'),
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if in_text {
                    // 0.41: decode bytes→str, then unescape XML entities separately.
                    if let Ok(decoded) = t.decode() {
                        match quick_xml::escape::unescape(&decoded) {
                            Ok(u) => out.push_str(&u),
                            Err(_) => out.push_str(&decoded),
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// Convert legacy .doc / .rtf using the macOS built-in `textutil`.
fn textutil(path: &Path) -> Result<String> {
    let out = Command::new("textutil")
        .args(["-convert", "txt", "-stdout"])
        .arg(path)
        .output()
        .context("running textutil (macOS only)")?;
    if !out.status.success() {
        return Err(anyhow!(
            "textutil failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}
