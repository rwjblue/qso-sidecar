use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Band {
    B160,
    B80,
    B40,
    B20,
    B15,
    B10,
}

impl Band {
    pub const ALL: [Self; 6] = [
        Self::B160,
        Self::B80,
        Self::B40,
        Self::B20,
        Self::B15,
        Self::B10,
    ];

    pub fn from_frequency_khz(frequency: f64) -> Option<Self> {
        match frequency {
            1_800.0..=2_000.0 => Some(Self::B160),
            3_500.0..=4_000.0 => Some(Self::B80),
            7_000.0..=7_300.0 => Some(Self::B40),
            14_000.0..=14_350.0 => Some(Self::B20),
            21_000.0..=21_450.0 => Some(Self::B15),
            28_000.0..=29_700.0 => Some(Self::B10),
            _ => None,
        }
    }

    pub fn meters(self) -> u16 {
        match self {
            Self::B160 => 160,
            Self::B80 => 80,
            Self::B40 => 40,
            Self::B20 => 20,
            Self::B15 => 15,
            Self::B10 => 10,
        }
    }
}

impl fmt::Display for Band {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}m", self.meters())
    }
}

impl FromStr for Band {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.trim_end_matches('m') {
            "160" => Ok(Self::B160),
            "80" => Ok(Self::B80),
            "40" => Ok(Self::B40),
            "20" => Ok(Self::B20),
            "15" => Ok(Self::B15),
            "10" => Ok(Self::B10),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Qso {
    pub id: String,
    pub call: String,
    pub timestamp: DateTime<Utc>,
    pub band: Option<Band>,
    pub frequency_khz: Option<f64>,
    pub mode: String,
    pub name: Option<String>,
    pub location: Option<String>,
    pub country: Option<String>,
    pub dxcc: Option<u32>,
    pub contest_id: Option<String>,
    pub deleted: bool,
    #[serde(default)]
    pub raw: Value,
}

impl Qso {
    pub fn normalized_call(&self) -> String {
        self.call.trim().to_ascii_uppercase()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    LocalQso,
    ReverseBeaconNetwork,
    ContestOnlineScoreboard,
    TeamRegistration,
    CallHistory,
    Callbook,
    Prefix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceId {
    ReverseBeaconNetwork,
    ContestOnlineScoreboard,
    N1mmCallHistory,
    PoloLofi,
    AdifImport,
    NaqpRules,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceCapability {
    LiveExternalAssistance,
    StaticHistory,
    LocalLog,
    OfflineReference,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceStatus {
    pub id: SourceId,
    pub label: &'static str,
    pub capability: SourceCapability,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct SourcePolicy {
    sources: BTreeMap<SourceId, SourceStatus>,
}

impl Default for SourcePolicy {
    fn default() -> Self {
        let sources = [
            (
                SourceId::ReverseBeaconNetwork,
                "Reverse Beacon Network",
                SourceCapability::LiveExternalAssistance,
                false,
            ),
            (
                SourceId::ContestOnlineScoreboard,
                "Contest Online ScoreBoard",
                SourceCapability::LiveExternalAssistance,
                false,
            ),
            (
                SourceId::N1mmCallHistory,
                "N1MM Call History",
                SourceCapability::StaticHistory,
                false,
            ),
            (
                SourceId::PoloLofi,
                "Ham2K PoLo via LoFi",
                SourceCapability::LocalLog,
                false,
            ),
            (
                SourceId::AdifImport,
                "ADIF import",
                SourceCapability::LocalLog,
                false,
            ),
            (
                SourceId::NaqpRules,
                "NAQP rules and multiplier catalog",
                SourceCapability::OfflineReference,
                true,
            ),
        ]
        .into_iter()
        .map(|(id, label, capability, enabled)| {
            (
                id,
                SourceStatus {
                    id,
                    label,
                    capability,
                    enabled,
                },
            )
        })
        .collect();
        Self { sources }
    }
}

impl SourcePolicy {
    pub fn set_enabled(&mut self, id: SourceId, enabled: bool) {
        if let Some(source) = self.sources.get_mut(&id) {
            source.enabled = enabled;
        }
    }

    pub fn is_enabled(&self, id: SourceId) -> bool {
        self.sources.get(&id).is_some_and(|source| source.enabled)
    }

    pub fn statuses(&self) -> Vec<SourceStatus> {
        self.sources.values().cloned().collect()
    }

    pub fn requires_assisted_entry(&self) -> bool {
        self.sources.values().any(|source| {
            source.enabled && source.capability == SourceCapability::LiveExternalAssistance
        })
    }

    pub fn assisted_warning(&self) -> Option<String> {
        if !self.requires_assisted_entry() {
            return None;
        }
        let live_sources: Vec<_> = self
            .sources
            .values()
            .filter(|source| {
                source.enabled && source.capability == SourceCapability::LiveExternalAssistance
            })
            .map(|source| source.label)
            .collect();
        Some(format!(
            "Live external assistance is enabled ({}). Enter Single Operator Assisted or another category that permits assistance.",
            live_sources.join(", ")
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipationConfidence {
    Unknown,
    Probable,
    Declared,
    Confirmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocationConfidence {
    Unknown,
    PrefixOnly,
    Callbook,
    History,
    ContestDeclared,
    Verified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NameConfidence {
    Unknown,
    History,
    Verified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParticipationEvidence {
    pub confidence: ParticipationConfidence,
    pub source: EvidenceSource,
    pub observed_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl ParticipationEvidence {
    pub fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_none_or(|expires_at| expires_at > now)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationEvidence {
    pub value: String,
    pub confidence: LocationConfidence,
    pub source: EvidenceSource,
    pub observed_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl LocationEvidence {
    pub fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_none_or(|expires_at| expires_at > now)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameEvidence {
    pub value: String,
    pub confidence: NameConfidence,
    pub source: EvidenceSource,
    pub observed_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl NameEvidence {
    pub fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_none_or(|expires_at| expires_at > now)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParticipationConclusion {
    pub confidence: ParticipationConfidence,
    pub evidence: Vec<ParticipationEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationConclusion {
    pub value: Option<String>,
    pub confidence: LocationConfidence,
    pub evidence: Vec<LocationEvidence>,
    pub conflicts: Vec<LocationEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameConclusion {
    pub value: Option<String>,
    pub confidence: NameConfidence,
    pub evidence: Vec<NameEvidence>,
    pub conflicts: Vec<NameEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StationEvidence {
    pub call: String,
    pub participation: Vec<ParticipationEvidence>,
    pub names: Vec<NameEvidence>,
    pub locations: Vec<LocationEvidence>,
}

impl StationEvidence {
    pub fn new(call: impl Into<String>) -> Self {
        Self {
            call: call.into().trim().to_ascii_uppercase(),
            participation: Vec::new(),
            names: Vec::new(),
            locations: Vec::new(),
        }
    }

    pub fn participation_at(&self, now: DateTime<Utc>) -> ParticipationConclusion {
        let evidence: Vec<_> = self
            .participation
            .iter()
            .filter(|item| item.is_fresh(now))
            .cloned()
            .collect();
        let confidence = evidence
            .iter()
            .map(|item| item.confidence)
            .max()
            .unwrap_or(ParticipationConfidence::Unknown);
        ParticipationConclusion {
            confidence,
            evidence,
        }
    }

    pub fn location_at(&self, now: DateTime<Utc>) -> LocationConclusion {
        let evidence: Vec<_> = self
            .locations
            .iter()
            .filter(|item| item.is_fresh(now))
            .cloned()
            .collect();
        let selected = evidence
            .iter()
            .enumerate()
            .max_by_key(|(index, item)| (item.confidence, item.observed_at, *index))
            .map(|(_, item)| item);
        let value = selected.map(|item| item.value.clone());
        let confidence = selected
            .map(|item| item.confidence)
            .unwrap_or(LocationConfidence::Unknown);
        let conflicts = selected.map_or_else(Vec::new, |selected| {
            evidence
                .iter()
                .filter(|item| !item.value.eq_ignore_ascii_case(&selected.value))
                .cloned()
                .collect()
        });
        LocationConclusion {
            value,
            confidence,
            evidence,
            conflicts,
        }
    }

    pub fn name_at(&self, now: DateTime<Utc>) -> NameConclusion {
        let evidence: Vec<_> = self
            .names
            .iter()
            .filter(|item| item.is_fresh(now))
            .cloned()
            .collect();
        let selected = evidence
            .iter()
            .enumerate()
            .max_by_key(|(index, item)| (item.confidence, item.observed_at, *index))
            .map(|(_, item)| item);
        let value = selected.map(|item| item.value.clone());
        let confidence = selected
            .map(|item| item.confidence)
            .unwrap_or(NameConfidence::Unknown);
        let conflicts = selected.map_or_else(Vec::new, |selected| {
            evidence
                .iter()
                .filter(|item| !item.value.eq_ignore_ascii_case(&selected.value))
                .cloned()
                .collect()
        });
        NameConclusion {
            value,
            confidence,
            evidence,
            conflicts,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpotClass {
    VerifiedMultiplier,
    PredictedMultiplier,
    NeededQso,
    Worked,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Spot {
    pub id: String,
    pub call: String,
    pub frequency_khz: f64,
    pub band: Band,
    pub time: DateTime<Utc>,
    pub spotter: String,
    pub spotters: std::collections::BTreeSet<String>,
    pub snr_db: Option<i16>,
    pub best_snr_db: Option<i16>,
    pub speed_wpm: Option<u16>,
    pub class: SpotClass,
    pub predicted_multiplier: Option<String>,
    pub reports: u32,
    pub preferred_spotter: bool,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub title: String,
    pub station_call: Option<String>,
    pub subtitle: Option<String>,
    pub qso_count: Option<u64>,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub is_naqp: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordDiagnostic {
    pub id: Option<String>,
    pub reason: RecordReason,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecordReason {
    EventOrNonContact,
    MissingCall,
    MissingTimestamp,
    MalformedRecord,
}

#[cfg(test)]
mod evidence_tests {
    use chrono::TimeZone;

    use super::*;

    fn time(minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 1, 18, minute, 0).unwrap()
    }

    fn participation(
        confidence: ParticipationConfidence,
        source: EvidenceSource,
        observed_at: DateTime<Utc>,
        expires_at: Option<DateTime<Utc>>,
    ) -> ParticipationEvidence {
        ParticipationEvidence {
            confidence,
            source,
            observed_at,
            expires_at,
        }
    }

    fn location(
        value: &str,
        confidence: LocationConfidence,
        source: EvidenceSource,
        observed_at: DateTime<Utc>,
        expires_at: Option<DateTime<Utc>>,
    ) -> LocationEvidence {
        LocationEvidence {
            value: value.into(),
            confidence,
            source,
            observed_at,
            expires_at,
        }
    }

    #[test]
    fn participation_and_location_confidence_are_independent() {
        let mut station = StationEvidence::new(" k1abc ");
        station.locations.push(location(
            "CT",
            LocationConfidence::History,
            EvidenceSource::CallHistory,
            time(0),
            None,
        ));

        assert_eq!(station.call, "K1ABC");
        assert_eq!(
            station.participation_at(time(10)).confidence,
            ParticipationConfidence::Unknown
        );
        assert_eq!(
            station.location_at(time(10)).confidence,
            LocationConfidence::History
        );
    }

    #[test]
    fn conclusions_retain_source_and_observation_time() {
        let mut station = StationEvidence::new("K1ABC");
        station.participation.push(participation(
            ParticipationConfidence::Declared,
            EvidenceSource::ContestOnlineScoreboard,
            time(2),
            None,
        ));

        let conclusion = station.participation_at(time(10));
        assert_eq!(conclusion.confidence, ParticipationConfidence::Declared);
        assert_eq!(
            conclusion.evidence[0].source,
            EvidenceSource::ContestOnlineScoreboard
        );
        assert_eq!(conclusion.evidence[0].observed_at, time(2));
    }

    #[test]
    fn completed_local_qso_outranks_predictions_and_retains_conflicts() {
        let mut station = StationEvidence::new("K1ABC");
        station.locations.extend([
            location(
                "MA",
                LocationConfidence::Callbook,
                EvidenceSource::Callbook,
                time(8),
                None,
            ),
            location(
                "CT",
                LocationConfidence::Verified,
                EvidenceSource::LocalQso,
                time(1),
                None,
            ),
        ]);

        let conclusion = station.location_at(time(10));
        assert_eq!(conclusion.value.as_deref(), Some("CT"));
        assert_eq!(conclusion.confidence, LocationConfidence::Verified);
        assert_eq!(conclusion.evidence.len(), 2);
        assert_eq!(conclusion.conflicts.len(), 1);
        assert_eq!(conclusion.conflicts[0].value, "MA");
    }

    #[test]
    fn completed_local_qso_name_outranks_history() {
        let mut station = StationEvidence::new("K1ABC");
        station.names.extend([
            NameEvidence {
                value: "Pat".into(),
                confidence: NameConfidence::History,
                source: EvidenceSource::CallHistory,
                observed_at: time(8),
                expires_at: None,
            },
            NameEvidence {
                value: "Alex".into(),
                confidence: NameConfidence::Verified,
                source: EvidenceSource::LocalQso,
                observed_at: time(1),
                expires_at: None,
            },
        ]);

        let conclusion = station.name_at(time(10));
        assert_eq!(conclusion.value.as_deref(), Some("Alex"));
        assert_eq!(conclusion.confidence, NameConfidence::Verified);
        assert_eq!(conclusion.conflicts[0].value, "Pat");
    }

    #[test]
    fn stale_live_evidence_is_removed_while_static_history_remains() {
        let mut station = StationEvidence::new("K1ABC");
        station.participation.extend([
            participation(
                ParticipationConfidence::Confirmed,
                EvidenceSource::ContestOnlineScoreboard,
                time(0),
                Some(time(5)),
            ),
            participation(
                ParticipationConfidence::Declared,
                EvidenceSource::TeamRegistration,
                time(0),
                None,
            ),
        ]);

        let conclusion = station.participation_at(time(10));
        assert_eq!(conclusion.confidence, ParticipationConfidence::Declared);
        assert_eq!(conclusion.evidence.len(), 1);
        assert_eq!(
            conclusion.evidence[0].source,
            EvidenceSource::TeamRegistration
        );
    }

    #[test]
    fn equal_strength_location_conflicts_resolve_to_newest_observation() {
        let mut station = StationEvidence::new("K1ABC");
        station.locations.extend([
            location(
                "MA",
                LocationConfidence::ContestDeclared,
                EvidenceSource::ContestOnlineScoreboard,
                time(1),
                None,
            ),
            location(
                "CT",
                LocationConfidence::ContestDeclared,
                EvidenceSource::ContestOnlineScoreboard,
                time(2),
                None,
            ),
        ]);

        let conclusion = station.location_at(time(10));
        assert_eq!(conclusion.value.as_deref(), Some("CT"));
        assert_eq!(conclusion.conflicts[0].value, "MA");
    }

    #[test]
    fn empty_evidence_resolves_to_unknown() {
        let station = StationEvidence::new("K1ABC");
        assert_eq!(
            station.participation_at(time(0)).confidence,
            ParticipationConfidence::Unknown
        );
        assert_eq!(station.location_at(time(0)).value, None);
        assert_eq!(
            station.location_at(time(0)).confidence,
            LocationConfidence::Unknown
        );
    }

    #[test]
    fn live_external_sources_are_disabled_by_default() {
        let policy = SourcePolicy::default();
        assert!(!policy.is_enabled(SourceId::ReverseBeaconNetwork));
        assert!(!policy.is_enabled(SourceId::ContestOnlineScoreboard));
        assert!(!policy.requires_assisted_entry());
        assert_eq!(policy.assisted_warning(), None);
    }

    #[test]
    fn any_enabled_live_external_source_requires_assistance() {
        let mut policy = SourcePolicy::default();
        policy.set_enabled(SourceId::ReverseBeaconNetwork, true);
        assert!(policy.requires_assisted_entry());
        assert!(
            policy
                .assisted_warning()
                .unwrap()
                .contains("Reverse Beacon Network")
        );

        policy.set_enabled(SourceId::ReverseBeaconNetwork, false);
        policy.set_enabled(SourceId::ContestOnlineScoreboard, true);
        assert!(policy.requires_assisted_entry());
        assert!(
            policy
                .assisted_warning()
                .unwrap()
                .contains("Contest Online ScoreBoard")
        );
    }

    #[test]
    fn static_history_and_local_logs_do_not_trigger_assisted_category() {
        let mut policy = SourcePolicy::default();
        policy.set_enabled(SourceId::N1mmCallHistory, true);
        policy.set_enabled(SourceId::PoloLofi, true);
        policy.set_enabled(SourceId::AdifImport, true);

        assert!(!policy.requires_assisted_entry());
        assert_eq!(policy.assisted_warning(), None);
        let statuses = policy.statuses();
        assert_eq!(
            statuses
                .iter()
                .find(|source| source.id == SourceId::N1mmCallHistory)
                .unwrap()
                .capability,
            SourceCapability::StaticHistory
        );
        assert_eq!(
            statuses
                .iter()
                .find(|source| source.id == SourceId::PoloLofi)
                .unwrap()
                .capability,
            SourceCapability::LocalLog
        );
    }
}
