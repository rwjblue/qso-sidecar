use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use chrono::{TimeZone, Utc};
use directories::ProjectDirs;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::model::{Band, Operation, Qso};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Credentials {
    key: String,
    secret: String,
    token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Registration {
    pub linked: bool,
    pub account_call: Option<String>,
}

#[derive(Clone)]
pub struct LofiClient {
    http: reqwest::Client,
    base: String,
    credentials: Arc<Mutex<Credentials>>,
    credentials_path: PathBuf,
}

impl LofiClient {
    pub fn new(base: String) -> Result<Self> {
        let project = ProjectDirs::from("net", "rwjblue", "qso-sidecar")
            .context("operating system has no application-data directory")?;
        let directory = project.data_local_dir();
        secure_directory(directory)?;
        let credentials_path = directory.join("lofi-credentials.json");
        let credentials = load_or_create_credentials(&credentials_path)?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("qso-sidecar/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(15))
            .build()?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            credentials: Arc::new(Mutex::new(credentials)),
            credentials_path,
        })
    }

    pub async fn register(&self) -> Result<Registration> {
        let mut credentials = self.credentials.lock().await;
        let response = self
            .http
            .post(format!("{}/v1/client", self.base))
            .json(&json!({
                "client": {
                    "name": "QSO Sidecar",
                    "type": "browser",
                    "key": credentials.key,
                    "secret": credentials.secret,
                }
            }))
            .send()
            .await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            credentials.key = format!("browser:{}", Uuid::new_v4());
            credentials.secret = Uuid::new_v4().to_string();
            credentials.token = None;
            save_credentials(&self.credentials_path, &credentials)?;
            bail!("saved LoFi client identity was rejected; created a new identity, retry linking")
        }
        let response = checked_json(response).await?;
        credentials.token = response
            .get("token")
            .and_then(Value::as_str)
            .map(str::to_string);
        save_credentials(&self.credentials_path, &credentials)?;
        let account = response.get("account").filter(|value| !value.is_null());
        Ok(Registration {
            linked: account.is_some(),
            account_call: account
                .and_then(|value| value.get("call"))
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    pub async fn link_email(&self, email: &str) -> Result<()> {
        let body = json!({"email": email.trim(), "send_email": true});
        self.post_protected("/v1/client/permissions", &body).await?;
        Ok(())
    }

    pub async fn account(&self) -> Result<Registration> {
        let response = self.get_protected("/v1/accounts", &[]).await?;
        let account = response
            .get("current_account")
            .filter(|value| !value.is_null());
        Ok(Registration {
            linked: account.is_some(),
            account_call: account
                .and_then(|value| value.get("call"))
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    pub async fn operations(&self, synced_since: Option<i64>) -> Result<Vec<Operation>> {
        let now = Utc::now().timestamp_millis().to_string();
        let since = synced_since.map(|value| value.to_string());
        let query = if let Some(ref value) = since {
            vec![("syncedSinceMillis", value.as_str()), ("limit", "100")]
        } else {
            vec![("startedUntilMillis", now.as_str()), ("limit", "100")]
        };
        let response = self.get_protected("/v1/operations", &query).await?;
        Ok(response
            .get("operations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(parse_operation)
            .collect())
    }

    pub async fn qsos(&self, operation_id: &str, synced_since: Option<i64>) -> Result<Vec<Qso>> {
        let path = format!("/v1/operations/{operation_id}/qsos");
        let mut all = Vec::new();
        let mut cursor: Option<i64> = None;
        loop {
            let since = synced_since.map(|value| value.to_string());
            let until = cursor.map(|value| value.to_string());
            let mut query = vec![("limit", if cursor.is_some() { "200" } else { "50" })];
            if let Some(ref value) = since {
                query.push(("syncedSinceMillis", value.as_str()));
            }
            if let Some(ref value) = until {
                query.push(("syncedUntilMillis", value.as_str()));
            }
            let response = self.get_protected(&path, &query).await?;
            all.extend(
                response
                    .get("qsos")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(parse_qson),
            );
            let meta = response.pointer("/meta/qsos");
            let records_left = meta
                .and_then(|value| value.get("records_left"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let next = meta
                .and_then(|value| value.get("next_synced_until_millis"))
                .and_then(Value::as_i64);
            if records_left <= 0 || next.is_none() || next == cursor {
                break;
            }
            cursor = next;
        }
        Ok(all)
    }

    async fn get_protected(&self, path: &str, query: &[(&str, &str)]) -> Result<Value> {
        for attempt in 0..2 {
            let token = self.token().await?;
            let response = self
                .http
                .get(format!("{}{}", self.base, path))
                .bearer_auth(token)
                .query(query)
                .send()
                .await?;
            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.register().await?;
                continue;
            }
            return checked_json(response).await;
        }
        unreachable!()
    }

    async fn post_protected(&self, path: &str, body: &Value) -> Result<Value> {
        for attempt in 0..2 {
            let token = self.token().await?;
            let response = self
                .http
                .post(format!("{}{}", self.base, path))
                .bearer_auth(token)
                .json(body)
                .send()
                .await?;
            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.register().await?;
                continue;
            }
            return checked_json(response).await;
        }
        unreachable!()
    }

    async fn token(&self) -> Result<String> {
        if let Some(token) = self.credentials.lock().await.token.clone() {
            return Ok(token);
        }
        self.register().await?;
        self.credentials
            .lock()
            .await
            .token
            .clone()
            .context("LoFi registration returned no bearer token")
    }
}

async fn checked_json(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        bail!("LoFi HTTP {status}: {}", truncate(&body, 300));
    }
    if body.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&body).context("LoFi returned invalid JSON")
}

fn parse_operation(value: &Value) -> Option<Operation> {
    if value.get("deleted").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let id = scalar_string(value.get("uuid")?)?;
    let is_naqp = value
        .get("refs")
        .and_then(Value::as_array)
        .is_some_and(|refs| {
            refs.iter().any(|item| {
                item.get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("naqp"))
            })
        });
    Some(Operation {
        id,
        title: value
            .get("userTitle")
            .or_else(|| value.get("title"))
            .or_else(|| value.get("broaderTitle"))
            .and_then(Value::as_str)
            .unwrap_or("Untitled operation")
            .to_string(),
        start: millis(value, &["startAtMillisMin", "createdAtMillis"]),
        end: millis(value, &["startAtMillisMax"]),
        is_naqp,
    })
}

fn parse_qson(value: &Value) -> Option<Qso> {
    if value.get("event").is_some() {
        return None;
    }
    let id = scalar_string(value.get("uuid")?)?;
    let call = value
        .pointer("/their/call")
        .or_else(|| value.pointer("/their/baseCall"))
        .and_then(Value::as_str)?
        .trim()
        .to_ascii_uppercase();
    let timestamp = millis(value, &["startAtMillis", "endAtMillis"]).or_else(|| {
        value
            .get("startAt")
            .and_then(Value::as_str)
            .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
            .map(|time| time.with_timezone(&Utc))
    })?;
    let frequency_khz = value.get("freq").and_then(Value::as_f64).map(|frequency| {
        if frequency > 1_000_000.0 {
            frequency / 1_000.0
        } else if frequency < 1_000.0 {
            frequency * 1_000.0
        } else {
            frequency
        }
    });
    let band = value
        .get("band")
        .and_then(Value::as_str)
        .and_then(|band| Band::from_str(band).ok())
        .or_else(|| frequency_khz.and_then(Band::from_frequency_khz));
    let naqp_ref = value
        .get("refs")
        .and_then(Value::as_array)
        .and_then(|refs| {
            refs.iter().find(|item| {
                item.get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("naqp"))
            })
        });
    let raw_exchange = value.pointer("/their/exchange").and_then(Value::as_str);
    let (exchange_name, exchange_location) =
        raw_exchange.map(split_exchange).unwrap_or((None, None));
    let name = naqp_ref
        .and_then(|item| item.get("name"))
        .or_else(|| value.pointer("/their/name"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(exchange_name);
    let location = naqp_ref
        .and_then(|item| item.get("location"))
        .or_else(|| value.pointer("/their/state"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(exchange_location);

    Some(Qso {
        id,
        call,
        timestamp,
        band,
        frequency_khz,
        mode: value
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_uppercase(),
        name,
        location,
        country: value
            .pointer("/their/country")
            .and_then(Value::as_str)
            .map(str::to_string),
        dxcc: value
            .pointer("/their/dxccCode")
            .and_then(Value::as_u64)
            .and_then(|number| u32::try_from(number).ok()),
        contest_id: Some("NAQP-CW".into()),
        deleted: value.get("deleted").and_then(Value::as_bool) == Some(true),
        raw: value.clone(),
    })
}

fn split_exchange(value: &str) -> (Option<String>, Option<String>) {
    let mut pieces = value.split_whitespace();
    (
        pieces.next().map(str::to_string),
        pieces.next().map(str::to_string),
    )
}

fn scalar_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_u64().map(|value| value.to_string()))
}

fn millis(value: &Value, keys: &[&str]) -> Option<chrono::DateTime<Utc>> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_i64)
            .and_then(|time| Utc.timestamp_millis_opt(time).single())
    })
}

fn secure_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn load_or_create_credentials(path: &Path) -> Result<Credentials> {
    if path.exists() {
        let body = fs::read(path)?;
        return serde_json::from_slice(&body).context("reading saved LoFi client identity");
    }
    let credentials = Credentials {
        key: format!("browser:{}", Uuid::new_v4()),
        secret: Uuid::new_v4().to_string(),
        token: None,
    };
    save_credentials(path, &credentials)?;
    Ok(credentials)
}

fn save_credentials(path: &Path, credentials: &Credentials) -> Result<()> {
    let temporary = path.with_extension("tmp");
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(&serde_json::to_vec(credentials)?)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn truncate(value: &str, max: usize) -> String {
    let mut output: String = value.chars().take(max).collect();
    if value.chars().count() > max {
        output.push('…');
    }
    output
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_flexible_operation_response() {
        let operation = parse_operation(&json!({
            "uuid": 42,
            "userTitle": "NAQP CW",
            "refs": [{"type": "naqp", "mode": "CW", "extra": true}],
            "unknown": {"is": "preserved by caller"}
        }))
        .unwrap();
        assert_eq!(operation.id, "42");
        assert!(operation.is_naqp);
    }

    #[test]
    fn parses_qson_and_preserves_unknown_fields() {
        let qso = parse_qson(&json!({
            "uuid": "q1",
            "their": {"call": "w1aw", "country": "United States"},
            "band": "20m",
            "mode": "CW",
            "startAtMillis": 1785607200000_i64,
            "refs": [{"type": "naqp", "name": "AL", "location": "CT"}],
            "futureField": {"value": 7}
        }))
        .unwrap();
        assert_eq!(qso.call, "W1AW");
        assert_eq!(qso.location.as_deref(), Some("CT"));
        assert_eq!(qso.raw["futureField"]["value"], 7);
    }

    #[test]
    fn skips_event_records_and_keeps_tombstones() {
        assert!(parse_qson(&json!({"uuid":"event", "event":"break"})).is_none());
        let deleted = parse_qson(&json!({
            "uuid":"q1", "their":{"call":"W1AW"}, "band":"20m", "mode":"CW",
            "startAtMillis":1785607200000_i64, "deleted":true
        }))
        .unwrap();
        assert!(deleted.deleted);
    }
}
