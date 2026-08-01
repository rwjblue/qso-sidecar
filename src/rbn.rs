use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Datelike, NaiveTime, TimeZone, Utc};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::model::Band;

const LOGIN_TIMEOUT: Duration = Duration::from_secs(15);

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
    Status {
        state: ConnectionState,
        message: String,
    },
    Spot(RawSpot),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Degraded,
    Disconnected,
}

impl ConnectionState {
    pub fn candidates_are_stale(self) -> bool {
        matches!(self, Self::Degraded | Self::Disconnected)
    }
}

pub trait SpotSource {
    fn parse(&self, line: &str, now: DateTime<Utc>) -> Option<RawSpot>;
}

#[derive(Debug, Default)]
pub struct RbnSpotSource;

impl SpotSource for RbnSpotSource {
    fn parse(&self, line: &str, now: DateTime<Utc>) -> Option<RawSpot> {
        parse_line(line, now)
    }
}

pub async fn run(address: String, login_call: Option<String>, tx: mpsc::Sender<ClusterEvent>) {
    let mut backoff = Duration::from_secs(1);
    let mut entropy = retry_entropy(&address);
    loop {
        let mut connected = false;
        let _ = tx
            .send(ClusterEvent::Status {
                state: ConnectionState::Connecting,
                message: format!("connecting to {address}"),
            })
            .await;
        match connection(
            &address,
            login_call.as_deref(),
            &tx,
            &mut connected,
            LOGIN_TIMEOUT,
        )
        .await
        {
            Ok(()) => warn!(%address, "cluster connection ended"),
            Err(error) => warn!(%address, %error, "cluster connection failed"),
        }
        let state = if connected {
            ConnectionState::Degraded
        } else {
            ConnectionState::Disconnected
        };
        let (base_delay, next_backoff) = retry_backoff(backoff, connected);
        let delay = jittered_delay(base_delay, entropy);
        entropy = next_entropy(entropy);
        let _ = tx
            .send(ClusterEvent::Status {
                state,
                message: format!("disconnected; retrying in {:.1}s", delay.as_secs_f64()),
            })
            .await;
        tokio::time::sleep(delay).await;
        backoff = next_backoff;
    }
}

fn retry_entropy(address: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    address.hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    hasher.finish()
}

fn next_entropy(value: u64) -> u64 {
    value
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407)
}

fn jittered_delay(base: Duration, entropy: u64) -> Duration {
    let percent = 80 + entropy % 41;
    let millis = base.as_millis().saturating_mul(u128::from(percent)) / 100;
    Duration::from_millis(u64::try_from(millis).unwrap_or(u64::MAX))
}

fn retry_backoff(previous: Duration, connected: bool) -> (Duration, Duration) {
    let delay = if connected {
        Duration::from_secs(1)
    } else {
        previous
    };
    (delay, (delay * 2).min(Duration::from_secs(60)))
}

async fn connection(
    address: &str,
    login_call: Option<&str>,
    tx: &mpsc::Sender<ClusterEvent>,
    connected: &mut bool,
    login_timeout: Duration,
) -> Result<()> {
    let stream = TcpStream::connect(address)
        .await
        .with_context(|| format!("connecting to {address}"))?;
    let (read, mut write) = stream.into_split();
    if let Some(call) = login_call {
        write.write_all(call.as_bytes()).await?;
        write.write_all(b"\r\n").await?;
    }
    let source = RbnSpotSource;
    let mut lines = BufReader::new(read).lines();
    let login_deadline = Instant::now() + login_timeout;
    loop {
        let next_line = if *connected {
            lines.next_line().await?
        } else {
            tokio::time::timeout_at(login_deadline, lines.next_line())
                .await
                .map_err(|_| anyhow!("cluster login timed out after {login_timeout:?}"))??
        };
        let Some(line) = next_line else {
            if !*connected {
                bail!("cluster disconnected before login was acknowledged");
            }
            return Ok(());
        };
        let spot = source.parse(&line, Utc::now());
        if !*connected {
            if login_rejected(&line) {
                bail!("cluster rejected the login");
            }
            if spot.is_some() || login_acknowledged(&line, login_call) {
                *connected = true;
                tx.send(ClusterEvent::Status {
                    state: ConnectionState::Connected,
                    message: "connected; live RBN spots enabled".into(),
                })
                .await
                .ok();
                info!(%address, "cluster login established");
            }
        }
        if let Some(spot) = spot {
            if tx.send(ClusterEvent::Spot(spot)).await.is_err() {
                return Ok(());
            }
        } else {
            let safe_line: String = line
                .chars()
                .take(256)
                .map(|character| {
                    if character.is_control() {
                        '\u{fffd}'
                    } else {
                        character
                    }
                })
                .collect();
            debug!(raw_line = %safe_line, "ignored non-CW or malformed cluster line");
        }
    }
}

fn login_acknowledged(line: &str, login_call: Option<&str>) -> bool {
    let normalized = line.to_ascii_lowercase();
    let positive = normalized.contains("welcome") || normalized.contains("hello");
    positive
        && login_call.is_some_and(|call| normalized.contains(&call.trim().to_ascii_lowercase()))
}

fn login_rejected(line: &str) -> bool {
    let normalized = line.to_ascii_lowercase();
    [
        "invalid call",
        "invalid login",
        "login failed",
        "login rejected",
        "access denied",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
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
    let wpm_index = tokens
        .iter()
        .position(|token| token.eq_ignore_ascii_case("WPM"))?;
    let speed_wpm = tokens.get(wpm_index.checked_sub(1)?)?.parse().ok()?;
    if !tokens
        .get(wpm_index + 1)
        .is_some_and(|activity| activity.eq_ignore_ascii_case("CQ"))
    {
        return None;
    }
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
        speed_wpm: Some(speed_wpm),
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

use chrono::Timelike;

#[cfg(test)]
mod tests {
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

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
    fn rejects_non_cw_and_malformed_lines() {
        assert!(parse_line("DX de WZ7I-#: 14025.1 K1ABC 17 dB RTTY 1823Z", now()).is_none());
        assert!(parse_line("cluster login:", now()).is_none());
        assert!(parse_line("DX de WZ7I-#: nope K1ABC 17 dB 26 WPM 1823Z", now()).is_none());
    }

    #[test]
    fn retains_only_unambiguous_cw_cq_reports() {
        assert!(parse_line("DX de WZ7I-#: 14025.1 K1ABC 17 dB 26 WPM cq 1823Z", now()).is_some());
        for activity in ["BEACON", "DX", "TEST"] {
            let line = format!("DX de WZ7I-#: 14025.1 K1ABC 17 dB 26 WPM {activity} 1823Z");
            assert!(parse_line(&line, now()).is_none(), "accepted {activity}");
        }
        assert!(parse_line("DX de WZ7I-#: 14025.1 K1ABC 17 dB 26 WPM 1823Z", now()).is_none());
        assert!(parse_line("DX de WZ7I-#: 14025.1 K1ABC 17 dB RTTY CQ 1823Z", now()).is_none());
    }

    #[test]
    fn disconnected_and_degraded_states_stale_candidates() {
        assert!(!ConnectionState::Connecting.candidates_are_stale());
        assert!(!ConnectionState::Connected.candidates_are_stale());
        assert!(ConnectionState::Degraded.candidates_are_stale());
        assert!(ConnectionState::Disconnected.candidates_are_stale());
    }

    #[test]
    fn successful_connection_resets_retry_backoff() {
        assert_eq!(
            retry_backoff(Duration::from_secs(60), true),
            (Duration::from_secs(1), Duration::from_secs(2))
        );
        assert_eq!(
            retry_backoff(Duration::from_secs(60), false),
            (Duration::from_secs(60), Duration::from_secs(60))
        );
    }

    #[test]
    fn reconnect_jitter_is_deterministic_and_bounded() {
        let base = Duration::from_secs(10);
        assert_eq!(jittered_delay(base, 7), jittered_delay(base, 7));
        for entropy in 0..100 {
            let delay = jittered_delay(base, entropy);
            assert!(delay >= Duration::from_secs(8));
            assert!(delay <= Duration::from_secs(12));
        }
    }

    async fn read_login(stream: &mut TcpStream) -> Vec<u8> {
        let mut login = Vec::new();
        loop {
            let byte = stream.read_u8().await.unwrap();
            login.push(byte);
            if login.ends_with(b"\r\n") {
                return login;
            }
        }
    }

    #[tokio::test]
    async fn socket_handshake_sends_crlf_and_streams_multiple_spots() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (login_tx, login_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream
                .write_all(b"Welcome banner\r\nlogin: ")
                .await
                .unwrap();
            login_tx.send(read_login(&mut stream).await).ok();
            stream
                .write_all(
                    b"Hello N1RWJ, welcome to the cluster\r\n\
                      DX de WZ7I-#: 14025.1 K1ABC 17 dB 26 WPM CQ 1823Z\r\n\
                      DX de VE6JY-#: 7032.4 W2XYZ 12 dB 22 WPM CQ 1824Z\r\n",
                )
                .await
                .unwrap();
        });
        let (tx, mut rx) = mpsc::channel(16);
        let mut connected = false;

        connection(
            &address.to_string(),
            Some("N1RWJ"),
            &tx,
            &mut connected,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        server.await.unwrap();

        assert_eq!(login_rx.await.unwrap(), b"N1RWJ\r\n");
        assert!(connected);
        drop(tx);
        let mut connected_events = 0;
        let mut spots = Vec::new();
        while let Some(event) = rx.recv().await {
            match event {
                ClusterEvent::Status {
                    state: ConnectionState::Connected,
                    ..
                } => connected_events += 1,
                ClusterEvent::Spot(spot) => spots.push(spot.call),
                ClusterEvent::Status { .. } => {}
            }
        }
        assert_eq!(connected_events, 1);
        assert_eq!(spots, ["K1ABC", "W2XYZ"]);
    }

    #[tokio::test]
    async fn first_valid_spot_establishes_login_without_a_banner() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            assert_eq!(read_login(&mut stream).await, b"N1RWJ\r\n");
            stream
                .write_all(b"DX de WZ7I-#: 14025.1 K1ABC 17 dB 26 WPM CQ 1823Z\r\n")
                .await
                .unwrap();
        });
        let (tx, mut rx) = mpsc::channel(8);
        let mut connected = false;

        connection(
            &address.to_string(),
            Some("N1RWJ"),
            &tx,
            &mut connected,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        server.await.unwrap();

        assert!(connected);
        assert!(matches!(
            rx.recv().await,
            Some(ClusterEvent::Status {
                state: ConnectionState::Connected,
                ..
            })
        ));
        assert!(matches!(rx.recv().await, Some(ClusterEvent::Spot(_))));
    }

    #[tokio::test]
    async fn stalled_login_times_out_without_claiming_a_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            assert_eq!(read_login(&mut stream).await, b"N1RWJ\r\n");
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let (tx, mut rx) = mpsc::channel(8);
        let mut connected = false;

        let error = connection(
            &address.to_string(),
            Some("N1RWJ"),
            &tx,
            &mut connected,
            Duration::from_millis(50),
        )
        .await
        .unwrap_err();
        server.abort();

        assert!(error.to_string().contains("login timed out"));
        assert!(!connected);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn rejected_login_reconnects_and_can_become_live() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            assert_eq!(read_login(&mut first).await, b"N1RWJ\r\n");
            first.write_all(b"Login rejected\r\n").await.unwrap();
            drop(first);

            let (mut second, _) = listener.accept().await.unwrap();
            assert_eq!(read_login(&mut second).await, b"N1RWJ\r\n");
            second
                .write_all(b"Hello N1RWJ, welcome to the cluster\r\n")
                .await
                .unwrap();
        });
        let (tx, mut rx) = mpsc::channel(16);
        let client = tokio::spawn(run(address.to_string(), Some("N1RWJ".into()), tx));

        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if matches!(
                    rx.recv().await,
                    Some(ClusterEvent::Status {
                        state: ConnectionState::Connected,
                        ..
                    })
                ) {
                    break;
                }
            }
        })
        .await
        .expect("client did not reconnect and establish a live session");
        client.abort();
        server.await.unwrap();
    }
}
