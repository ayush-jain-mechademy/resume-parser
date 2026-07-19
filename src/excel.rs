//! Output writers: a formatted, trust-first Excel workbook plus CSV and JSON
//! dumps of the same records. Low/medium-confidence cells are colour-coded and
//! the supporting evidence is attached as cell notes.

use crate::schema::{CandidateRecord, Confidence, RowStatus};
use anyhow::{Context, Result, anyhow};
use rust_xlsxwriter::{Color, Format, Note, Workbook};
use std::path::{Path, PathBuf};

const HEADERS: [&str; 17] = [
    "Status",
    "Source File",
    "Name",
    "WhatsApp/Phone",
    "Email",
    "Currently Working At",
    "Currently Employed?",
    "Gap?",
    "Gap Duration",
    "Gap (months)",
    "Years of Experience",
    "Fresher/Experienced",
    "Primary Role",
    "Secondary Role",
    "Key Skills",
    "Confidence",
    "Review Reason",
];

/// Column widths (chars), index-aligned with HEADERS.
const WIDTHS: [f64; 17] = [
    11.0, 22.0, 20.0, 16.0, 26.0, 22.0, 10.0, 7.0, 12.0, 9.0, 10.0, 12.0, 16.0, 14.0, 34.0, 10.0,
    40.0,
];

/// Map a data column index to the field-confidence key it should be shaded by.
fn conf_key(col: usize) -> Option<&'static str> {
    match HEADERS[col] {
        "Name" => Some("Name"),
        "WhatsApp/Phone" => Some("WhatsApp/Phone"),
        "Email" => Some("Email"),
        "Currently Working At" => Some("Currently Working At"),
        "Currently Employed?" => Some("Currently Employed?"),
        "Gap?" => Some("Gap?"),
        "Years of Experience" => Some("Years of Experience"),
        "Fresher/Experienced" => Some("Fresher/Experienced"),
        "Primary Role" => Some("Primary Role"),
        _ => None,
    }
}

/// Map a data column to the evidence key whose note should be attached.
fn evidence_key(col: usize) -> Option<&'static str> {
    match HEADERS[col] {
        "Name" => Some("Name"),
        "Currently Working At" => Some("Currently Working At"),
        "Primary Role" => Some("Primary Role"),
        _ => None,
    }
}

/// Write all three outputs. Each is written independently and atomically, so a
/// failure in one format still produces the others, and an interrupted run never
/// leaves a half-written file in place.
pub fn write_all(
    records: &[CandidateRecord],
    xlsx: &Path,
    csv: &Path,
    json: &Path,
) -> Result<()> {
    let mut errs = Vec::new();
    if let Err(e) = write_xlsx(records, xlsx) {
        errs.push(format!("xlsx: {e:#}"));
    }
    if let Err(e) = write_csv(records, csv) {
        errs.push(format!("csv: {e:#}"));
    }
    if let Err(e) = write_json(records, json) {
        errs.push(format!("json: {e:#}"));
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("output write issues — {}", errs.join("; ")))
    }
}

/// Path with a `.tmp` suffix for atomic write-then-rename.
fn tmp_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

fn write_json(records: &[CandidateRecord], path: &Path) -> Result<()> {
    let tmp = tmp_path(path);
    let f = std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    serde_json::to_writer_pretty(&f, records)?;
    f.sync_all().ok();
    std::fs::rename(&tmp, path).with_context(|| format!("finalizing {}", path.display()))?;
    Ok(())
}

fn write_xlsx(records: &[CandidateRecord], path: &Path) -> Result<()> {
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();

    let header = Format::new()
        .set_bold()
        .set_font_color(Color::White)
        .set_background_color(Color::RGB(0x1F4E78));
    let low = Format::new()
        .set_background_color(Color::RGB(0xFFC7CE))
        .set_font_color(Color::RGB(0x9C0006));
    let medium = Format::new()
        .set_background_color(Color::RGB(0xFFEB9C))
        .set_font_color(Color::RGB(0x9C6500));
    let review = Format::new()
        .set_background_color(Color::RGB(0xFFF2CC))
        .set_bold();
    let verified = Format::new().set_font_color(Color::RGB(0x006100));
    let wrap = Format::new().set_text_wrap();

    // header row
    for (c, h) in HEADERS.iter().enumerate() {
        ws.write_with_format(0, c as u16, *h, &header)?;
        ws.set_column_width(c as u16, WIDTHS[c])?;
    }
    ws.set_freeze_panes(1, 0)?;
    ws.autofilter(0, 0, records.len() as u32, (HEADERS.len() - 1) as u16)?;

    for (i, rec) in records.iter().enumerate() {
        let row = (i + 1) as u32;
        let cells = row_values(rec);
        for (c, val) in cells.iter().enumerate() {
            let col = c as u16;

            // choose a format: confidence shading wins on key fields
            let shade = conf_key(c).and_then(|k| rec.field_confidence.get(k)).and_then(|conf| {
                match conf {
                    Confidence::Low => Some(&low),
                    Confidence::Medium => Some(&medium),
                    Confidence::High => None,
                }
            });

            match (HEADERS[c], shade) {
                ("Status", _) => {
                    let fmt = if rec.status == RowStatus::AutoVerified {
                        &verified
                    } else {
                        &review
                    };
                    ws.write_with_format(row, col, val, fmt)?;
                }
                ("Key Skills" | "Review Reason", _) => {
                    ws.write_with_format(row, col, val, &wrap)?;
                }
                (_, Some(fmt)) => ws.write_with_format(row, col, val, fmt).map(|_| ())?,
                (_, None) => ws.write(row, col, val).map(|_| ())?,
            }

            // attach evidence note (best-effort: never fail the export over a note)
            if let Some(ek) = evidence_key(c) {
                if let Some(ev) = rec.evidence.get(ek) {
                    if !ev.is_empty() {
                        let _ = ws.insert_note(row, col, &Note::new(ev).add_author_prefix(false));
                    }
                }
            }
        }
    }

    let tmp = tmp_path(path);
    wb.save(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("finalizing {}", path.display()))?;
    Ok(())
}

fn row_values(rec: &CandidateRecord) -> Vec<String> {
    vec![
        rec.status.label().to_string(),
        rec.source_file.clone(),
        rec.name.clone(),
        rec.whatsapp.clone(),
        rec.email.clone(),
        rec.current_company.clone(),
        yesno(rec.currently_employed),
        yesno(rec.has_gap),
        rec.gap_duration.clone(),
        rec.gap_months.to_string(),
        format!("{:.1}", rec.years_experience),
        rec.experience_level.label().to_string(),
        rec.primary_role.label().to_string(),
        rec.secondary_role.map(|r| r.label().to_string()).unwrap_or_default(),
        rec.key_skills.join(", "),
        rec.overall_confidence.label().to_string(),
        rec.review_reasons.join("; "),
    ]
}

fn yesno(b: bool) -> String {
    if b { "Yes" } else { "No" }.to_string()
}

fn write_csv(records: &[CandidateRecord], path: &Path) -> Result<()> {
    let mut out = String::new();
    out.push_str(&HEADERS.map(csv_escape).join(","));
    out.push('\n');
    for rec in records {
        let line = row_values(rec)
            .iter()
            .map(|s| csv_escape(s))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&line);
        out.push('\n');
    }
    let tmp = tmp_path(path);
    std::fs::write(&tmp, out).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("finalizing {}", path.display()))?;
    Ok(())
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}
