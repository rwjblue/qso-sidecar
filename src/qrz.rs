use anyhow::{Context, Result, anyhow, bail};
use quick_xml::de::from_str;
use reqwest::Client as HttpClient;
use serde::Deserialize;

const DEFAULT_ENDPOINT: &str = "https://xmldata.qrz.com/xml/current/";

pub struct Config {
    username: String,
    password: String,
    endpoint: String,
}

impl Config {
    pub fn new(username: String, password: String) -> Self {
        Self {
            username,
            password,
            endpoint: DEFAULT_ENDPOINT.into(),
        }
    }

    #[cfg(test)]
    fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }
}

pub struct Client {
    http: HttpClient,
    config: Config,
    session_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    pub call: String,
    pub state: Option<String>,
    pub country: Option<String>,
    pub dxcc: Option<u32>,
    pub grid: Option<String>,
    pub geoloc: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename = "QRZDatabase")]
struct Database {
    #[serde(rename = "Session")]
    session: Option<Session>,
    #[serde(rename = "Callsign")]
    callsign: Option<Callsign>,
}

#[derive(Debug, Deserialize, Default)]
struct Session {
    #[serde(rename = "Key")]
    key: Option<String>,
    #[serde(rename = "Error")]
    error: Option<String>,
    #[serde(rename = "Message")]
    message: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct Callsign {
    #[serde(rename = "call")]
    call: Option<String>,
    #[serde(rename = "state")]
    state: Option<String>,
    #[serde(rename = "land", alias = "country")]
    country: Option<String>,
    #[serde(rename = "dxcc")]
    dxcc: Option<u32>,
    #[serde(rename = "grid")]
    grid: Option<String>,
    #[serde(rename = "geoloc")]
    geoloc: Option<String>,
}

impl Client {
    pub fn new(config: Config) -> Result<Self> {
        Ok(Self {
            http: HttpClient::builder()
                .user_agent(concat!("qso-sidecar/", env!("CARGO_PKG_VERSION")))
                .build()?,
            config,
            session_key: None,
        })
    }

    pub async fn lookup(&mut self, call: &str) -> Result<Option<Lookup>> {
        let call = call.trim().to_ascii_uppercase();
        if call.is_empty() {
            bail!("QRZ lookup callsign cannot be empty");
        }
        if self.session_key.is_none() {
            self.authenticate().await?;
        }
        let mut retried_session = false;
        loop {
            let session_key = self
                .session_key
                .as_deref()
                .ok_or_else(|| anyhow!("QRZ authentication did not return a session key"))?;
            let response = self
                .http
                .get(&self.config.endpoint)
                .query(&[("s", session_key), ("callsign", call.as_str())])
                .send()
                .await
                .context("QRZ callsign request failed")?
                .error_for_status()
                .context("QRZ callsign request returned an HTTP error")?
                .text()
                .await
                .context("could not read QRZ callsign response")?;
            let database = parse_database(&response)?;
            if session_expired(database.session.as_ref()) && !retried_session {
                retried_session = true;
                self.session_key = None;
                self.authenticate().await?;
                continue;
            }
            if let Some(error) = database
                .session
                .as_ref()
                .and_then(|session| session.error.as_deref())
            {
                if error.to_ascii_lowercase().contains("not found") {
                    return Ok(None);
                }
                bail!("QRZ lookup failed: {error}");
            }
            return database.callsign.map(lookup_from).transpose();
        }
    }

    async fn authenticate(&mut self) -> Result<()> {
        let response = self
            .http
            .get(&self.config.endpoint)
            .query(&[
                ("username", self.config.username.as_str()),
                ("password", self.config.password.as_str()),
                ("agent", concat!("qso-sidecar-", env!("CARGO_PKG_VERSION"))),
            ])
            .send()
            .await
            .context("QRZ authentication request failed")?
            .error_for_status()
            .context("QRZ authentication returned an HTTP error")?
            .text()
            .await
            .context("could not read QRZ authentication response")?;
        let database = parse_database(&response)?;
        let session = database
            .session
            .ok_or_else(|| anyhow!("QRZ authentication response omitted Session"))?;
        if let Some(error) = session.error {
            bail!("QRZ authentication failed: {error}");
        }
        self.session_key = session.key.filter(|key| !key.trim().is_empty());
        if self.session_key.is_none() {
            bail!("QRZ authentication did not return a session key");
        }
        Ok(())
    }
}

fn parse_database(xml: &str) -> Result<Database> {
    from_str(xml).context("could not parse QRZ XML response")
}

fn lookup_from(callsign: Callsign) -> Result<Lookup> {
    let call = callsign
        .call
        .filter(|call| !call.trim().is_empty())
        .ok_or_else(|| anyhow!("QRZ callsign response omitted call"))?;
    Ok(Lookup {
        call: call.trim().to_ascii_uppercase(),
        state: cleaned(callsign.state).map(|value| value.to_ascii_uppercase()),
        country: cleaned(callsign.country),
        dxcc: callsign.dxcc,
        grid: cleaned(callsign.grid).map(|value| value.to_ascii_uppercase()),
        geoloc: cleaned(callsign.geoloc),
    })
}

fn cleaned(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn session_expired(session: Option<&Session>) -> bool {
    let text = session
        .and_then(|session| session.error.as_deref().or(session.message.as_deref()))
        .unwrap_or_default()
        .to_ascii_lowercase();
    text.contains("session") && (text.contains("timeout") || text.contains("invalid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reordered_callsign_fields_and_ignores_unknowns() {
        let database = parse_database(
            r#"<?xml version="1.0"?><QRZDatabase version="1.34"><Callsign><grid>FN42</grid><future>ignored</future><state>ma</state><call>K1ABC</call><dxcc>291</dxcc><geoloc>grid</geoloc><land>United States</land></Callsign><Session><Key>abc</Key></Session></QRZDatabase>"#,
        )
        .unwrap();
        let lookup = lookup_from(database.callsign.unwrap()).unwrap();
        assert_eq!(lookup.call, "K1ABC");
        assert_eq!(lookup.state.as_deref(), Some("MA"));
        assert_eq!(lookup.grid.as_deref(), Some("FN42"));
        assert_eq!(lookup.dxcc, Some(291));
    }

    #[test]
    fn detects_expired_session_messages() {
        let session = Session {
            message: Some("Session Timeout".into()),
            ..Session::default()
        };
        assert!(session_expired(Some(&session)));
        assert!(!session_expired(None));
    }

    #[test]
    fn test_config_can_override_endpoint_without_exposing_credentials() {
        let config =
            Config::new("user".into(), "secret".into()).with_endpoint("http://127.0.0.1:1234/");
        assert_eq!(config.endpoint, "http://127.0.0.1:1234/");
    }
}
