use std::cmp::Reverse;
use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::model::{
    Band, LocationConclusion, LocationConfidence, Spot, SpotClass, calls_equivalent, normalize_call,
};
use crate::naqp;

pub const TACTICAL_TTL: Duration = Duration::minutes(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MultiplierCertainty {
    Verified,
    Declared,
    History,
    Callbook,
    Prefix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceptionFreshness {
    Hot,
    Warm,
    Fading,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MultTarget {
    pub call: String,
    pub frequency_khz: f64,
    pub time: DateTime<Utc>,
    pub snr_db: Option<i16>,
    pub site_count: usize,
    pub skimmer_count: usize,
    pub reports: u32,
    pub nearest_site_km: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MultOpportunity {
    pub band: Band,
    pub multiplier_id: String,
    pub multiplier_code: String,
    pub multiplier_name: String,
    pub certainty: MultiplierCertainty,
    pub freshness: ReceptionFreshness,
    pub evidence_value: Option<String>,
    pub evidence_conflict: bool,
    pub primary: MultTarget,
    pub alternates: Vec<MultTarget>,
    pub rank_reasons: Vec<String>,
}

#[derive(Debug, Clone)]
struct RankedTarget {
    target: MultTarget,
    certainty: MultiplierCertainty,
    freshness: ReceptionFreshness,
    evidence_value: Option<String>,
    evidence_conflict: bool,
}

pub fn opportunities(
    score: &naqp::Score,
    spots: &[Spot],
    locations: &BTreeMap<String, LocationConclusion>,
    current_band: Option<Band>,
    now: DateTime<Utc>,
    nearby: bool,
) -> Vec<MultOpportunity> {
    let mut grouped = BTreeMap::<(Band, String), Vec<RankedTarget>>::new();

    for spot in spots {
        let age = now - spot.time;
        if spot.stale || age < Duration::zero() || age > TACTICAL_TTL {
            continue;
        }
        if !matches!(
            spot.class,
            SpotClass::VerifiedMultiplier | SpotClass::PredictedMultiplier
        ) {
            continue;
        }
        let Some(multiplier_id) = spot.predicted_multiplier.as_deref() else {
            continue;
        };
        let Some(multiplier) = score
            .multiplier_rows
            .iter()
            .find(|row| row.id == multiplier_id)
        else {
            continue;
        };
        if multiplier.worked_bands.contains(&spot.band) {
            continue;
        }
        if nearby && (spot.local_time.is_none() || spot.local_spotters.is_empty()) {
            continue;
        }

        let location = location_for_call(locations, &spot.call);
        let certainty = location
            .and_then(|location| certainty(location.confidence))
            .unwrap_or(match spot.class {
                SpotClass::VerifiedMultiplier => MultiplierCertainty::Verified,
                _ => MultiplierCertainty::Prefix,
            });
        let freshness = freshness(age);
        let target = MultTarget {
            call: spot.call.clone(),
            frequency_khz: spot.frequency_khz,
            time: spot.time,
            snr_db: if nearby {
                spot.local_snr_db
            } else {
                spot.snr_db
            },
            site_count: if nearby { spot.local_sites.len() } else { 0 },
            skimmer_count: if nearby {
                spot.local_spotters.len()
            } else {
                spot.spotters.len()
            },
            reports: if nearby {
                spot.local_reports
            } else {
                spot.reports
            },
            nearest_site_km: nearby.then_some(spot.nearest_local_km).flatten(),
        };
        grouped
            .entry((spot.band, multiplier.id.to_owned()))
            .or_default()
            .push(RankedTarget {
                target,
                certainty,
                freshness,
                evidence_value: location.and_then(|location| location.value.clone()),
                evidence_conflict: location.is_some_and(|location| !location.conflicts.is_empty()),
            });
    }

    let mut result = Vec::with_capacity(grouped.len());
    for ((band, multiplier_id), mut targets) in grouped {
        targets.sort_by_key(|candidate| target_rank(candidate, now));
        let primary = targets.remove(0);
        let multiplier = score
            .multiplier_rows
            .iter()
            .find(|row| row.id == multiplier_id)
            .expect("grouped multiplier came from score rows");
        let rank_reasons = rank_reasons(&primary, nearby, now);
        result.push(MultOpportunity {
            band,
            multiplier_id,
            multiplier_code: multiplier.code.to_owned(),
            multiplier_name: multiplier.display_name.to_owned(),
            certainty: primary.certainty,
            freshness: primary.freshness,
            evidence_value: primary.evidence_value,
            evidence_conflict: primary.evidence_conflict,
            primary: primary.target,
            alternates: targets
                .into_iter()
                .map(|candidate| candidate.target)
                .collect(),
            rank_reasons,
        });
    }
    result.sort_by_key(|opportunity| opportunity_rank(opportunity, current_band, now));
    result
}

fn location_for_call<'a>(
    locations: &'a BTreeMap<String, LocationConclusion>,
    call: &str,
) -> Option<&'a LocationConclusion> {
    let normalized = normalize_call(call);
    locations.get(&normalized).or_else(|| {
        locations
            .iter()
            .find(|(known, _)| calls_equivalent(known, &normalized))
            .map(|(_, value)| value)
    })
}

fn certainty(confidence: LocationConfidence) -> Option<MultiplierCertainty> {
    match confidence {
        LocationConfidence::Verified => Some(MultiplierCertainty::Verified),
        LocationConfidence::ContestDeclared => Some(MultiplierCertainty::Declared),
        LocationConfidence::History => Some(MultiplierCertainty::History),
        LocationConfidence::Callbook => Some(MultiplierCertainty::Callbook),
        LocationConfidence::PrefixOnly => Some(MultiplierCertainty::Prefix),
        LocationConfidence::Unknown => None,
    }
}

fn certainty_priority(certainty: MultiplierCertainty) -> u8 {
    match certainty {
        MultiplierCertainty::Verified => 0,
        MultiplierCertainty::Declared => 1,
        MultiplierCertainty::History => 2,
        MultiplierCertainty::Callbook => 3,
        MultiplierCertainty::Prefix => 4,
    }
}

fn freshness(age: Duration) -> ReceptionFreshness {
    if age <= Duration::seconds(45) {
        ReceptionFreshness::Hot
    } else if age <= Duration::seconds(120) {
        ReceptionFreshness::Warm
    } else {
        ReceptionFreshness::Fading
    }
}

fn freshness_priority(freshness: ReceptionFreshness) -> u8 {
    match freshness {
        ReceptionFreshness::Hot => 0,
        ReceptionFreshness::Warm => 1,
        ReceptionFreshness::Fading => 2,
    }
}

type TargetRank = (
    u8,
    u8,
    Reverse<usize>,
    Reverse<i16>,
    Reverse<u32>,
    i64,
    String,
    i64,
);

fn target_rank(candidate: &RankedTarget, now: DateTime<Utc>) -> TargetRank {
    (
        freshness_priority(candidate.freshness),
        certainty_priority(candidate.certainty),
        Reverse(candidate.target.site_count),
        Reverse(candidate.target.snr_db.unwrap_or(i16::MIN)),
        Reverse(candidate.target.reports),
        (now - candidate.target.time).num_seconds(),
        candidate.target.call.clone(),
        (candidate.target.frequency_khz * 10.0).round() as i64,
    )
}

type OpportunityRank = (
    bool,
    u8,
    u8,
    Reverse<usize>,
    Reverse<i16>,
    Reverse<u32>,
    i64,
    String,
    String,
);

fn opportunity_rank(
    opportunity: &MultOpportunity,
    current_band: Option<Band>,
    now: DateTime<Utc>,
) -> OpportunityRank {
    (
        current_band.is_some_and(|band| opportunity.band != band),
        freshness_priority(opportunity.freshness),
        certainty_priority(opportunity.certainty),
        Reverse(opportunity.primary.site_count),
        Reverse(opportunity.primary.snr_db.unwrap_or(i16::MIN)),
        Reverse(opportunity.primary.reports),
        (now - opportunity.primary.time).num_seconds(),
        opportunity.multiplier_code.clone(),
        opportunity.primary.call.clone(),
    )
}

fn rank_reasons(candidate: &RankedTarget, nearby: bool, now: DateTime<Utc>) -> Vec<String> {
    let mut reasons = vec![match candidate.freshness {
        ReceptionFreshness::Hot => "heard in the last 45 seconds".to_owned(),
        ReceptionFreshness::Warm => "heard in the last 2 minutes".to_owned(),
        ReceptionFreshness::Fading => format!(
            "last heard {} minutes ago",
            (now - candidate.target.time).num_minutes().max(2)
        ),
    }];
    if nearby {
        reasons.push(format!(
            "heard by {} nearby site{}",
            candidate.target.site_count,
            if candidate.target.site_count == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    reasons.push(match candidate.certainty {
        MultiplierCertainty::Verified => "multiplier copied in this contest".to_owned(),
        MultiplierCertainty::Declared => "current-contest declaration".to_owned(),
        MultiplierCertainty::History => "call history estimate".to_owned(),
        MultiplierCertainty::Callbook => "QRZ callbook estimate".to_owned(),
        MultiplierCertainty::Prefix => "callsign prefix guess".to_owned(),
    });
    reasons
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::TimeZone;

    use super::*;
    use crate::model::{EvidenceSource, LocationEvidence, Qso};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 1, 20, 0, 0).unwrap()
    }

    fn score_with(worked: &[(&str, Band)]) -> naqp::Score {
        naqp::score(
            worked
                .iter()
                .enumerate()
                .map(|(index, (location, band))| Qso {
                    id: format!("qso-{index}"),
                    call: format!("W1{index}AA"),
                    timestamp: now() - Duration::minutes(30),
                    band: Some(*band),
                    frequency_khz: None,
                    mode: "CW".into(),
                    name: Some("AL".into()),
                    location: Some((*location).into()),
                    country: Some("United States".into()),
                    dxcc: Some(291),
                    contest_id: Some("NAQP-CW".into()),
                    deleted: false,
                    raw: serde_json::Value::Null,
                }),
        )
    }

    fn spot(call: &str, multiplier: &str, seconds_ago: i64, sites: usize) -> Spot {
        let local_spotters = (0..sites).map(|index| format!("SK{index}")).collect();
        let local_sites = (0..sites).map(|index| format!("FN4{index}")).collect();
        Spot {
            id: format!("{call}-20"),
            call: call.into(),
            frequency_khz: 14_030.0,
            band: Band::B20,
            time: now() - Duration::seconds(seconds_ago),
            spotter: "SK0".into(),
            spotters: BTreeSet::from(["SK0".into()]),
            snr_db: Some(8),
            best_snr_db: Some(8),
            speed_wpm: Some(28),
            class: SpotClass::PredictedMultiplier,
            predicted_multiplier: Some(multiplier.into()),
            reports: sites as u32,
            preferred_spotter: false,
            stale: false,
            local_time: Some(now() - Duration::seconds(seconds_ago)),
            local_frequency_khz: Some(14_030.0),
            local_spotter: Some("SK0".into()),
            local_spotters,
            local_sites,
            local_snr_db: Some(8),
            local_best_snr_db: Some(8),
            local_speed_wpm: Some(28),
            local_reports: sites as u32,
            nearest_local_km: Some(20),
        }
    }

    fn location(
        call: &str,
        value: &str,
        confidence: LocationConfidence,
    ) -> (String, LocationConclusion) {
        let evidence = LocationEvidence {
            value: value.into(),
            confidence,
            source: EvidenceSource::CallHistory,
            observed_at: now(),
            expires_at: None,
        };
        (
            call.into(),
            LocationConclusion {
                value: Some(value.into()),
                confidence,
                evidence: vec![evidence],
                conflicts: Vec::new(),
            },
        )
    }

    #[test]
    fn excludes_worked_multiplier_and_expired_reception() {
        let score = score_with(&[("CT", Band::B20)]);
        let spots = vec![spot("K1CT", "CT", 10, 2), spot("K1RI", "RI", 301, 2)];
        let locations = BTreeMap::from([
            location("K1CT", "CT", LocationConfidence::History),
            location("K1RI", "RI", LocationConfidence::History),
        ]);
        assert!(opportunities(&score, &spots, &locations, Some(Band::B20), now(), true).is_empty());
    }

    #[test]
    fn groups_alternates_by_multiplier() {
        let score = score_with(&[]);
        let spots = vec![spot("K1AAA", "RI", 10, 2), spot("K1BBB", "RI", 20, 1)];
        let locations = BTreeMap::from([
            location("K1AAA", "RI", LocationConfidence::History),
            location("K1BBB", "RI", LocationConfidence::History),
        ]);
        let result = opportunities(&score, &spots, &locations, Some(Band::B20), now(), true);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].primary.call, "K1AAA");
        assert_eq!(result[0].alternates.len(), 1);
    }

    #[test]
    fn freshness_beats_certainty_then_sites_break_ties() {
        let score = score_with(&[]);
        let mut verified = spot("K1VER", "VT", 80, 3);
        verified.class = SpotClass::VerifiedMultiplier;
        let spots = vec![
            verified,
            spot("K1HOT", "RI", 10, 1),
            spot("K1TWO", "ME", 10, 2),
        ];
        let locations = BTreeMap::from([
            location("K1VER", "VT", LocationConfidence::Verified),
            location("K1HOT", "RI", LocationConfidence::History),
            location("K1TWO", "ME", LocationConfidence::History),
        ]);
        let result = opportunities(&score, &spots, &locations, Some(Band::B20), now(), true);
        assert_eq!(
            result
                .iter()
                .map(|item| item.primary.call.as_str())
                .collect::<Vec<_>>(),
            vec!["K1TWO", "K1HOT", "K1VER"]
        );
    }
}
