use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Utc};
use clap::ValueEnum;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::Band;

const NODE_DIRECTORY_URL: &str = "https://www.reversebeacon.net/nodes/detail_json.php";
const CATALOG_MAX_AGE_HOURS: i64 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum SkimmerScope {
    Nearby,
    All,
}

#[derive(Debug, Clone)]
struct Maidenhead {
    value: String,
    latitude: f64,
    longitude: f64,
}

impl Maidenhead {
    fn parse(value: &str) -> Result<Self> {
        let value = value.trim().to_ascii_uppercase();
        ensure!(
            matches!(value.len(), 4 | 6 | 8),
            "Maidenhead grid must contain 4, 6, or 8 characters"
        );
        let bytes = value.as_bytes();
        ensure!(
            matches!(bytes[0], b'A'..=b'R') && matches!(bytes[1], b'A'..=b'R'),
            "Maidenhead grid field must use letters A through R"
        );
        ensure!(
            bytes[2].is_ascii_digit() && bytes[3].is_ascii_digit(),
            "Maidenhead grid square must use digits"
        );
        if bytes.len() >= 6 {
            ensure!(
                matches!(bytes[4], b'A'..=b'X') && matches!(bytes[5], b'A'..=b'X'),
                "Maidenhead grid subsquare must use letters A through X"
            );
        }
        if bytes.len() == 8 {
            ensure!(
                bytes[6].is_ascii_digit() && bytes[7].is_ascii_digit(),
                "Maidenhead extended square must use digits"
            );
        }

        let mut longitude = f64::from(bytes[0] - b'A') * 20.0 - 180.0;
        let mut latitude = f64::from(bytes[1] - b'A') * 10.0 - 90.0;
        longitude += f64::from(bytes[2] - b'0') * 2.0;
        latitude += f64::from(bytes[3] - b'0');
        let (mut width, mut height) = (2.0, 1.0);
        if bytes.len() >= 6 {
            width /= 24.0;
            height /= 24.0;
            longitude += f64::from(bytes[4] - b'A') * width;
            latitude += f64::from(bytes[5] - b'A') * height;
        }
        if bytes.len() == 8 {
            width /= 10.0;
            height /= 10.0;
            longitude += f64::from(bytes[6] - b'0') * width;
            latitude += f64::from(bytes[7] - b'0') * height;
        }

        Ok(Self {
            value,
            latitude: latitude + height / 2.0,
            longitude: longitude + width / 2.0,
        })
    }

    fn distance_km(&self, other: &Self) -> u32 {
        let lat1 = self.latitude.to_radians();
        let lat2 = other.latitude.to_radians();
        let delta_lat = lat2 - lat1;
        let delta_lon = (other.longitude - self.longitude).to_radians();
        let haversine = (delta_lat / 2.0).sin().powi(2)
            + lat1.cos() * lat2.cos() * (delta_lon / 2.0).sin().powi(2);
        (6_371.0 * 2.0 * haversine.sqrt().atan2((1.0 - haversine).sqrt())).round() as u32
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NodeRecord {
    call: String,
    grid: String,
    #[serde(default)]
    band: Value,
}

impl NodeRecord {
    fn bands(&self) -> BTreeSet<Band> {
        let entries: Vec<&Value> = match &self.band {
            Value::Object(entries) => entries.values().collect(),
            Value::Array(entries) => entries.iter().collect(),
            _ => Vec::new(),
        };
        entries
            .into_iter()
            .filter_map(|entry| {
                let fields = entry.as_array()?;
                if !fields.first()?.as_str()?.eq_ignore_ascii_case("CW") {
                    return None;
                }
                Band::from_str(fields.get(1)?.as_str()?).ok()
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedCatalog {
    retrieved_at: DateTime<Utc>,
    nodes: Vec<NodeRecord>,
}

#[derive(Debug, Clone)]
pub struct CatalogCache {
    path: PathBuf,
}

impl CatalogCache {
    pub fn for_app() -> Result<Self> {
        let project = ProjectDirs::from("net", "rwjblue", "qso-sidecar")
            .context("operating system has no application-data directory")?;
        let directory = project.cache_dir();
        fs::create_dir_all(directory).context("creating RBN catalog cache directory")?;
        Ok(Self {
            path: directory.join("rbn-nodes.json"),
        })
    }

    fn load(&self, now: DateTime<Utc>) -> Result<Option<CachedCatalog>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let body = fs::read(&self.path).context("reading cached RBN node catalog")?;
        let catalog: CachedCatalog =
            serde_json::from_slice(&body).context("decoding cached RBN node catalog")?;
        if now - catalog.retrieved_at > chrono::Duration::hours(CATALOG_MAX_AGE_HOURS) {
            return Ok(None);
        }
        Ok(Some(catalog))
    }

    fn save(&self, catalog: &CachedCatalog) -> Result<()> {
        let body = serde_json::to_vec(catalog)?;
        let temporary = self.path.with_extension("tmp");
        fs::write(&temporary, body).context("writing temporary RBN node catalog")?;
        #[cfg(windows)]
        if self.path.exists() {
            fs::remove_file(&self.path).context("removing previous RBN node catalog")?;
        }
        fs::rename(&temporary, &self.path).context("installing RBN node catalog")?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogStatus {
    NotConfigured,
    Loading,
    Live,
    Stale,
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicReceiverSite {
    pub grid: String,
    pub distance_km: u32,
    pub nodes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicBandSelection {
    pub band: Band,
    pub coverage: &'static str,
    pub effective_radius_km: Option<u32>,
    pub sites: Vec<PublicReceiverSite>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicLocality {
    pub configured_scope: SkimmerScope,
    pub effective_scope: SkimmerScope,
    pub station_grid: Option<String>,
    pub catalog_status: CatalogStatus,
    pub catalog_updated_at: Option<DateTime<Utc>>,
    pub message: String,
    pub bands: Vec<PublicBandSelection>,
}

#[derive(Debug, Clone)]
struct ReceiverSite {
    grid: String,
    distance_km: u32,
    nodes: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct LocalMatch {
    pub site_grid: String,
    pub distance_km: u32,
}

#[derive(Debug, Clone)]
pub struct RbnLocality {
    configured_scope: SkimmerScope,
    effective_scope: SkimmerScope,
    station: Option<Maidenhead>,
    nearest_sites: usize,
    catalog_status: CatalogStatus,
    catalog_updated_at: Option<DateTime<Utc>>,
    message: String,
    selections: BTreeMap<Band, Vec<ReceiverSite>>,
}

impl Default for RbnLocality {
    fn default() -> Self {
        Self::new(None, Some(SkimmerScope::All), 3)
            .expect("the default all-RBN locality configuration is valid")
    }
}

impl RbnLocality {
    pub fn new(
        grid: Option<&str>,
        requested_scope: Option<SkimmerScope>,
        nearest_sites: usize,
    ) -> Result<Self> {
        ensure!(
            nearest_sites > 0,
            "nearest RBN site count must be greater than zero"
        );
        let station = grid.map(Maidenhead::parse).transpose()?;
        let configured_scope = requested_scope.unwrap_or(if station.is_some() {
            SkimmerScope::Nearby
        } else {
            SkimmerScope::All
        });
        ensure!(
            configured_scope != SkimmerScope::Nearby || station.is_some(),
            "--rbn-skimmer-scope nearby requires --station-grid"
        );
        let catalog_status = if configured_scope == SkimmerScope::Nearby {
            CatalogStatus::Loading
        } else {
            CatalogStatus::NotConfigured
        };
        let message = if configured_scope == SkimmerScope::Nearby {
            "loading nearby RBN receiver sites; showing all spots meanwhile".into()
        } else {
            "using the full RBN feed".into()
        };
        Ok(Self {
            configured_scope,
            effective_scope: SkimmerScope::All,
            station,
            nearest_sites,
            catalog_status,
            catalog_updated_at: None,
            message,
            selections: BTreeMap::new(),
        })
    }

    pub fn configured_for_nearby(&self) -> bool {
        self.configured_scope == SkimmerScope::Nearby
    }

    pub fn mark_feed_disabled(&mut self) {
        if self.configured_for_nearby() {
            self.catalog_status = CatalogStatus::NotConfigured;
            self.effective_scope = SkimmerScope::All;
            self.message =
                "RBN is disabled; nearby receiver selection will activate with --rbn".into();
        }
    }

    pub fn effective_scope(&self) -> SkimmerScope {
        self.effective_scope
    }

    pub fn install_cached(&mut self, cache: &CatalogCache, now: DateTime<Utc>) -> Result<()> {
        if !self.configured_for_nearby() {
            return Ok(());
        }
        if let Some(catalog) = cache.load(now)? {
            self.install(catalog.nodes, catalog.retrieved_at, CatalogStatus::Stale);
        }
        Ok(())
    }

    fn install(
        &mut self,
        nodes: Vec<NodeRecord>,
        retrieved_at: DateTime<Utc>,
        status: CatalogStatus,
    ) {
        let Some(station) = &self.station else {
            return;
        };
        let mut sites: BTreeMap<String, (Maidenhead, BTreeMap<String, BTreeSet<Band>>)> =
            BTreeMap::new();
        for node in nodes {
            let Ok(grid) = Maidenhead::parse(&node.grid) else {
                continue;
            };
            let bands = node.bands();
            if bands.is_empty() {
                continue;
            }
            sites
                .entry(grid.value.clone())
                .or_insert_with(|| (grid, BTreeMap::new()))
                .1
                .entry(normalize_spotter(&node.call))
                .or_default()
                .extend(bands);
        }

        self.selections = Band::ALL
            .into_iter()
            .map(|band| {
                let mut compatible: Vec<_> = sites
                    .iter()
                    .filter_map(|(grid_name, (grid, nodes))| {
                        let compatible_nodes: BTreeSet<_> = nodes
                            .iter()
                            .filter(|(_, bands)| bands.contains(&band))
                            .map(|(call, _)| call.clone())
                            .collect();
                        (!compatible_nodes.is_empty()).then(|| ReceiverSite {
                            grid: grid_name.clone(),
                            distance_km: station.distance_km(grid),
                            nodes: compatible_nodes,
                        })
                    })
                    .collect();
                compatible.sort_by_key(|site| (site.distance_km, site.grid.clone()));
                compatible.truncate(self.nearest_sites);
                (band, compatible)
            })
            .collect();
        self.catalog_status = status;
        self.catalog_updated_at = Some(retrieved_at);
        self.effective_scope = SkimmerScope::Nearby;
        self.message = match status {
            CatalogStatus::Stale => "using cached nearby RBN receiver sites".into(),
            _ => "filtering candidates through nearby RBN receiver sites".into(),
        };
    }

    pub fn mark_unavailable(&mut self, message: impl Into<String>) {
        if self.selections.is_empty() {
            self.effective_scope = SkimmerScope::All;
            self.catalog_status = CatalogStatus::Unavailable;
            self.message = format!(
                "RBN receiver directory unavailable; showing all spots: {}",
                message.into()
            );
        } else {
            self.catalog_status = CatalogStatus::Stale;
            self.message = format!("using cached RBN receiver sites: {}", message.into());
        }
    }

    pub fn match_spotter(&self, band: Band, spotter: &str) -> Option<LocalMatch> {
        if self.effective_scope != SkimmerScope::Nearby {
            return None;
        }
        let spotter = normalize_spotter(spotter);
        self.selections.get(&band)?.iter().find_map(|site| {
            site.nodes.contains(&spotter).then(|| LocalMatch {
                site_grid: site.grid.clone(),
                distance_km: site.distance_km,
            })
        })
    }

    pub fn public(&self) -> PublicLocality {
        PublicLocality {
            configured_scope: self.configured_scope,
            effective_scope: self.effective_scope,
            station_grid: self.station.as_ref().map(|grid| grid.value.clone()),
            catalog_status: self.catalog_status,
            catalog_updated_at: self.catalog_updated_at,
            message: self.message.clone(),
            bands: Band::ALL
                .into_iter()
                .map(|band| {
                    let sites = self.selections.get(&band).cloned().unwrap_or_default();
                    let nearby_count = sites.iter().filter(|site| site.distance_km <= 500).count();
                    let coverage = if nearby_count >= self.nearest_sites.min(3) {
                        "good"
                    } else if nearby_count > 0 {
                        "limited"
                    } else if sites.is_empty() {
                        "unavailable"
                    } else {
                        "distant"
                    };
                    PublicBandSelection {
                        band,
                        coverage,
                        effective_radius_km: sites.iter().map(|site| site.distance_km).max(),
                        sites: sites
                            .into_iter()
                            .map(|site| PublicReceiverSite {
                                grid: site.grid,
                                distance_km: site.distance_km,
                                nodes: site.nodes.into_iter().collect(),
                            })
                            .collect(),
                    }
                })
                .collect(),
        }
    }

    #[cfg(test)]
    pub(crate) fn install_test_nodes(
        &mut self,
        nodes: &[(&str, &str, &[Band])],
        now: DateTime<Utc>,
    ) {
        let nodes = nodes
            .iter()
            .map(|(call, grid, bands)| NodeRecord {
                call: (*call).into(),
                grid: (*grid).into(),
                band: Value::Array(
                    bands
                        .iter()
                        .map(|band| {
                            Value::Array(vec![
                                Value::String("CW".into()),
                                Value::String(band.to_string()),
                                Value::String("test".into()),
                            ])
                        })
                        .collect(),
                ),
            })
            .collect();
        self.install(nodes, now, CatalogStatus::Live);
    }
}

pub async fn refresh(locality: &mut RbnLocality, cache: &CatalogCache) -> Result<()> {
    if !locality.configured_for_nearby() {
        return Ok(());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("qso-sidecar/0.1 RBN node directory")
        .build()?;
    let response = client
        .get(NODE_DIRECTORY_URL)
        .send()
        .await
        .context("fetching RBN node directory")?
        .error_for_status()
        .context("RBN node directory returned an error")?;
    let value: Value = response
        .json()
        .await
        .context("decoding RBN node directory")?;
    let entries = value
        .as_array()
        .context("RBN node directory is not an array")?;
    let nodes: Vec<NodeRecord> = entries
        .iter()
        .filter_map(|entry| serde_json::from_value(entry.clone()).ok())
        .filter(|node: &NodeRecord| !node.call.trim().is_empty())
        .collect();
    if nodes.is_empty() {
        bail!("RBN node directory contained no usable nodes");
    }
    let catalog = CachedCatalog {
        retrieved_at: Utc::now(),
        nodes,
    };
    cache.save(&catalog)?;
    locality.install(catalog.nodes, catalog.retrieved_at, CatalogStatus::Live);
    Ok(())
}

pub fn normalize_spotter(spotter: &str) -> String {
    spotter
        .trim()
        .trim_end_matches("-#")
        .trim_end_matches('#')
        .trim_end_matches('-')
        .to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(call: &str, grid: &str, bands: &[&str]) -> NodeRecord {
        NodeRecord {
            call: call.into(),
            grid: grid.into(),
            band: Value::Array(
                bands
                    .iter()
                    .map(|band| {
                        Value::Array(vec![
                            Value::String("CW".into()),
                            Value::String((*band).into()),
                            Value::String("test".into()),
                        ])
                    })
                    .collect(),
            ),
        }
    }

    #[test]
    fn validates_and_centers_maidenhead_grids() {
        let grid = Maidenhead::parse("fn31pr").unwrap();
        assert_eq!(grid.value, "FN31PR");
        assert!((grid.latitude - 41.729).abs() < 0.01);
        assert!((grid.longitude - -72.708).abs() < 0.01);
        assert!(Maidenhead::parse("FN3").is_err());
        assert!(Maidenhead::parse("SN31PR").is_err());
        assert!(Maidenhead::parse("FN31ZZ").is_err());
    }

    #[test]
    fn selects_nearest_distinct_sites_per_band() {
        let mut locality = RbnLocality::new(Some("FN31PR"), None, 2).unwrap();
        locality.install(
            vec![
                node("ONE-#", "FN31JG", &["20m"]),
                node("ONE-2", "FN31JG", &["20m", "40m"]),
                node("TWO", "FN42ET", &["20m", "40m"]),
                node("THREE", "EM00AA", &["20m", "40m"]),
            ],
            Utc::now(),
            CatalogStatus::Live,
        );
        let twenty = locality
            .public()
            .bands
            .into_iter()
            .find(|b| b.band == Band::B20)
            .unwrap();
        assert_eq!(twenty.sites.len(), 2);
        assert_eq!(twenty.sites[0].nodes, vec!["ONE", "ONE-2"]);
        assert!(locality.match_spotter(Band::B20, "ONE-#").is_some());
        assert!(locality.match_spotter(Band::B20, "THREE-#").is_none());
        assert!(locality.match_spotter(Band::B40, "ONE-2-#").is_some());
        assert!(locality.match_spotter(Band::B40, "ONE-#").is_none());
    }

    #[test]
    fn nearby_without_a_grid_is_rejected_and_all_scope_needs_no_catalog() {
        assert!(RbnLocality::new(None, Some(SkimmerScope::Nearby), 3).is_err());
        let locality = RbnLocality::new(None, None, 3).unwrap();
        assert_eq!(locality.effective_scope(), SkimmerScope::All);
        assert!(!locality.configured_for_nearby());
    }
}
