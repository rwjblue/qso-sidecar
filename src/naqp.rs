use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::Serialize;

use crate::model::{Band, Qso};
use crate::naqp_catalog::{self, MultiplierGroup};

pub const RULES_VERSION: &str = "2026-08-cw";
pub const OFFICIAL_RULES_URL: &str = "https://ncjweb.com/NAQP-Rules.pdf";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ContestRules {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    pub official_source: &'static str,
    pub mode: &'static str,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub maximum_operating_minutes: i64,
    pub bands: [Band; 6],
}

pub fn contest_rules() -> ContestRules {
    ContestRules {
        id: "naqp-cw-2026-08",
        name: "NAQP CW — August 2026",
        version: RULES_VERSION,
        official_source: OFFICIAL_RULES_URL,
        mode: "CW",
        starts_at: Utc.with_ymd_and_hms(2026, 8, 1, 18, 0, 0).unwrap(),
        ends_at: Utc.with_ymd_and_hms(2026, 8, 2, 6, 0, 0).unwrap(),
        maximum_operating_minutes: 600,
        bands: Band::ALL,
    }
}

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
    pub band: Option<Band>,
    pub status: QsoStatus,
    pub reason: QsoReason,
    pub duplicate: bool,
    pub unresolved_exchange: bool,
    pub multiplier: Option<String>,
    pub multiplier_id: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QsoStatus {
    Valid,
    Duplicate,
    Unresolved,
    Excluded,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QsoReason {
    Credited,
    DuplicateCallOnBand,
    IncompleteExchange,
    WrongMode,
    IneligibleBand,
    OutsideContestPeriod,
    Tombstone,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MultiplierRow {
    pub id: &'static str,
    pub code: &'static str,
    pub display_name: &'static str,
    pub group: MultiplierGroup,
    pub worked_bands: BTreeSet<Band>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Score {
    pub rules_version: &'static str,
    pub valid_qsos: usize,
    pub duplicates: usize,
    pub unresolved_exchanges: usize,
    pub excluded_qsos: usize,
    pub total_multipliers: usize,
    pub claimed_score: usize,
    pub operating_minutes: i64,
    pub off_minutes: i64,
    pub bands: Vec<BandScore>,
    pub multiplier_rows: Vec<MultiplierRow>,
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
    naqp_catalog::find(&token).map(|multiplier| multiplier.code.to_string())
}

pub fn resolve_qso_multiplier(qso: &Qso) -> Option<&'static naqp_catalog::MultiplierDefinition> {
    let code = qso
        .location
        .as_deref()
        .and_then(|location| location.split_whitespace().last())
        .and_then(normalize_multiplier)?;
    naqp_catalog::resolve(&code, &qso.call, qso.country.as_deref())
}

pub fn score(qsos: impl IntoIterator<Item = Qso>) -> Score {
    score_with_rules(qsos, &contest_rules())
}

pub fn score_with_rules(qsos: impl IntoIterator<Item = Qso>, rules: &ContestRules) -> Score {
    let mut input: Vec<_> = qsos.into_iter().collect();
    input.sort_by_key(|qso| qso.timestamp);

    let mut seen = HashSet::new();
    let mut multipliers: HashMap<Band, BTreeSet<String>> = HashMap::new();
    let mut counts: HashMap<Band, usize> = HashMap::new();
    let mut scored = Vec::with_capacity(input.len());
    let mut valid_timestamps = Vec::new();
    let mut duplicate_count = 0;
    let mut unresolved_count = 0;
    let mut excluded_count = 0;

    for qso in input {
        let multiplier = qso
            .location
            .as_deref()
            .and_then(|location| location.split_whitespace().last())
            .and_then(normalize_multiplier);
        let multiplier_definition = resolve_qso_multiplier(&qso);
        let explicit_non_na = qso
            .location
            .as_deref()
            .and_then(|location| location.split_whitespace().last())
            .is_some_and(|location| location.trim_matches(['.', ',']).eq_ignore_ascii_case("DX"));
        let has_name = qso
            .name
            .as_deref()
            .is_some_and(|name| !name.trim().is_empty());
        let complete_exchange = has_name && (multiplier.is_some() || explicit_non_na);
        let (status, reason, duplicate, unresolved) = if qso.deleted {
            excluded_count += 1;
            (QsoStatus::Excluded, QsoReason::Tombstone, false, false)
        } else if !qso.mode.eq_ignore_ascii_case(rules.mode) {
            excluded_count += 1;
            (QsoStatus::Excluded, QsoReason::WrongMode, false, false)
        } else if qso.band.is_none() {
            excluded_count += 1;
            (QsoStatus::Excluded, QsoReason::IneligibleBand, false, false)
        } else if qso.timestamp < rules.starts_at || qso.timestamp >= rules.ends_at {
            excluded_count += 1;
            (
                QsoStatus::Excluded,
                QsoReason::OutsideContestPeriod,
                false,
                false,
            )
        } else if !complete_exchange {
            unresolved_count += 1;
            (
                QsoStatus::Unresolved,
                QsoReason::IncompleteExchange,
                false,
                true,
            )
        } else if let Some(band) = qso.band {
            let key = (qso.normalized_call(), band);
            if !seen.insert(key) {
                duplicate_count += 1;
                (
                    QsoStatus::Duplicate,
                    QsoReason::DuplicateCallOnBand,
                    true,
                    false,
                )
            } else {
                *counts.entry(band).or_default() += 1;
                valid_timestamps.push(qso.timestamp);
                if let Some(definition) = multiplier_definition {
                    multipliers
                        .entry(band)
                        .or_default()
                        .insert(definition.id.to_string());
                }
                (QsoStatus::Valid, QsoReason::Credited, false, false)
            }
        } else {
            unreachable!("missing bands are excluded above")
        };
        scored.push(ScoredQso {
            id: qso.id,
            band: qso.band,
            status,
            reason,
            duplicate,
            unresolved_exchange: unresolved,
            multiplier,
            multiplier_id: multiplier_definition.map(|definition| definition.id),
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
    let multiplier_rows = naqp_catalog::MULTIPLIERS
        .iter()
        .map(|definition| MultiplierRow {
            id: definition.id,
            code: definition.code,
            display_name: definition.display_name,
            group: definition.group,
            worked_bands: Band::ALL
                .into_iter()
                .filter(|band| {
                    multipliers
                        .get(band)
                        .is_some_and(|values| values.contains(definition.id))
                })
                .collect(),
        })
        .collect();

    Score {
        rules_version: rules.version,
        valid_qsos,
        duplicates: duplicate_count,
        unresolved_exchanges: unresolved_count,
        excluded_qsos: excluded_count,
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
        let ct = result
            .multiplier_rows
            .iter()
            .find(|row| row.code == "CT")
            .unwrap();
        assert_eq!(ct.worked_bands.len(), 2);
        assert_eq!(result.multiplier_rows.len(), 111);
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
        let result = score([wrong_mode, late]);
        assert_eq!(result.valid_qsos, 0);
        assert_eq!(result.excluded_qsos, 2);
        assert_eq!(result.qsos.len(), 2);
        assert_eq!(result.qsos[0].reason, QsoReason::WrongMode);
        assert_eq!(result.qsos[1].reason, QsoReason::OutsideContestPeriod);
    }

    #[test]
    fn every_normalized_record_has_an_explicit_status_and_reason() {
        let valid = qso("valid", "W1AW", Band::B20, 0, "CT");
        let duplicate = qso("duplicate", "W1AW", Band::B20, 1, "CT");
        let mut unknown_band = qso("band", "K1ABC", Band::B20, 2, "MA");
        unknown_band.band = None;
        let mut tombstone = qso("deleted", "K2ABC", Band::B20, 3, "NY");
        tombstone.deleted = true;

        let result = score([valid, duplicate, unknown_band, tombstone]);

        assert_eq!(result.qsos[0].status, QsoStatus::Valid);
        assert_eq!(result.qsos[0].reason, QsoReason::Credited);
        assert_eq!(result.qsos[1].status, QsoStatus::Duplicate);
        assert_eq!(result.qsos[1].reason, QsoReason::DuplicateCallOnBand);
        assert_eq!(result.qsos[2].reason, QsoReason::IneligibleBand);
        assert_eq!(result.qsos[3].reason, QsoReason::Tombstone);
    }

    #[test]
    fn hand_calculated_fixture_covers_all_bands_and_multiplier_groups() {
        let result = score([
            qso("1", "W1AW", Band::B160, 0, "CT"),
            qso("2", "VE3EJ", Band::B80, 1, "ON"),
            qso("3", "KP2M", Band::B40, 2, "KP2"),
            qso("4", "K1ABC", Band::B20, 3, "MA"),
            qso("5", "VE2ABC", Band::B15, 4, "QC"),
            qso("6", "ZF1A", Band::B10, 5, "ZF"),
        ]);

        assert_eq!(result.valid_qsos, 6);
        assert_eq!(result.total_multipliers, 6);
        assert_eq!(result.claimed_score, 36);
        assert!(result.bands.iter().all(|band| band.qsos == 1));
    }

    #[test]
    fn hawaii_and_dominican_republic_are_distinct_hi_multipliers() {
        let mut hawaii = qso("1", "KH6ABC", Band::B20, 0, "HI");
        hawaii.country = Some("Hawaii".into());
        let mut dominican = qso("2", "HI8ABC", Band::B20, 1, "HI");
        dominican.country = Some("Dominican Republic".into());

        let result = score([hawaii, dominican]);

        assert_eq!(result.total_multipliers, 2);
        assert!(
            result
                .multiplier_rows
                .iter()
                .find(|row| row.id == "US-HI")
                .unwrap()
                .worked_bands
                .contains(&Band::B20)
        );
        assert!(
            result
                .multiplier_rows
                .iter()
                .find(|row| row.id == "DXCC-HI")
                .unwrap()
                .worked_bands
                .contains(&Band::B20)
        );
    }
}
