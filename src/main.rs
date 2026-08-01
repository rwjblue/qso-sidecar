mod adif;
mod lofi;
mod log_source;
mod model;
mod naqp;
mod naqp_catalog;
mod rbn;
mod storage;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, ensure};
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, TimeZone, Utc};
use clap::{Parser, ValueEnum};
use futures_util::Stream;
use model::{Band, Operation, Qso, Spot, SpotClass};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{RwLock, broadcast, mpsc};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::trace::TraceLayer;
use tracing::{error, info, info_span, warn};
use tracing_subscriber::EnvFilter;

const LOFI_LINK_POLL_INTERVAL: Duration = Duration::from_secs(8);
const LOFI_SYNC_POLL_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Serve synthetic contest data; no real log data is exposed in the dashboard.
    #[arg(long, env = "QSO_SIDECAR_DEMO")]
    demo: bool,
    /// Synthetic state to load with --demo.
    #[arg(
        long,
        value_enum,
        default_value_t = DemoScenario::Normal,
        env = "QSO_SIDECAR_DEMO_SCENARIO"
    )]
    demo_scenario: DemoScenario,
    /// Loopback HTTP port.
    #[arg(long, default_value_t = 7878, env = "QSO_SIDECAR_PORT")]
    port: u16,
    /// CW DX-cluster host and port.
    #[arg(
        long,
        default_value = "telnet.reversebeacon.net:7000",
        env = "QSO_SIDECAR_CLUSTER"
    )]
    cluster: String,
    /// Callsign sent to the DX-cluster login prompt. Required when RBN is enabled.
    #[arg(long, env = "QSO_SIDECAR_CALL")]
    call: Option<String>,
    /// Enable live RBN spots (requires a Single Operator Assisted entry).
    #[arg(long, env = "QSO_SIDECAR_RBN")]
    rbn: bool,
    /// Minutes before an RBN candidate expires.
    #[arg(long, default_value_t = 10, env = "QSO_SIDECAR_SPOT_TTL_MINUTES")]
    spot_ttl_minutes: u64,
    /// Seconds in which nearby reports for the same call and band are aggregated.
    #[arg(long, default_value_t = 90, env = "QSO_SIDECAR_SPOT_DEDUPE_SECONDS")]
    spot_dedupe_seconds: u64,
    /// Maximum frequency separation for aggregating reports, in kHz.
    #[arg(long, default_value_t = 1.0, env = "QSO_SIDECAR_SPOT_DEDUPE_KHZ")]
    spot_dedupe_khz: f64,
    /// Maximum number of RBN candidates retained in memory.
    #[arg(long, default_value_t = 200, env = "QSO_SIDECAR_SPOT_CAPACITY")]
    spot_capacity: usize,
    /// Override the LoFi API base for development.
    #[arg(
        long,
        default_value = "https://lofi.ham2k.net",
        env = "QSO_SIDECAR_LOFI_BASE"
    )]
    lofi_base: String,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum DemoScenario {
    #[default]
    Normal,
    NoLog,
    StaleAdif,
    LofiUnavailable,
    RbnDisconnected,
    MalformedImport,
    UnresolvedExchange,
}

#[derive(Debug, Clone, Copy)]
struct SpotPolicy {
    ttl: chrono::Duration,
    dedupe_window: chrono::Duration,
    dedupe_khz: f64,
    capacity: usize,
}

impl Default for SpotPolicy {
    fn default() -> Self {
        Self {
            ttl: chrono::Duration::minutes(10),
            dedupe_window: chrono::Duration::seconds(90),
            dedupe_khz: 1.0,
            capacity: 200,
        }
    }
}

impl Args {
    fn spot_policy(&self) -> Result<SpotPolicy> {
        ensure!(
            self.spot_ttl_minutes > 0,
            "spot TTL must be greater than zero"
        );
        ensure!(
            self.spot_dedupe_seconds > 0,
            "spot dedupe window must be greater than zero"
        );
        ensure!(
            self.spot_dedupe_khz.is_finite() && self.spot_dedupe_khz > 0.0,
            "spot dedupe frequency must be a positive finite number"
        );
        ensure!(
            self.spot_capacity > 0,
            "spot capacity must be greater than zero"
        );
        Ok(SpotPolicy {
            ttl: chrono::Duration::minutes(i64::try_from(self.spot_ttl_minutes)?),
            dedupe_window: chrono::Duration::seconds(i64::try_from(self.spot_dedupe_seconds)?),
            dedupe_khz: self.spot_dedupe_khz,
            capacity: self.spot_capacity,
        })
    }
}

fn validate_rbn_config(rbn_enabled: bool, call: Option<&str>) -> Result<()> {
    ensure!(
        !rbn_enabled || call.is_some_and(|call| !call.trim().is_empty()),
        "a login callsign is required when RBN is enabled; pass --call <CALL> with --rbn"
    );
    Ok(())
}

fn http_trace_path(uri: &Uri) -> &str {
    uri.path()
}

#[derive(Clone)]
struct AppState {
    runtime: Arc<RwLock<Runtime>>,
    updates: broadcast::Sender<()>,
    shutdown: broadcast::Sender<()>,
    lofi: lofi::LofiClient,
    store: storage::StateStore,
}

#[derive(Debug)]
struct Runtime {
    qsos: BTreeMap<String, Qso>,
    spots: VecDeque<Spot>,
    operations: Vec<Operation>,
    selected_operation: Option<String>,
    source: String,
    source_kind: Option<log_source::LogSourceKind>,
    source_freshness: Option<DateTime<Utc>>,
    lofi_status: String,
    lofi_account_call: Option<String>,
    import_diagnostics: adif::ImportDiagnostics,
    source_diagnostics: Vec<model::RecordDiagnostic>,
    spot_status: String,
    spots_enabled: bool,
    spot_policy: SpotPolicy,
    demo: bool,
}

#[derive(Debug, Serialize)]
struct PublicState {
    api_version: u8,
    generated_at: DateTime<Utc>,
    contest: naqp::ContestRules,
    score: naqp::Score,
    multiplier_matrix: Vec<MatrixRow>,
    spots: Vec<Spot>,
    operations: Vec<Operation>,
    selected_operation: Option<String>,
    source: String,
    source_kind: Option<log_source::LogSourceKind>,
    source_freshness: Option<DateTime<Utc>>,
    lofi_status: String,
    lofi_linked: bool,
    lofi_account_call: Option<String>,
    import_diagnostics: adif::ImportDiagnostics,
    source_diagnostics: Vec<model::RecordDiagnostic>,
    spot_status: String,
    spots_enabled: bool,
    assisted_warning: Option<&'static str>,
    demo: bool,
    current_band: Option<Band>,
}

#[derive(Debug, Serialize)]
struct MatrixRow {
    id: &'static str,
    code: &'static str,
    display_name: &'static str,
    group: naqp_catalog::MultiplierGroup,
    cells: Vec<MatrixCell>,
}

#[derive(Debug, Serialize)]
struct MatrixCell {
    band: Band,
    state: MatrixCellState,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MatrixCellState {
    Needed,
    PossibleSpotted,
    VerifiedSpotted,
    Unresolved,
    Worked,
}

impl Runtime {
    fn normal(
        spots_enabled: bool,
        spot_policy: SpotPolicy,
        restored: Option<storage::PersistedState>,
    ) -> Self {
        let spot_status = if spots_enabled {
            "starting".into()
        } else {
            "disabled — Single Operator safe".into()
        };
        if let Some(restored) = restored {
            return Self {
                qsos: restored.qsos,
                spots: VecDeque::new(),
                operations: Vec::new(),
                selected_operation: restored.selected_operation,
                source: format!("Restored last-good: {}", restored.source),
                source_kind: restored.source_kind,
                source_freshness: restored.source_freshness,
                lofi_status: "starting LoFi client registration".into(),
                lofi_account_call: None,
                import_diagnostics: restored.import_diagnostics,
                source_diagnostics: restored.source_diagnostics,
                spot_status,
                spots_enabled,
                spot_policy,
                demo: false,
            };
        }
        Self {
            qsos: BTreeMap::new(),
            spots: VecDeque::new(),
            operations: Vec::new(),
            selected_operation: None,
            source: "Waiting for PoLo data".into(),
            source_kind: None,
            source_freshness: None,
            lofi_status: "starting LoFi client registration".into(),
            lofi_account_call: None,
            import_diagnostics: adif::ImportDiagnostics::default(),
            source_diagnostics: Vec::new(),
            spot_status,
            spots_enabled,
            spot_policy,
            demo: false,
        }
    }

    fn persisted(
        &self,
        qsos: BTreeMap<String, Qso>,
        source_kind: log_source::LogSourceKind,
        source: String,
        source_freshness: DateTime<Utc>,
        source_diagnostics: Vec<model::RecordDiagnostic>,
    ) -> storage::PersistedState {
        storage::PersistedState::new(
            qsos,
            self.selected_operation.clone(),
            Some(source_kind),
            source,
            Some(source_freshness),
            self.import_diagnostics.clone(),
            source_diagnostics,
        )
    }

    fn public(&self) -> PublicState {
        let score = naqp::score(self.qsos.values().cloned());
        let spots = self.fresh_spots(Utc::now());
        let multiplier_matrix = build_multiplier_matrix(&score, &spots);
        let current_band = self
            .qsos
            .values()
            .filter(|qso| !qso.deleted)
            .max_by_key(|qso| qso.timestamp)
            .and_then(|qso| qso.band);
        PublicState {
            api_version: 1,
            generated_at: Utc::now(),
            contest: naqp::contest_rules(),
            score,
            multiplier_matrix,
            spots,
            operations: self.operations.clone(),
            selected_operation: self.selected_operation.clone(),
            source: self.source.clone(),
            source_kind: self.source_kind,
            source_freshness: self.source_freshness,
            lofi_status: self.lofi_status.clone(),
            lofi_linked: self.lofi_account_call.is_some(),
            lofi_account_call: self.lofi_account_call.clone(),
            import_diagnostics: self.import_diagnostics.clone(),
            source_diagnostics: self.source_diagnostics.clone(),
            spot_status: self.spot_status.clone(),
            spots_enabled: self.spots_enabled,
            assisted_warning: self.spots_enabled.then_some(
                "Live spots/skimmers are prohibited for Single Operator. Keep them enabled only if entering Single Operator Assisted.",
            ),
            demo: self.demo,
            current_band,
        }
    }

    fn fresh_spots(&self, now: DateTime<Utc>) -> Vec<Spot> {
        let mut spots: Vec<_> = self
            .spots
            .iter()
            .filter(|spot| now - spot.time <= self.spot_policy.ttl)
            .cloned()
            .collect();
        spots.sort_by_key(|spot| std::cmp::Reverse(spot.time));
        spots
    }
}

fn build_multiplier_matrix(score: &naqp::Score, spots: &[Spot]) -> Vec<MatrixRow> {
    score
        .multiplier_rows
        .iter()
        .map(|row| MatrixRow {
            id: row.id,
            code: row.code,
            display_name: row.display_name,
            group: row.group,
            cells: Band::ALL
                .into_iter()
                .map(|band| {
                    let state = if row.worked_bands.contains(&band) {
                        MatrixCellState::Worked
                    } else if score.qsos.iter().any(|qso| {
                        qso.status == naqp::QsoStatus::Unresolved
                            && qso.band == Some(band)
                            && qso.multiplier_id == Some(row.id)
                    }) {
                        MatrixCellState::Unresolved
                    } else if spots.iter().any(|spot| {
                        !spot.stale
                            && spot.band == band
                            && spot.class == SpotClass::VerifiedMultiplier
                            && spot.predicted_multiplier.as_deref() == Some(row.id)
                    }) {
                        MatrixCellState::VerifiedSpotted
                    } else if spots.iter().any(|spot| {
                        !spot.stale
                            && spot.band == band
                            && spot.class == SpotClass::PossibleMultiplier
                            && spot.predicted_multiplier.as_deref() == Some(row.id)
                    }) {
                        MatrixCellState::PossibleSpotted
                    } else {
                        MatrixCellState::Needed
                    };
                    MatrixCell { band, state }
                })
                .collect(),
        })
        .collect()
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("qso_sidecar=info")),
        )
        .init();
    let args = Args::parse();
    validate_rbn_config(args.rbn, args.call.as_deref())?;
    let spot_policy = args.spot_policy()?;
    let lofi = lofi::LofiClient::new(args.lofi_base)?;
    let store = storage::StateStore::for_app()?;
    let restored = match store.load() {
        Ok(state) => state,
        Err(error) => {
            warn!(%error, "could not restore last-good log state");
            None
        }
    };
    let (updates, _) = broadcast::channel(32);
    let (shutdown, _) = broadcast::channel(1);
    let runtime = if args.demo {
        demo_runtime(args.rbn, spot_policy, args.demo_scenario)
    } else {
        Runtime::normal(args.rbn, spot_policy, restored)
    };
    let state = AppState {
        runtime: Arc::new(RwLock::new(runtime)),
        updates,
        shutdown: shutdown.clone(),
        lofi,
        store,
    };

    tokio::spawn(run_lofi_sync(state.clone()));
    if args.rbn {
        spawn_cluster(state.clone(), args.cluster, args.call);
    }

    let app = Router::new()
        .route("/", get(index))
        .route("/app.css", get(styles))
        .route("/responsive.css", get(responsive_styles))
        .route("/app.js", get(script))
        .route("/healthz", get(health))
        .route("/api/state", get(api_state))
        .route("/api/events", get(events))
        .route("/api/import", post(import_adif))
        .route("/api/lofi/link", post(link_lofi))
        .route("/api/operation", post(select_operation))
        .route("/api/demo", post(toggle_demo))
        .layer(DefaultBodyLimit::max(25 * 1024 * 1024))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &axum::http::Request<_>| {
                    info_span!(
                        "http.request",
                        method = %request.method(),
                        path = %http_trace_path(request.uri())
                    )
                })
                .on_response(
                    |response: &axum::http::Response<_>,
                     latency: Duration,
                     _span: &tracing::Span| {
                        info!(
                            status = response.status().as_u16(),
                            latency_ms = latency.as_millis(),
                            "HTTP response"
                        );
                    },
                ),
        )
        .with_state(state);

    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), args.port);
    let listener = tokio::net::TcpListener::bind(address).await?;
    info!(%address, "QSO Sidecar ready");
    let mut server_shutdown = shutdown.subscribe();
    tokio::spawn(async move {
        shutdown_signal().await;
        shutdown.send(()).ok();
    });
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            server_shutdown.recv().await.ok();
        })
        .await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(include_str!("static/index.html"))
}

async fn styles() -> impl IntoResponse {
    asset("text/css; charset=utf-8", include_str!("static/app.css"))
}

async fn responsive_styles() -> impl IntoResponse {
    asset(
        "text/css; charset=utf-8",
        include_str!("static/responsive.css"),
    )
}

async fn script() -> impl IntoResponse {
    asset(
        "text/javascript; charset=utf-8",
        include_str!("static/app.js"),
    )
}

fn asset(content_type: &'static str, body: &'static str) -> (HeaderMap, &'static str) {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, content_type.parse().unwrap());
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    (headers, body)
}

async fn health() -> Json<Value> {
    Json(json!({"ok": true, "version": env!("CARGO_PKG_VERSION")}))
}

async fn api_state(State(state): State<AppState>) -> Json<PublicState> {
    Json(state.runtime.read().await.public())
}

async fn events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let stream = event_stream(state.updates.subscribe(), state.shutdown.subscribe());
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn event_stream(
    updates: broadcast::Receiver<()>,
    mut shutdown: broadcast::Receiver<()>,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    let updates = BroadcastStream::new(updates).filter_map(|event| match event {
        Ok(()) => Some(Ok(Event::default().event("update").data("state"))),
        Err(_) => None,
    });
    futures_util::StreamExt::take_until(updates, async move {
        shutdown.recv().await.ok();
    })
}

async fn import_adif(State(state): State<AppState>, mut multipart: Multipart) -> Response {
    let mut imported = None;
    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() == Some("file") {
            match field.bytes().await {
                Ok(bytes) => imported = Some(bytes),
                Err(error) => return api_error(StatusCode::BAD_REQUEST, error.to_string()),
            }
        }
    }
    let Some(bytes) = imported else {
        return api_error(StatusCode::BAD_REQUEST, "missing ADIF file".into());
    };
    let mut runtime = state.runtime.write().await;
    let was_demo = runtime.demo;
    let mut next_qsos = if runtime.demo {
        BTreeMap::new()
    } else {
        runtime.qsos.clone()
    };
    match adif::import_snapshot(&bytes, &mut next_qsos) {
        Ok(diagnostics) => {
            let source = "PoLo ADIF snapshot".to_string();
            let freshness = Utc::now();
            let persisted = storage::PersistedState::new(
                next_qsos.clone(),
                None,
                Some(log_source::LogSourceKind::Adif),
                source.clone(),
                Some(freshness),
                diagnostics.clone(),
                diagnostics.record_diagnostics.clone(),
            );
            if let Err(error) = state.store.save(&persisted) {
                return api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("could not preserve imported log: {error:#}"),
                );
            }
            runtime.qsos = next_qsos;
            if was_demo {
                runtime.spots.clear();
            }
            runtime.selected_operation = None;
            runtime.import_diagnostics = diagnostics.clone();
            runtime.source_diagnostics = diagnostics.record_diagnostics.clone();
            runtime.source = source;
            runtime.source_kind = Some(log_source::LogSourceKind::Adif);
            runtime.source_freshness = Some(freshness);
            runtime.demo = false;
            drop(runtime);
            state.updates.send(()).ok();
            Json(json!({"ok": true, "diagnostics": diagnostics})).into_response()
        }
        Err(error) => api_error(StatusCode::UNPROCESSABLE_ENTITY, format!("{error:#}")),
    }
}

#[derive(Debug, Deserialize)]
struct EmailRequest {
    email: String,
}

async fn link_lofi(State(state): State<AppState>, Json(request): Json<EmailRequest>) -> Response {
    if !request.email.contains('@') || request.email.len() > 254 {
        return api_error(
            StatusCode::BAD_REQUEST,
            "enter a valid email address".into(),
        );
    }
    match state.lofi.link_email(&request.email).await {
        Ok(()) => {
            let mut runtime = state.runtime.write().await;
            runtime.lofi_status =
                "verification email sent; open it, then this page will link automatically".into();
            drop(runtime);
            state.updates.send(()).ok();
            Json(json!({"ok": true})).into_response()
        }
        Err(error) => api_error(StatusCode::BAD_GATEWAY, format!("{error:#}")),
    }
}

#[derive(Debug, Deserialize)]
struct OperationRequest {
    operation_id: String,
}

async fn select_operation(
    State(state): State<AppState>,
    Json(request): Json<OperationRequest>,
) -> Response {
    let mut runtime = state.runtime.write().await;
    if !runtime
        .operations
        .iter()
        .any(|operation| operation.id == request.operation_id)
    {
        return api_error(StatusCode::NOT_FOUND, "operation not found".into());
    }
    runtime.selected_operation = Some(request.operation_id);
    runtime.source = "Last-good data retained; synchronizing selected LoFi operation".into();
    drop(runtime);
    state.updates.send(()).ok();
    Json(json!({"ok": true})).into_response()
}

#[derive(Debug, Deserialize)]
struct DemoRequest {
    enabled: bool,
}

async fn toggle_demo(State(state): State<AppState>, Json(request): Json<DemoRequest>) -> Response {
    let spots_enabled = state.runtime.read().await.spots_enabled;
    let mut runtime = state.runtime.write().await;
    if request.enabled {
        *runtime = demo_runtime(spots_enabled, runtime.spot_policy, DemoScenario::Normal);
    } else {
        let restored = match state.store.load() {
            Ok(restored) => restored,
            Err(error) => {
                return api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("could not restore last-good log: {error:#}"),
                );
            }
        };
        *runtime = Runtime::normal(spots_enabled, runtime.spot_policy, restored);
    }
    drop(runtime);
    state.updates.send(()).ok();
    Json(json!({"ok": true})).into_response()
}

fn api_error(status: StatusCode, message: String) -> Response {
    (status, Json(json!({"ok": false, "error": message}))).into_response()
}

async fn run_lofi_sync(state: AppState) {
    let mut selected_seen: Option<String> = None;
    let mut watermark: Option<i64> = None;
    let mut registered = false;
    let mut poll_interval = LOFI_LINK_POLL_INTERVAL;
    loop {
        let result: Result<()> = async {
            let account = if registered {
                state.lofi.account().await?
            } else {
                let registration = state.lofi.register().await?;
                registered = true;
                registration
            };
            {
                let mut runtime = state.runtime.write().await;
                runtime.lofi_account_call = account.account_call.clone();
                runtime.lofi_status = if account.linked {
                    format!(
                        "linked{}",
                        account
                            .account_call
                            .as_deref()
                            .map(|call| format!(" as {call}"))
                            .unwrap_or_default()
                    )
                } else {
                    "client registered; link your PoLo account below".into()
                };
            }
            poll_interval = if account.linked {
                LOFI_SYNC_POLL_INTERVAL
            } else {
                LOFI_LINK_POLL_INTERVAL
            };
            if !account.linked {
                state.updates.send(()).ok();
                return Ok(());
            }

            let operations = state.lofi.operations(None).await?;
            let selected = {
                let mut runtime = state.runtime.write().await;
                runtime.operations = operations;
                let selected_is_valid = runtime.selected_operation.as_ref().is_some_and(|id| {
                    runtime
                        .operations
                        .iter()
                        .any(|operation| &operation.id == id)
                });
                if !selected_is_valid {
                    runtime.selected_operation = preferred_operation(&runtime.operations)
                        .map(|operation| operation.id.clone());
                }
                runtime.selected_operation.clone()
            };
            let Some(selected) = selected else {
                let mut runtime = state.runtime.write().await;
                runtime.lofi_status = "linked; no operations returned by LoFi".into();
                state.updates.send(()).ok();
                return Ok(());
            };
            let selection_changed = selected_seen.as_deref() != Some(&selected);
            let query_watermark = if selection_changed { None } else { watermark };
            let lofi::QsoBatch { qsos, diagnostics } =
                state.lofi.qsos(&selected, query_watermark).await?;
            let next_watermark = qsos
                .iter()
                .filter_map(|qso| qso.raw.get("updatedAtMillis").and_then(Value::as_i64))
                .max()
                .map(|next| query_watermark.map_or(next, |old| old.max(next)));
            let mut runtime = state.runtime.write().await;
            if !runtime.demo {
                let mut next_qsos = runtime.qsos.clone();
                let applied =
                    log_source::LogUpdate::lofi(qsos, selection_changed).apply(&mut next_qsos);
                let source = "Live Ham2K LoFi sync".to_string();
                let freshness = Utc::now();
                let persisted = runtime.persisted(
                    next_qsos.clone(),
                    applied.source,
                    source.clone(),
                    freshness,
                    diagnostics.clone(),
                );
                state.store.save(&persisted)?;
                runtime.qsos = next_qsos;
                runtime.source = source;
                runtime.source_kind = Some(applied.source);
                runtime.source_freshness = Some(freshness);
                runtime.source_diagnostics = diagnostics;
            }
            selected_seen = Some(selected);
            watermark = next_watermark.or(query_watermark);
            drop(runtime);
            state.updates.send(()).ok();
            Ok(())
        }
        .await;
        if let Err(error) = result {
            warn!(%error, "LoFi synchronization failed");
            let mut runtime = state.runtime.write().await;
            runtime.lofi_status = format!("sync error: {error:#}");
            drop(runtime);
            state.updates.send(()).ok();
        }
        tokio::time::sleep(poll_interval).await;
    }
}

fn preferred_operation(operations: &[Operation]) -> Option<&Operation> {
    let contest_start = Utc.with_ymd_and_hms(2026, 8, 1, 18, 0, 0).unwrap();
    let contest_end = Utc.with_ymd_and_hms(2026, 8, 2, 6, 0, 0).unwrap();
    operations
        .iter()
        .filter(|operation| operation.is_naqp)
        .max_by_key(|operation| {
            let current = operation.start.is_none_or(|start| start <= contest_end)
                && operation.end.is_none_or(|end| end >= contest_start);
            (current, operation.start)
        })
        .or_else(|| operations.iter().max_by_key(|operation| operation.start))
}

fn spawn_cluster(state: AppState, address: String, login_call: Option<String>) {
    let (tx, mut rx) = mpsc::channel(256);
    tokio::spawn(rbn::run(address, login_call, tx));
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let mut runtime = state.runtime.write().await;
            match event {
                rbn::ClusterEvent::Status { state, message } => {
                    update_cluster_status(&mut runtime, state, message)
                }
                rbn::ClusterEvent::Spot(raw) => merge_spot(&mut runtime, raw),
            }
            drop(runtime);
            state.updates.send(()).ok();
        }
    });
}

fn update_cluster_status(runtime: &mut Runtime, state: rbn::ConnectionState, message: String) {
    runtime.spot_status = message;
    if state.candidates_are_stale() {
        for spot in &mut runtime.spots {
            spot.stale = true;
        }
    }
}

fn merge_spot(runtime: &mut Runtime, raw: rbn::RawSpot) {
    runtime
        .spots
        .retain(|spot| raw.time - spot.time <= runtime.spot_policy.ttl);
    if let Some(existing) = runtime.spots.iter_mut().find(|spot| {
        spot.call == raw.call
            && spot.band == raw.band
            && (spot.time - raw.time).abs() <= runtime.spot_policy.dedupe_window
            && (spot.frequency_khz - raw.frequency_khz).abs() <= runtime.spot_policy.dedupe_khz
    }) {
        existing.spotters.insert(raw.spotter.clone());
        existing.best_snr_db = match (existing.best_snr_db, raw.snr_db) {
            (Some(old), Some(new)) => Some(old.max(new)),
            (old, new) => old.or(new),
        };
        if raw.time >= existing.time {
            existing.time = raw.time;
            existing.frequency_khz = raw.frequency_khz;
            existing.spotter = raw.spotter;
            existing.snr_db = raw.snr_db.or(existing.snr_db);
            existing.speed_wpm = raw.speed_wpm.or(existing.speed_wpm);
        }
        existing.reports += 1;
        existing.stale = false;
        return;
    }
    let (class, predicted_multiplier) = classify_spot(runtime, &raw.call, raw.band);
    runtime.spots.push_front(Spot {
        id: format!(
            "{}-{}-{}",
            raw.call,
            raw.band.meters(),
            raw.time.timestamp()
        ),
        call: raw.call,
        frequency_khz: raw.frequency_khz,
        band: raw.band,
        time: raw.time,
        spotter: raw.spotter.clone(),
        spotters: BTreeSet::from([raw.spotter]),
        snr_db: raw.snr_db,
        best_snr_db: raw.snr_db,
        speed_wpm: raw.speed_wpm,
        class,
        predicted_multiplier,
        reports: 1,
        stale: false,
    });
    runtime.spots.truncate(runtime.spot_policy.capacity);
}

fn classify_spot(runtime: &Runtime, call: &str, band: Band) -> (SpotClass, Option<String>) {
    let matching: Vec<_> = runtime
        .qsos
        .values()
        .filter(|qso| !qso.deleted && qso.normalized_call() == call)
        .collect();
    if matching.iter().any(|qso| qso.band == Some(band)) {
        return (SpotClass::Worked, None);
    }
    if let Some(multiplier) = matching
        .iter()
        .find_map(|qso| naqp::resolve_qso_multiplier(qso))
    {
        let already_have = runtime.qsos.values().any(|qso| {
            qso.band == Some(band)
                && naqp::resolve_qso_multiplier(qso).map(|value| value.id) == Some(multiplier.id)
        });
        return if already_have {
            (SpotClass::NeededQso, Some(multiplier.id.to_string()))
        } else {
            (
                SpotClass::VerifiedMultiplier,
                Some(multiplier.id.to_string()),
            )
        };
    }
    if matching.iter().any(|qso| qso.country.is_some()) {
        return (SpotClass::NeededQso, None);
    }
    (SpotClass::Unknown, None)
}

fn demo_runtime(spots_enabled: bool, spot_policy: SpotPolicy, scenario: DemoScenario) -> Runtime {
    let mut qsos = BTreeMap::new();
    let demo = [
        ("W1AW", Band::B20, 18, 3, "AL", "CT"),
        ("K3LR", Band::B20, 18, 7, "TIM", "PA"),
        ("VE3EJ", Band::B20, 18, 12, "JOHN", "ON"),
        ("N6RO", Band::B15, 18, 16, "KEN", "CA"),
        ("W1AW", Band::B40, 19, 1, "AL", "CT"),
        ("KP2M", Band::B20, 19, 8, "FRED", "KP2"),
        ("N4ZZ", Band::B40, 19, 14, "DON", "TN"),
        ("K3LR", Band::B40, 19, 18, "TIM", "PA"),
        ("VE3EJ", Band::B40, 20, 2, "JOHN", "ON"),
        ("DL1ABC", Band::B20, 20, 9, "HANS", "DX"),
    ];
    for (index, (call, band, hour, minute, name, location)) in demo.into_iter().enumerate() {
        let qso = Qso {
            id: format!("demo-{index}"),
            call: call.into(),
            timestamp: Utc.with_ymd_and_hms(2026, 8, 1, hour, minute, 0).unwrap(),
            band: Some(band),
            frequency_khz: None,
            mode: "CW".into(),
            name: Some(name.into()),
            location: Some(location.into()),
            country: (location == "DX").then_some("Germany".into()),
            dxcc: None,
            contest_id: Some("NAQP-CW".into()),
            deleted: false,
            raw: Value::Null,
        };
        qsos.insert(qso.id.clone(), qso);
    }
    let now = Utc::now();
    let spots = VecDeque::from([
        demo_spot(
            "N6RO",
            Band::B20,
            14_031.4,
            SpotClass::VerifiedMultiplier,
            Some("CA"),
            now,
            3,
        ),
        demo_spot(
            "W7RN",
            Band::B20,
            14_038.2,
            SpotClass::PossibleMultiplier,
            Some("NV"),
            now - chrono::Duration::seconds(18),
            2,
        ),
        demo_spot(
            "K3LR",
            Band::B20,
            14_026.8,
            SpotClass::Worked,
            Some("PA"),
            now - chrono::Duration::seconds(36),
            5,
        ),
        demo_spot(
            "ZF1A",
            Band::B15,
            21_033.0,
            SpotClass::Unknown,
            None,
            now - chrono::Duration::seconds(51),
            1,
        ),
    ]);
    let mut runtime = Runtime {
        qsos,
        spots,
        operations: vec![Operation {
            id: "demo-naqp".into(),
            title: "NAQP CW Demo Operation".into(),
            station_call: Some("N1RWJ".into()),
            subtitle: Some("Synthetic operation".into()),
            qso_count: Some(6),
            start: Some(Utc.with_ymd_and_hms(2026, 8, 1, 18, 0, 0).unwrap()),
            end: None,
            is_naqp: true,
        }],
        selected_operation: Some("demo-naqp".into()),
        source: "Synthetic demo data — no private log loaded".into(),
        source_kind: None,
        source_freshness: Some(now),
        lofi_status: "demo mode; LoFi can still be linked below".into(),
        lofi_account_call: None,
        import_diagnostics: adif::ImportDiagnostics::default(),
        source_diagnostics: Vec::new(),
        spot_status: if spots_enabled {
            "demo candidates; live connection starting".into()
        } else {
            "disabled — Single Operator safe".into()
        },
        spots_enabled,
        spot_policy,
        demo: true,
    };
    match scenario {
        DemoScenario::Normal => {}
        DemoScenario::NoLog => {
            runtime.qsos.clear();
            runtime.source = "Demo: no log loaded".into();
            runtime.source_freshness = None;
        }
        DemoScenario::StaleAdif => {
            runtime.source = "Demo: stale PoLo ADIF snapshot".into();
            runtime.source_kind = Some(log_source::LogSourceKind::Adif);
            runtime.source_freshness = Some(now - chrono::Duration::minutes(45));
        }
        DemoScenario::LofiUnavailable => {
            runtime.lofi_status = "demo failure: LoFi unavailable".into();
        }
        DemoScenario::RbnDisconnected => {
            runtime.spot_status = "demo failure: RBN disconnected".into();
            for spot in &mut runtime.spots {
                spot.stale = true;
            }
        }
        DemoScenario::MalformedImport => {
            runtime.import_diagnostics = adif::ImportDiagnostics {
                records_seen: 1,
                skipped: 1,
                warnings: vec!["Record 1: malformed demo ADIF field".into()],
                ..adif::ImportDiagnostics::default()
            };
        }
        DemoScenario::UnresolvedExchange => {
            let qso = Qso {
                id: "demo-unresolved".into(),
                call: "K1BAD".into(),
                timestamp: Utc.with_ymd_and_hms(2026, 8, 1, 20, 30, 0).unwrap(),
                band: Some(Band::B20),
                frequency_khz: Some(14_040.0),
                mode: "CW".into(),
                name: None,
                location: Some("MA".into()),
                country: Some("United States".into()),
                dxcc: Some(291),
                contest_id: Some("NAQP-CW".into()),
                deleted: false,
                raw: Value::Null,
            };
            runtime.qsos.insert(qso.id.clone(), qso);
        }
    }
    runtime
}

fn demo_spot(
    call: &str,
    band: Band,
    frequency_khz: f64,
    class: SpotClass,
    multiplier: Option<&str>,
    time: DateTime<Utc>,
    reports: u32,
) -> Spot {
    Spot {
        id: format!("demo-{call}-{band}"),
        call: call.into(),
        frequency_khz,
        band,
        time,
        spotter: "WZ7I-#".into(),
        spotters: BTreeSet::from(["WZ7I-#".into()]),
        snr_db: Some(16),
        best_snr_db: Some(16),
        speed_wpm: Some(28),
        class,
        predicted_multiplier: multiplier.map(str::to_string),
        reports,
        stale: false,
    }
}

async fn shutdown_signal() {
    let control_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            error!(%error, "failed to install Ctrl-C handler");
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => error!(%error, "failed to install terminate handler"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = control_c => {},
        () = terminate => {},
    }
    info!("shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn event_stream_closes_on_shutdown() {
        let (updates, _) = broadcast::channel(1);
        let (shutdown, _) = broadcast::channel(1);
        let stream = event_stream(updates.subscribe(), shutdown.subscribe());
        tokio::pin!(stream);

        updates.send(()).unwrap();
        let event = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .unwrap();
        assert!(event.is_some());

        shutdown.send(()).unwrap();
        let end = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .unwrap();
        assert!(end.is_none());
    }

    fn raw_spot(
        call: &str,
        frequency_khz: f64,
        time: DateTime<Utc>,
        spotter: &str,
        snr_db: i16,
    ) -> rbn::RawSpot {
        rbn::RawSpot {
            call: call.into(),
            frequency_khz,
            band: Band::B20,
            time,
            spotter: spotter.into(),
            snr_db: Some(snr_db),
            speed_wpm: Some(28),
        }
    }

    #[test]
    fn enabled_rbn_requires_a_login_callsign() {
        let error = validate_rbn_config(true, None).unwrap_err();
        assert!(error.to_string().contains("login callsign is required"));
        assert!(validate_rbn_config(true, Some("N1RWJ")).is_ok());
        assert!(validate_rbn_config(true, Some("   ")).is_err());
    }

    #[test]
    fn rbn_is_disabled_by_default_and_does_not_require_a_login_callsign() {
        let args = Args::try_parse_from(["qso-sidecar"]).unwrap();
        assert!(!args.rbn);
        assert!(validate_rbn_config(args.rbn, None).is_ok());

        let args = Args::try_parse_from(["qso-sidecar", "--rbn", "--call", "N1RWJ"]).unwrap();
        assert!(args.rbn);
    }

    #[test]
    fn http_tracing_excludes_query_values() {
        let uri: Uri = "/api/state?email=private%40example.com&token=secret"
            .parse()
            .unwrap();
        assert_eq!(http_trace_path(&uri), "/api/state");
    }

    #[test]
    fn spot_policy_rejects_unbounded_or_invalid_values() {
        let args = Args::try_parse_from(["qso-sidecar", "--spot-capacity", "0"]).unwrap();
        assert!(args.spot_policy().is_err());
        let args = Args::try_parse_from(["qso-sidecar", "--spot-dedupe-khz", "NaN"]).unwrap();
        assert!(args.spot_policy().is_err());
    }

    #[test]
    fn normal_runtime_restores_last_good_log_state() {
        let demo = demo_runtime(false, SpotPolicy::default(), DemoScenario::Normal);
        let persisted = storage::PersistedState::new(
            demo.qsos.clone(),
            Some("restored-operation".into()),
            Some(log_source::LogSourceKind::Lofi),
            "Live Ham2K LoFi sync".into(),
            demo.source_freshness,
            adif::ImportDiagnostics::default(),
            Vec::new(),
        );

        let runtime = Runtime::normal(false, SpotPolicy::default(), Some(persisted));

        assert_eq!(runtime.qsos.len(), demo.qsos.len());
        assert_eq!(
            runtime.selected_operation.as_deref(),
            Some("restored-operation")
        );
        assert_eq!(runtime.source_kind, Some(log_source::LogSourceKind::Lofi));
        assert!(runtime.source.starts_with("Restored last-good:"));
    }

    #[test]
    fn disconnect_marks_existing_candidates_stale() {
        let mut runtime = demo_runtime(true, SpotPolicy::default(), DemoScenario::Normal);

        update_cluster_status(
            &mut runtime,
            rbn::ConnectionState::Degraded,
            "disconnected".into(),
        );

        assert!(runtime.spots.iter().all(|spot| spot.stale));
    }

    #[test]
    fn refreshed_candidate_is_no_longer_stale() {
        let mut runtime = demo_runtime(true, SpotPolicy::default(), DemoScenario::Normal);
        update_cluster_status(
            &mut runtime,
            rbn::ConnectionState::Disconnected,
            "disconnected".into(),
        );
        let existing = runtime.spots.front().unwrap().clone();

        merge_spot(
            &mut runtime,
            rbn::RawSpot {
                call: existing.call.clone(),
                frequency_khz: existing.frequency_khz,
                band: existing.band,
                time: existing.time + chrono::Duration::seconds(1),
                spotter: existing.spotter.clone(),
                snr_db: existing.snr_db,
                speed_wpm: existing.speed_wpm,
            },
        );

        let refreshed = runtime
            .spots
            .iter()
            .find(|spot| spot.call == existing.call && spot.band == existing.band)
            .unwrap();
        assert!(!refreshed.stale);
        assert!(
            runtime
                .spots
                .iter()
                .filter(|spot| spot.id != refreshed.id)
                .all(|spot| spot.stale)
        );
    }

    #[test]
    fn reports_aggregate_distinct_skimmers_and_best_snr() {
        let mut runtime = Runtime::normal(false, SpotPolicy::default(), None);
        let now = Utc::now();
        merge_spot(&mut runtime, raw_spot("K1ABC", 14_025.1, now, "WZ7I-#", 20));
        merge_spot(
            &mut runtime,
            raw_spot(
                "K1ABC",
                14_025.3,
                now + chrono::Duration::seconds(20),
                "K3LR-#",
                8,
            ),
        );
        merge_spot(
            &mut runtime,
            raw_spot(
                "K1ABC",
                14_025.2,
                now + chrono::Duration::seconds(30),
                "K3LR-#",
                12,
            ),
        );

        let spot = runtime.spots.front().unwrap();
        assert_eq!(runtime.spots.len(), 1);
        assert_eq!(spot.reports, 3);
        assert_eq!(spot.spotters.len(), 2);
        assert_eq!(spot.spotter, "K3LR-#");
        assert_eq!(spot.snr_db, Some(12));
        assert_eq!(spot.best_snr_db, Some(20));
    }

    #[test]
    fn dedupe_respects_frequency_and_time_windows() {
        let mut runtime = Runtime::normal(false, SpotPolicy::default(), None);
        let now = Utc::now();
        merge_spot(&mut runtime, raw_spot("K1ABC", 14_025.0, now, "WZ7I-#", 10));
        merge_spot(
            &mut runtime,
            raw_spot(
                "K1ABC",
                14_027.0,
                now + chrono::Duration::seconds(20),
                "K3LR-#",
                11,
            ),
        );
        merge_spot(
            &mut runtime,
            raw_spot(
                "K1ABC",
                14_025.1,
                now + chrono::Duration::seconds(91),
                "W1AW-#",
                12,
            ),
        );

        assert_eq!(runtime.spots.len(), 3);
    }

    #[test]
    fn candidate_expiry_and_capacity_are_bounded() {
        let policy = SpotPolicy {
            ttl: chrono::Duration::minutes(1),
            capacity: 25,
            ..SpotPolicy::default()
        };
        let mut runtime = Runtime::normal(false, policy, None);
        let now = Utc::now();
        for index in 0..10_000 {
            merge_spot(
                &mut runtime,
                raw_spot(
                    &format!("K{index}A"),
                    14_000.0 + f64::from(index % 300),
                    now,
                    "WZ7I-#",
                    10,
                ),
            );
        }

        assert_eq!(runtime.spots.len(), 25);
        assert!(
            runtime
                .fresh_spots(now + chrono::Duration::seconds(61))
                .is_empty()
        );
    }

    #[test]
    fn classifies_same_band_worked_and_cross_band_verified_multiplier() {
        let runtime = demo_runtime(true, SpotPolicy::default(), DemoScenario::Normal);

        assert_eq!(
            classify_spot(&runtime, "W1AW", Band::B20),
            (SpotClass::Worked, None)
        );
        assert_eq!(
            classify_spot(&runtime, "N6RO", Band::B20),
            (SpotClass::VerifiedMultiplier, Some("CA".into()))
        );
    }

    #[test]
    fn multiplier_matrix_exposes_all_rows_and_non_color_states() {
        let runtime = demo_runtime(true, SpotPolicy::default(), DemoScenario::Normal);
        let public = runtime.public();

        assert_eq!(public.multiplier_matrix.len(), 111);
        let state_for = |id: &str, band: Band| {
            public
                .multiplier_matrix
                .iter()
                .find(|row| row.id == id)
                .unwrap()
                .cells
                .iter()
                .find(|cell| cell.band == band)
                .unwrap()
                .state
        };
        assert_eq!(state_for("CT", Band::B20), MatrixCellState::Worked);
        assert_eq!(state_for("CA", Band::B20), MatrixCellState::VerifiedSpotted);
        assert_eq!(state_for("NV", Band::B20), MatrixCellState::PossibleSpotted);
        assert_eq!(state_for("MA", Band::B10), MatrixCellState::Needed);

        let unresolved = demo_runtime(
            true,
            SpotPolicy::default(),
            DemoScenario::UnresolvedExchange,
        )
        .public();
        assert_eq!(
            unresolved
                .multiplier_matrix
                .iter()
                .find(|row| row.id == "MA")
                .unwrap()
                .cells
                .iter()
                .find(|cell| cell.band == Band::B20)
                .unwrap()
                .state,
            MatrixCellState::Unresolved
        );
    }

    #[test]
    fn every_demo_failure_scenario_is_reproducible() {
        let policy = SpotPolicy::default();
        let no_log = demo_runtime(true, policy, DemoScenario::NoLog);
        assert!(no_log.qsos.is_empty());
        assert!(no_log.source_freshness.is_none());

        let stale = demo_runtime(true, policy, DemoScenario::StaleAdif);
        assert_eq!(stale.source_kind, Some(log_source::LogSourceKind::Adif));

        let lofi = demo_runtime(true, policy, DemoScenario::LofiUnavailable);
        assert!(lofi.lofi_status.contains("unavailable"));

        let rbn = demo_runtime(true, policy, DemoScenario::RbnDisconnected);
        assert!(rbn.spots.iter().all(|spot| spot.stale));

        let malformed = demo_runtime(true, policy, DemoScenario::MalformedImport);
        assert_eq!(malformed.import_diagnostics.skipped, 1);

        let unresolved = demo_runtime(true, policy, DemoScenario::UnresolvedExchange);
        assert_eq!(unresolved.public().score.unresolved_exchanges, 1);
    }
}
