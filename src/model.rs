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
pub enum SpotClass {
    VerifiedMultiplier,
    PossibleMultiplier,
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
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub title: String,
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
