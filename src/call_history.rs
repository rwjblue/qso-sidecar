use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::naqp;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallHistoryEntry {
    pub call: String,
    pub name: Option<String>,
    pub location: Option<String>,
    pub imported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportDiagnostics {
    pub rows_seen: usize,
    pub imported: usize,
    pub skipped: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Import {
    pub entries: BTreeMap<String, CallHistoryEntry>,
    pub diagnostics: ImportDiagnostics,
}

pub fn parse(bytes: &[u8], imported_at: DateTime<Utc>) -> Result<Import> {
    let text = std::str::from_utf8(bytes)
        .context("call history must be UTF-8 text")?
        .trim_start_matches('\u{feff}');
    let mut import = Import::default();
    let mut columns = None;
    let mut delimiter = ',';

    for (line_index, raw_line) in text.lines().enumerate() {
        let line_number = line_index + 1;
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }

        if line
            .get(.."!!Order!!".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("!!Order!!"))
        {
            delimiter = if line.matches(';').count() > line.matches(',').count() {
                ';'
            } else {
                ','
            };
            let parsed = split_fields(line, delimiter)
                .with_context(|| format!("line {line_number}: malformed !!Order!! header"))?;
            let mapped: Vec<_> = parsed
                .into_iter()
                .skip(1)
                .map(|field| field.trim().to_ascii_lowercase())
                .collect();
            if !mapped.iter().any(|field| field == "call") {
                bail!("line {line_number}: !!Order!! header must include a Call column");
            }
            columns = Some(mapped);
            continue;
        }

        if line.starts_with("!!") {
            continue;
        }

        let Some(columns) = columns.as_ref() else {
            bail!("line {line_number}: data appears before an !!Order!! header");
        };
        import.diagnostics.rows_seen += 1;
        let fields = match split_fields(line, delimiter) {
            Ok(fields) => fields,
            Err(error) => {
                import.diagnostics.skipped += 1;
                warn(
                    &mut import.diagnostics,
                    format!("Line {line_number}: malformed row: {error:#}"),
                );
                continue;
            }
        };
        if fields.len() != columns.len() {
            warn(
                &mut import.diagnostics,
                format!(
                    "Line {line_number}: expected {} fields from !!Order!! but found {}",
                    columns.len(),
                    fields.len()
                ),
            );
        }

        let value = |name: &str| {
            columns
                .iter()
                .position(|column| column == name)
                .and_then(|index| fields.get(index))
                .map(|value| value.trim())
        };
        let Some(call) = value("call").and_then(normalize_call) else {
            import.diagnostics.skipped += 1;
            warn(
                &mut import.diagnostics,
                format!("Line {line_number}: missing or invalid callsign"),
            );
            continue;
        };
        let name = value("name")
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let raw_location = value("state").filter(|value| !value.is_empty());
        let location = raw_location.and_then(normalize_location);
        if let (Some(raw_location), None) = (raw_location, &location) {
            warn(
                &mut import.diagnostics,
                format!(
                    "Line {line_number}: unrecognized NAQP State value {:?}; imported without a location prediction",
                    raw_location
                ),
            );
        }
        let entry = CallHistoryEntry {
            call: call.clone(),
            name,
            location,
            imported_at,
        };
        if import.entries.insert(call.clone(), entry).is_some() {
            warn(
                &mut import.diagnostics,
                format!("Line {line_number}: duplicate {call}; the later row was used"),
            );
        }
        import.diagnostics.imported += 1;
    }

    if columns.is_none() {
        bail!("missing !!Order!! header");
    }
    Ok(import)
}

fn normalize_call(value: &str) -> Option<String> {
    let call = value.trim().to_ascii_uppercase();
    let valid = (3..=16).contains(&call.len())
        && call
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '/')
        && call
            .chars()
            .any(|character| character.is_ascii_alphabetic())
        && call.chars().any(|character| character.is_ascii_digit());
    valid.then_some(call)
}

fn normalize_location(value: &str) -> Option<String> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("DX") {
        Some("DX".into())
    } else {
        naqp::normalize_multiplier(value)
    }
}

fn split_fields(line: &str, delimiter: char) -> Result<Vec<String>> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut characters = line.chars().peekable();
    let mut quoted = false;
    while let Some(character) = characters.next() {
        match character {
            '"' if quoted && characters.peek() == Some(&'"') => {
                field.push('"');
                characters.next();
            }
            '"' => quoted = !quoted,
            character if character == delimiter && !quoted => {
                fields.push(std::mem::take(&mut field));
            }
            character => field.push(character),
        }
    }
    if quoted {
        bail!("unterminated quoted field");
    }
    fields.push(field);
    Ok(fields)
}

fn warn(diagnostics: &mut ImportDiagnostics, warning: String) {
    if diagnostics.warnings.len() < 50 {
        diagnostics.warnings.push(warning);
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn imported_at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 1, 17, 0, 0).unwrap()
    }

    #[test]
    fn imports_reordered_named_columns_and_reports_bad_rows() {
        let import = parse(
            include_bytes!("../tests/fixtures/n1mm-naqp-call-history.txt"),
            imported_at(),
        )
        .unwrap();

        assert_eq!(import.diagnostics.rows_seen, 4);
        assert_eq!(import.diagnostics.imported, 3);
        assert_eq!(import.diagnostics.skipped, 1);
        assert!(import.diagnostics.warnings.len() >= 2);
        assert_eq!(import.entries.len(), 3);
        assert_eq!(import.entries["W1AW"].name.as_deref(), Some("Hiram"));
        assert_eq!(import.entries["W1AW"].location.as_deref(), Some("CT"));
        assert_eq!(import.entries["VE3EJ"].location.as_deref(), Some("ON"));
        assert_eq!(import.entries["DL1ABC"].location.as_deref(), Some("DX"));
    }

    #[test]
    fn accepts_semicolon_delimiters_and_case_insensitive_headers() {
        let import = parse(
            b"!!order!!;STATE;NAME;CALL;IGNORED\nca;Ken;n6ro;x\n",
            imported_at(),
        )
        .unwrap();
        assert_eq!(import.entries["N6RO"].location.as_deref(), Some("CA"));
    }

    #[test]
    fn requires_a_named_call_column() {
        let error = parse(b"!!Order!!,Name,State\nAl,CT\n", imported_at()).unwrap_err();
        assert!(error.to_string().contains("Call column"));
    }
}
