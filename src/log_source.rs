use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::Qso;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogSourceKind {
    Adif,
    Lofi,
}

#[derive(Debug)]
pub struct LogUpdate {
    pub source: LogSourceKind,
    pub qsos: Vec<Qso>,
    pub replace: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MergeSummary {
    pub source: LogSourceKind,
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
}

impl LogUpdate {
    pub fn adif(qsos: Vec<Qso>) -> Self {
        Self {
            source: LogSourceKind::Adif,
            qsos,
            replace: false,
        }
    }

    pub fn lofi(qsos: Vec<Qso>, replace: bool) -> Self {
        Self {
            source: LogSourceKind::Lofi,
            qsos,
            replace,
        }
    }

    pub fn apply(self, existing: &mut BTreeMap<String, Qso>) -> MergeSummary {
        let source = self.source;
        let mut next = if self.replace {
            BTreeMap::new()
        } else {
            existing.clone()
        };
        let mut summary = MergeSummary {
            source,
            added: 0,
            updated: 0,
            unchanged: 0,
        };

        for qso in self.qsos {
            match existing.get(&qso.id) {
                None => summary.added += 1,
                Some(old) if materially_equal(old, &qso) => summary.unchanged += 1,
                Some(_) => summary.updated += 1,
            }
            next.insert(qso.id.clone(), qso);
        }

        *existing = next;
        summary
    }
}

fn materially_equal(left: &Qso, right: &Qso) -> bool {
    serde_json::to_value(left).ok() == serde_json::to_value(right).ok()
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::Value;

    use super::*;
    use crate::model::Band;

    fn qso(id: &str, deleted: bool) -> Qso {
        Qso {
            id: id.into(),
            call: "W1AW".into(),
            timestamp: Utc.with_ymd_and_hms(2026, 8, 1, 18, 0, 0).unwrap(),
            band: Some(Band::B20),
            frequency_khz: Some(14_035.0),
            mode: "CW".into(),
            name: Some("AL".into()),
            location: Some("CT".into()),
            country: Some("United States".into()),
            dxcc: Some(291),
            contest_id: Some("NAQP-CW".into()),
            deleted,
            raw: Value::Null,
        }
    }

    #[test]
    fn incremental_updates_replace_qsos_and_tombstones_by_id() {
        let mut existing = BTreeMap::from([("q1".into(), qso("q1", false))]);
        let update = LogUpdate::lofi(vec![qso("q1", true), qso("q2", false)], false);

        let summary = update.apply(&mut existing);

        assert_eq!(summary.updated, 1);
        assert_eq!(summary.added, 1);
        assert_eq!(summary.source, LogSourceKind::Lofi);
        assert!(existing["q1"].deleted);
        assert_eq!(existing.len(), 2);
    }

    #[test]
    fn full_updates_remove_records_absent_from_the_snapshot() {
        let mut existing = BTreeMap::from([("old".into(), qso("old", false))]);

        LogUpdate::lofi(vec![qso("new", false)], true).apply(&mut existing);

        assert!(!existing.contains_key("old"));
        assert!(existing.contains_key("new"));
    }
}
