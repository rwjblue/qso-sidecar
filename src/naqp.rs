use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{Duration, TimeZone, Utc};
use serde::Serialize;

use crate::model::{Band, Qso};

const US_MULTIPLIERS: &[&str] = &[
    "AL", "AK", "AZ", "AR", "CA", "CO", "CT", "DE", "FL", "GA", "HI", "ID", "IL", "IN", "IA", "KS",
    "KY", "LA", "ME", "MD", "MA", "MI", "MN", "MS", "MO", "MT", "NE", "NV", "NH", "NJ", "NM", "NY",
    "NC", "ND", "OH", "OK", "OR", "PA", "RI", "SC", "SD", "TN", "TX", "UT", "VT", "VA", "WA", "WV",
    "WI", "WY", "DC",
];
const CANADIAN_MULTIPLIERS: &[&str] = &[
    "AB", "BC", "MB", "NB", "NL", "NS", "NT", "NU", "ON", "PE", "QC", "SK", "YT",
];
// Exchange abbreviations accepted by the NAQP rules for other North American entities.
const NA_ENTITY_MULTIPLIERS: &[&str] = &[
    "4U1U", "6Y", "8P", "C6", "CM", "CY9", "CY0", "FG", "FJ", "FM", "FO", "FP", "FS", "HH", "HI",
    "HK0", "HP", "HR", "J3", "J6", "J7", "J8", "KG4", "KP1", "KP2", "KP4", "KP5", "OX", "PJ5",
    "PJ7", "TG", "TI", "TI9", "V2", "V3", "V4", "VP2E", "VP2M", "VP2V", "VP5", "VP9", "XE", "XF4",
    "YN", "YS", "YV0", "ZF",
];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BandScore {
    pub band: Band,
    pub qsos: usize,
    pub multipliers: usize,
    pub multiplier_values: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScoredQso {
    pub id: String,
    pub duplicate: bool,
    pub unresolved_exchange: bool,
    pub multiplier: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Score {
    pub valid_qsos: usize,
    pub duplicates: usize,
    pub unresolved_exchanges: usize,
    pub total_multipliers: usize,
    pub claimed_score: usize,
    pub operating_minutes: i64,
    pub off_minutes: i64,
    pub bands: Vec<BandScore>,
    pub multiplier_rows: BTreeMap<String, BTreeSet<Band>>,
    pub qsos: Vec<ScoredQso>,
}

pub fn normalize_multiplier(value: &str) -> Option<String> {
    let mut token = value.trim().to_ascii_uppercase().replace(['.', ','], "");
    token = match token.as_str() {
        "NFLD" | "NF" => "NL".into(),
        "NWT" => "NT".into(),
        "QUE" | "PQ" => "QC".into(),
        "PEI" => "PE".into(),
        "YUK" => "YT".into(),
        "MEX" | "MEXICO" => "XE".into(),
        "4U1UN" | "4U1/U" => "4U1U".into(),
        other => other.into(),
    };
    US_MULTIPLIERS
        .iter()
        .chain(CANADIAN_MULTIPLIERS)
        .chain(NA_ENTITY_MULTIPLIERS)
        .any(|candidate| *candidate == token)
        .then_some(token)
}

pub fn score(qsos: impl IntoIterator<Item = Qso>) -> Score {
    let mut input: Vec<_> = qsos
        .into_iter()
        .filter(|qso| {
            let start = Utc.with_ymd_and_hms(2026, 8, 1, 18, 0, 0).unwrap();
            let end = Utc.with_ymd_and_hms(2026, 8, 2, 6, 0, 0).unwrap();
            !qso.deleted
                && qso.band.is_some()
                && qso.mode.eq_ignore_ascii_case("CW")
                && qso.timestamp >= start
                && qso.timestamp < end
        })
        .collect();
    input.sort_by_key(|qso| qso.timestamp);

    let mut seen = HashSet::new();
    let mut multipliers: HashMap<Band, BTreeSet<String>> = HashMap::new();
    let mut counts: HashMap<Band, usize> = HashMap::new();
    let mut scored = Vec::with_capacity(input.len());
    let mut valid_timestamps = Vec::new();
    let mut duplicate_count = 0;
    let mut unresolved_count = 0;

    for qso in input {
        let band = qso.band.expect("filtered to eligible bands");
        let key = (qso.normalized_call(), band);
        let multiplier = qso
            .location
            .as_deref()
            .and_then(|location| location.split_whitespace().last())
            .and_then(normalize_multiplier);
        let explicit_non_na = qso
            .location
            .as_deref()
            .and_then(|location| location.split_whitespace().last())
            .is_some_and(|location| location.trim_matches(['.', ',']).eq_ignore_ascii_case("DX"));
        let has_name = qso
            .name
            .as_deref()
            .is_some_and(|name| !name.trim().is_empty());
        let unresolved = !has_name || (multiplier.is_none() && !explicit_non_na);
        let complete_exchange = has_name && (multiplier.is_some() || explicit_non_na);
        let duplicate = complete_exchange && !seen.insert(key);

        if !complete_exchange {
            unresolved_count += 1;
        } else if duplicate {
            duplicate_count += 1;
        } else {
            *counts.entry(band).or_default() += 1;
            valid_timestamps.push(qso.timestamp);
            if let Some(value) = &multiplier {
                multipliers.entry(band).or_default().insert(value.clone());
            }
        }
        scored.push(ScoredQso {
            id: qso.id,
            duplicate,
            unresolved_exchange: unresolved,
            multiplier,
        });
    }

    let valid_qsos = counts.values().sum();
    let total_multipliers: usize = multipliers.values().map(BTreeSet::len).sum();
    let (operating_minutes, off_minutes) = operating_time(&valid_timestamps);
    let bands = Band::ALL
        .into_iter()
        .map(|band| BandScore {
            band,
            qsos: counts.get(&band).copied().unwrap_or_default(),
            multipliers: multipliers.get(&band).map_or(0, BTreeSet::len),
            multiplier_values: multipliers
                .get(&band)
                .map(|values| values.iter().cloned().collect())
                .unwrap_or_default(),
        })
        .collect();
    let mut multiplier_rows: BTreeMap<String, BTreeSet<Band>> = BTreeMap::new();
    for (band, values) in multipliers {
        for value in values {
            multiplier_rows.entry(value).or_default().insert(band);
        }
    }

    Score {
        valid_qsos,
        duplicates: duplicate_count,
        unresolved_exchanges: unresolved_count,
        total_multipliers,
        claimed_score: valid_qsos * total_multipliers,
        operating_minutes,
        off_minutes,
        bands,
        multiplier_rows,
        qsos: scored,
    }
}

fn operating_time(timestamps: &[chrono::DateTime<chrono::Utc>]) -> (i64, i64) {
    let Some(first) = timestamps.first() else {
        return (0, 0);
    };
    let Some(last) = timestamps.last() else {
        return (0, 0);
    };
    let elapsed = (*last - *first).num_minutes().max(0);
    let off: i64 = timestamps
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .filter(|gap| *gap >= Duration::minutes(31))
        .map(|gap| gap.num_minutes().saturating_sub(1))
        .sum();
    (elapsed.saturating_sub(off), off)
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::Value;

    use super::*;

    fn qso(id: &str, call: &str, band: Band, minute: u32, location: &str) -> Qso {
        Qso {
            id: id.into(),
            call: call.into(),
            timestamp: Utc.with_ymd_and_hms(2026, 8, 1, 18, minute, 0).unwrap(),
            band: Some(band),
            frequency_khz: None,
            mode: "CW".into(),
            name: Some("AL".into()),
            location: Some(location.into()),
            country: None,
            dxcc: None,
            contest_id: Some("NAQP-CW".into()),
            deleted: false,
            raw: Value::Null,
        }
    }

    #[test]
    fn duplicates_only_count_once_per_band() {
        let result = score([
            qso("1", "W1AW", Band::B20, 0, "CT"),
            qso("2", "w1aw", Band::B20, 1, "CT"),
            qso("3", "W1AW", Band::B40, 2, "CT"),
        ]);
        assert_eq!(result.valid_qsos, 2);
        assert_eq!(result.duplicates, 1);
        assert_eq!(result.total_multipliers, 2);
        assert_eq!(result.claimed_score, 4);
    }

    #[test]
    fn same_multiplier_counts_on_each_band() {
        let result = score([
            qso("1", "W1AW", Band::B20, 0, "CT"),
            qso("2", "K1ABC", Band::B40, 1, "CT"),
        ]);
        assert_eq!(result.total_multipliers, 2);
        assert_eq!(result.multiplier_rows["CT"].len(), 2);
    }

    #[test]
    fn thirty_one_minutes_is_off_time_boundary() {
        let result = score([
            qso("1", "W1AW", Band::B20, 0, "CT"),
            qso("2", "K1ABC", Band::B20, 30, "MA"),
        ]);
        assert_eq!(result.off_minutes, 0);
        let result = score([
            qso("1", "W1AW", Band::B20, 0, "CT"),
            qso("2", "K1ABC", Band::B20, 31, "MA"),
        ]);
        assert_eq!(result.off_minutes, 30);
        assert_eq!(result.operating_minutes, 1);
    }

    #[test]
    fn non_na_qso_counts_without_multiplier() {
        let mut contact = qso("1", "DL1ABC", Band::B20, 0, "dx,");
        contact.country = None;
        let result = score([contact]);
        assert_eq!(result.valid_qsos, 1);
        assert_eq!(result.total_multipliers, 0);
        assert_eq!(result.unresolved_exchanges, 0);
    }

    #[test]
    fn country_alone_does_not_complete_exchange() {
        let mut contact = qso("1", "DL1ABC", Band::B20, 0, "");
        contact.location = None;
        contact.country = Some("Germany".into());
        let result = score([contact]);
        assert_eq!(result.valid_qsos, 0);
        assert_eq!(result.total_multipliers, 0);
        assert_eq!(result.unresolved_exchanges, 1);
    }

    #[test]
    fn incomplete_na_exchange_is_explicit_and_excluded() {
        let mut contact = qso("1", "W1AW", Band::B20, 0, "");
        contact.location = None;
        contact.country = Some("United States".into());
        let result = score([contact]);
        assert_eq!(result.valid_qsos, 0);
        assert_eq!(result.unresolved_exchanges, 1);
    }

    #[test]
    fn excludes_wrong_mode_and_out_of_period_records() {
        let mut wrong_mode = qso("1", "W1AW", Band::B20, 0, "CT");
        wrong_mode.mode = "SSB".into();
        let mut late = qso("2", "K1ABC", Band::B20, 1, "MA");
        late.timestamp = Utc.with_ymd_and_hms(2026, 8, 2, 6, 0, 0).unwrap();
        assert_eq!(score([wrong_mode, late]).valid_qsos, 0);
    }
}
