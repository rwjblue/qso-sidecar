use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::log_source::LogUpdate;
use crate::model::{Band, Qso};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportDiagnostics {
    pub records_seen: usize,
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub warnings: Vec<String>,
}

pub fn import_snapshot(
    bytes: &[u8],
    existing: &mut BTreeMap<String, Qso>,
) -> Result<ImportDiagnostics> {
    let records = parse_records(bytes)?;
    let mut diagnostics = ImportDiagnostics {
        records_seen: records.len(),
        ..ImportDiagnostics::default()
    };

    let mut qsos = Vec::new();
    for (index, record) in records.into_iter().enumerate() {
        match record_to_qso(record) {
            Ok(qso) => qsos.push(qso),
            Err(error) => {
                diagnostics.skipped += 1;
                if diagnostics.warnings.len() < 20 {
                    diagnostics
                        .warnings
                        .push(format!("Record {}: {error:#}", index + 1));
                }
            }
        }
    }
    let summary = LogUpdate::adif(qsos).apply(existing);
    diagnostics.added = summary.added;
    diagnostics.updated = summary.updated;
    diagnostics.unchanged = summary.unchanged;
    Ok(diagnostics)
}

fn parse_records(bytes: &[u8]) -> Result<Vec<HashMap<String, String>>> {
    let mut records = Vec::new();
    let mut record = HashMap::new();
    let mut cursor = 0;

    while let Some(relative) = bytes[cursor..].iter().position(|byte| *byte == b'<') {
        let start = cursor + relative;
        let Some(close_relative) = bytes[start..].iter().position(|byte| *byte == b'>') else {
            bail!("unterminated ADIF field at byte {start}");
        };
        let close = start + close_relative;
        let header = std::str::from_utf8(&bytes[start + 1..close])?.trim();
        cursor = close + 1;

        let mut parts = header.split(':');
        let tag = parts.next().unwrap_or_default().trim().to_ascii_uppercase();
        if tag == "EOR" {
            if !record.is_empty() {
                records.push(std::mem::take(&mut record));
            }
            continue;
        }
        if tag == "EOH" {
            record.clear();
            continue;
        }
        let Some(length) = parts.next() else {
            continue;
        };
        let length: usize = length.trim().parse().context("invalid ADIF field length")?;
        let end = cursor
            .checked_add(length)
            .filter(|end| *end <= bytes.len())
            .context("ADIF field extends beyond the input")?;
        let value = String::from_utf8_lossy(&bytes[cursor..end])
            .trim()
            .to_string();
        cursor = end;
        record.insert(tag, value);
    }
    if !record.is_empty() {
        records.push(record);
    }
    Ok(records)
}

fn record_to_qso(record: HashMap<String, String>) -> Result<Qso> {
    let call = required(&record, "CALL")?.trim().to_ascii_uppercase();
    let date = required(&record, "QSO_DATE")?;
    let time = required(&record, "TIME_ON")?;
    let timestamp = parse_timestamp(date, time)?;
    let frequency_khz = record
        .get("FREQ")
        .and_then(|value| value.parse::<f64>().ok())
        .map(|mhz| mhz * 1_000.0);
    let band = record
        .get("BAND")
        .and_then(|value| Band::from_str(value).ok())
        .or_else(|| frequency_khz.and_then(Band::from_frequency_khz));
    let mode = record
        .get("MODE")
        .cloned()
        .unwrap_or_else(|| "CW".into())
        .to_ascii_uppercase();

    let (srx_name, srx_location) = record
        .get("SRX_STRING")
        .map(|exchange| split_exchange(exchange))
        .unwrap_or_default();
    let name = record.get("NAME").cloned().or(srx_name);
    let location = record
        .get("STATE")
        .cloned()
        .filter(|value| !value.trim().is_empty())
        .or(srx_location);

    let mut hasher = Sha256::new();
    hasher.update(call.as_bytes());
    hasher.update(timestamp.timestamp_millis().to_le_bytes());
    hasher.update(band.map(Band::meters).unwrap_or_default().to_le_bytes());
    hasher.update(mode.as_bytes());
    let id = format!("adif:{:x}", hasher.finalize());
    let raw = Value::Object(
        record
            .into_iter()
            .map(|(key, value)| (key, Value::String(value)))
            .collect::<Map<_, _>>(),
    );

    Ok(Qso {
        id,
        call,
        timestamp,
        band,
        frequency_khz,
        mode,
        name,
        location,
        country: string_field(&raw, "COUNTRY"),
        dxcc: string_field(&raw, "DXCC").and_then(|value| value.parse().ok()),
        contest_id: string_field(&raw, "CONTEST_ID"),
        deleted: false,
        raw,
    })
}

fn required<'a>(record: &'a HashMap<String, String>, tag: &str) -> Result<&'a str> {
    record
        .get(tag)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("missing {tag}"))
}

fn parse_timestamp(date: &str, time: &str) -> Result<chrono::DateTime<Utc>> {
    let date = NaiveDate::parse_from_str(date.trim(), "%Y%m%d").context("invalid QSO_DATE")?;
    let value = time.trim();
    let format = match value.len() {
        4 => "%H%M",
        6 => "%H%M%S",
        _ => bail!("TIME_ON must contain HHMM or HHMMSS"),
    };
    let time = NaiveTime::parse_from_str(value, format).context("invalid TIME_ON")?;
    Ok(Utc.from_utc_datetime(&NaiveDateTime::new(date, time)))
}

fn split_exchange(value: &str) -> (Option<String>, Option<String>) {
    let mut parts = value.split_whitespace();
    let name = parts.next().map(str::to_string);
    let location = parts.next().map(str::to_string);
    (name, location)
}

fn string_field(raw: &Value, key: &str) -> Option<String> {
    raw.get(key)?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPORT: &str = "Generated by PoLo <EOH>\n<CALL:5>W1AW <QSO_DATE:8>20260801 <TIME_ON:6>180102 <BAND:3>20M <FREQ:6>14.035 <MODE:2>CW <NAME:2>AL <STATE:2>CT <COUNTRY:13>United States <DXCC:3>291 <CONTEST_ID:7>NAQP-CW <SRX_STRING:5>AL CT <EOR>";

    #[test]
    fn imports_polo_fields_and_is_idempotent() {
        let mut qsos = BTreeMap::new();
        let first = import_snapshot(EXPORT.as_bytes(), &mut qsos).unwrap();
        assert_eq!(first.added, 1);
        let second = import_snapshot(EXPORT.as_bytes(), &mut qsos).unwrap();
        assert_eq!(second.unchanged, 1);
        assert_eq!(qsos.len(), 1);
        let qso = qsos.values().next().unwrap();
        assert_eq!(qso.name.as_deref(), Some("AL"));
        assert_eq!(qso.location.as_deref(), Some("CT"));
        assert_eq!(qso.frequency_khz, Some(14_035.0));
    }

    #[test]
    fn accepts_srx_string_as_exchange_fallback() {
        let input = "<CALL:5>K1ABC<QSO_DATE:8>20260801<TIME_ON:4>1900<BAND:3>40M<MODE:2>CW<SRX_STRING:6>BOB MA<EOR>";
        let mut qsos = BTreeMap::new();
        import_snapshot(input.as_bytes(), &mut qsos).unwrap();
        let qso = qsos.values().next().unwrap();
        assert_eq!(qso.name.as_deref(), Some("BOB"));
        assert_eq!(qso.location.as_deref(), Some("MA"));
    }

    #[test]
    fn growing_snapshot_only_adds_new_records() {
        let mut qsos = BTreeMap::new();
        import_snapshot(EXPORT.as_bytes(), &mut qsos).unwrap();
        let growing = format!(
            "{EXPORT}<CALL:5>K1ABC<QSO_DATE:8>20260801<TIME_ON:4>1900<BAND:3>40M<MODE:2>CW<SRX_STRING:6>BOB MA<EOR>"
        );
        let diagnostics = import_snapshot(growing.as_bytes(), &mut qsos).unwrap();
        assert_eq!(diagnostics.unchanged, 1);
        assert_eq!(diagnostics.added, 1);
        assert_eq!(qsos.len(), 2);
    }

    #[test]
    fn malformed_snapshot_does_not_change_last_good_records() {
        let mut qsos = BTreeMap::new();
        import_snapshot(EXPORT.as_bytes(), &mut qsos).unwrap();
        let before = serde_json::to_value(&qsos).unwrap();

        assert!(import_snapshot(b"<CALL:50>short", &mut qsos).is_err());

        assert_eq!(serde_json::to_value(&qsos).unwrap(), before);
    }
}
