use std::time::Duration;

#[cfg(test)]
use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, NaiveTime, TimeZone, Utc};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::model::Band;

#[derive(Debug, Clone, Serialize)]
pub struct RawSpot {
    pub call: String,
    pub frequency_khz: f64,
    pub band: Band,
    pub time: DateTime<Utc>,
    pub spotter: String,
    pub snr_db: Option<i16>,
    pub speed_wpm: Option<u16>,
}

#[derive(Debug)]
pub enum ClusterEvent {
    Status(String),
    Spot(RawSpot),
}

pub async fn run(address: String, login_call: Option<String>, tx: mpsc::Sender<ClusterEvent>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        let _ = tx
            .send(ClusterEvent::Status(format!("connecting to {address}")))
            .await;
        match connection(&address, login_call.as_deref(), &tx).await {
            Ok(()) => warn!(%address, "cluster connection ended"),
            Err(error) => warn!(%address, %error, "cluster connection failed"),
        }
        let _ = tx
            .send(ClusterEvent::Status(format!(
                "disconnected; retrying in {}s",
                backoff.as_secs()
            )))
            .await;
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

async fn connection(
    address: &str,
    login_call: Option<&str>,
    tx: &mpsc::Sender<ClusterEvent>,
) -> Result<()> {
    let stream = TcpStream::connect(address)
        .await
        .with_context(|| format!("connecting to {address}"))?;
    let (read, mut write) = stream.into_split();
    if let Some(call) = login_call {
        write.write_all(call.as_bytes()).await?;
        write.write_all(b"\r\n").await?;
    }
    tx.send(ClusterEvent::Status(
        "connected; live RBN spots enabled".into(),
    ))
    .await
    .ok();
    info!(%address, "cluster connected");

    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        if let Some(spot) = parse_line(&line, Utc::now())
            && tx.send(ClusterEvent::Spot(spot)).await.is_err()
        {
            return Ok(());
        }
    }
    Ok(())
}

pub fn parse_line(line: &str, now: DateTime<Utc>) -> Option<RawSpot> {
    let tokens: Vec<_> = line.split_whitespace().collect();
    if tokens.len() < 5 || !tokens[0].eq_ignore_ascii_case("DX") || tokens[1] != "de" {
        return None;
    }
    let spotter = tokens[2].trim_end_matches(':').to_ascii_uppercase();
    let frequency_khz: f64 = tokens[3].parse().ok()?;
    let band = Band::from_frequency_khz(frequency_khz)?;
    let call = tokens[4].trim().to_ascii_uppercase();
    if !is_plausible_call(&call) {
        return None;
    }

    let snr_db = tokens
        .windows(2)
        .find(|pair| pair[1].eq_ignore_ascii_case("dB"))
        .and_then(|pair| pair[0].parse().ok());
    let speed_wpm = tokens
        .windows(2)
        .find(|pair| pair[1].eq_ignore_ascii_case("WPM"))
        .and_then(|pair| pair[0].parse().ok());
    let time = tokens
        .iter()
        .find_map(|token| parse_hhmm(token, now))
        .unwrap_or(now);

    Some(RawSpot {
        call,
        frequency_khz,
        band,
        time,
        spotter,
        snr_db,
        speed_wpm,
    })
}

fn parse_hhmm(token: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let digits = token.strip_suffix('Z')?;
    if digits.len() != 4 {
        return None;
    }
    let time = NaiveTime::parse_from_str(digits, "%H%M").ok()?;
    let mut result = Utc
        .with_ymd_and_hms(
            now.year(),
            now.month(),
            now.day(),
            time.hour(),
            time.minute(),
            0,
        )
        .single()?;
    if result - now > chrono::Duration::hours(12) {
        result -= chrono::Duration::days(1);
    } else if now - result > chrono::Duration::hours(12) {
        result += chrono::Duration::days(1);
    }
    Some(result)
}

fn is_plausible_call(call: &str) -> bool {
    call.len() >= 3
        && call.len() <= 16
        && call
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || value == '/')
        && call.chars().any(|value| value.is_ascii_alphabetic())
        && call.chars().any(|value| value.is_ascii_digit())
}

#[cfg(test)]
#[derive(Debug, Default)]
pub struct Deduplicator {
    seen: HashMap<(String, Band), DateTime<Utc>>,
}

#[cfg(test)]
impl Deduplicator {
    pub fn accept(&mut self, spot: &RawSpot, window: chrono::Duration) -> bool {
        self.seen
            .retain(|_, time| spot.time.signed_duration_since(*time) <= window * 3);
        let key = (spot.call.clone(), spot.band);
        let duplicate = self
            .seen
            .get(&key)
            .is_some_and(|time| spot.time.signed_duration_since(*time).abs() <= window);
        self.seen.insert(key, spot.time);
        !duplicate
    }
}

use chrono::Timelike;

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 1, 18, 30, 0).unwrap()
    }

    #[test]
    fn parses_reverse_beacon_line() {
        let spot = parse_line(
            "DX de WZ7I-#:  14025.1  K1ABC       17 dB  26 WPM  CQ      1823Z",
            now(),
        )
        .unwrap();
        assert_eq!(spot.call, "K1ABC");
        assert_eq!(spot.band, Band::B20);
        assert_eq!(spot.spotter, "WZ7I-#");
        assert_eq!(spot.snr_db, Some(17));
        assert_eq!(spot.speed_wpm, Some(26));
        assert_eq!(spot.time.minute(), 23);
    }

    #[test]
    fn rejects_non_contest_bands() {
        assert!(parse_line("DX de WZ7I-#:  10125.1 K1ABC 12 dB 20 WPM 1823Z", now()).is_none());
    }

    #[test]
    fn deduplicates_call_on_band_inside_window() {
        let first = parse_line("DX de WZ7I-#: 14025.1 K1ABC 17 dB 26 WPM 1823Z", now()).unwrap();
        let mut second = first.clone();
        second.time += chrono::Duration::seconds(30);
        let mut dedupe = Deduplicator::default();
        assert!(dedupe.accept(&first, chrono::Duration::seconds(90)));
        assert!(!dedupe.accept(&second, chrono::Duration::seconds(90)));
    }
}
