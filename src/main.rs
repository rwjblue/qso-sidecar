mod adif;
mod lofi;
mod log_source;
mod model;
mod naqp;
mod rbn;
mod storage;

use std::collections::{BTreeMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, ensure};
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, TimeZone, Utc};
use clap::Parser;
use futures_util::Stream;
use model::{Band, Operation, Qso, Spot, SpotClass};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{RwLock, broadcast, mpsc};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Serve synthetic contest data; no real log data is exposed in the dashboard.
    #[arg(long, env = "QSO_SIDECAR_DEMO")]
    demo: bool,
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
    /// Callsign sent to the DX-cluster login prompt. Required unless RBN is disabled.
    #[arg(long, env = "QSO_SIDECAR_CALL")]
    call: Option<String>,
    /// Disable the live cluster connection (and remain Single Operator eligible).
    #[arg(long, env = "QSO_SIDECAR_NO_RBN")]
    no_rbn: bool,
    /// Override the LoFi API base for development.
    #[arg(
        long,
        default_value = "https://lofi.ham2k.net",
        env = "QSO_SIDECAR_LOFI_BASE"
    )]
    lofi_base: String,
}

fn validate_rbn_config(no_rbn: bool, call: Option<&str>) -> Result<()> {
    ensure!(
        no_rbn || call.is_some_and(|call| !call.trim().is_empty()),
        "a login callsign is required when RBN is enabled; pass --call <CALL> or disable RBN with --no-rbn"
    );
    Ok(())
}

#[derive(Clone)]
struct AppState {
    runtime: Arc<RwLock<Runtime>>,
    updates: broadcast::Sender<()>,
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
    spot_status: String,
    spots_enabled: bool,
    demo: bool,
}

#[derive(Debug, Serialize)]
struct PublicState {
    api_version: u8,
    generated_at: DateTime<Utc>,
    contest: Contest,
    score: naqp::Score,
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
    spot_status: String,
    spots_enabled: bool,
    assisted_warning: Option<&'static str>,
    demo: bool,
    current_band: Option<Band>,
}

#[derive(Debug, Serialize)]
struct Contest {
    name: &'static str,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    maximum_operating_minutes: i64,
}

impl Runtime {
    fn normal(spots_enabled: bool, restored: Option<storage::PersistedState>) -> Self {
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
                spot_status,
                spots_enabled,
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
            spot_status,
            spots_enabled,
            demo: false,
        }
    }

    fn persisted(
        &self,
        qsos: BTreeMap<String, Qso>,
        source_kind: log_source::LogSourceKind,
        source: String,
        source_freshness: DateTime<Utc>,
    ) -> storage::PersistedState {
        storage::PersistedState::new(
            qsos,
            self.selected_operation.clone(),
            Some(source_kind),
            source,
            Some(source_freshness),
            self.import_diagnostics.clone(),
        )
    }

    fn public(&self) -> PublicState {
        let score = naqp::score(self.qsos.values().cloned());
        let mut spots: Vec<_> = self
            .spots
            .iter()
            .filter(|spot| Utc::now() - spot.time <= chrono::Duration::minutes(10))
            .cloned()
            .collect();
        spots.sort_by_key(|spot| std::cmp::Reverse(spot.time));
        let current_band = self
            .qsos
            .values()
            .filter(|qso| !qso.deleted)
            .max_by_key(|qso| qso.timestamp)
            .and_then(|qso| qso.band);
        PublicState {
            api_version: 1,
            generated_at: Utc::now(),
            contest: Contest {
                name: "NAQP CW — August 2026",
                starts_at: Utc.with_ymd_and_hms(2026, 8, 1, 18, 0, 0).unwrap(),
                ends_at: Utc.with_ymd_and_hms(2026, 8, 2, 6, 0, 0).unwrap(),
                maximum_operating_minutes: 600,
            },
            score,
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
            spot_status: self.spot_status.clone(),
            spots_enabled: self.spots_enabled,
            assisted_warning: self.spots_enabled.then_some(
                "Live spots/skimmers are prohibited for Single Operator. Keep them enabled only if entering Single Operator Assisted.",
            ),
            demo: self.demo,
            current_band,
        }
    }
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
    validate_rbn_config(args.no_rbn, args.call.as_deref())?;
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
    let runtime = if args.demo {
        demo_runtime(!args.no_rbn)
    } else {
        Runtime::normal(!args.no_rbn, restored)
    };
    let state = AppState {
        runtime: Arc::new(RwLock::new(runtime)),
        updates,
        lofi,
        store,
    };

    tokio::spawn(run_lofi_sync(state.clone()));
    if !args.no_rbn {
        spawn_cluster(state.clone(), args.cluster, args.call);
    }

    let app = Router::new()
        .route("/", get(index))
        .route("/app.css", get(styles))
        .route("/app.js", get(script))
        .route("/healthz", get(health))
        .route("/api/state", get(api_state))
        .route("/api/events", get(events))
        .route("/api/import", post(import_adif))
        .route("/api/lofi/link", post(link_lofi))
        .route("/api/operation", post(select_operation))
        .route("/api/demo", post(toggle_demo))
        .layer(DefaultBodyLimit::max(25 * 1024 * 1024))
        .with_state(state);

    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), args.port);
    let listener = tokio::net::TcpListener::bind(address).await?;
    info!(%address, "QSO Sidecar ready");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(include_str!("static/index.html"))
}

async fn styles() -> impl IntoResponse {
    asset("text/css; charset=utf-8", include_str!("static/app.css"))
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
    let stream = BroadcastStream::new(state.updates.subscribe()).filter_map(|event| match event {
        Ok(()) => Some(Ok(Event::default().event("update").data("state"))),
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
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
        *runtime = demo_runtime(spots_enabled);
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
        *runtime = Runtime::normal(spots_enabled, restored);
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
            let qsos = state.lofi.qsos(&selected, query_watermark).await?;
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
                let persisted =
                    runtime.persisted(next_qsos.clone(), applied.source, source.clone(), freshness);
                state.store.save(&persisted)?;
                runtime.qsos = next_qsos;
                runtime.source = source;
                runtime.source_kind = Some(applied.source);
                runtime.source_freshness = Some(freshness);
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
        tokio::time::sleep(Duration::from_secs(8)).await;
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
        .retain(|spot| raw.time - spot.time <= chrono::Duration::minutes(10));
    if let Some(existing) = runtime.spots.iter_mut().find(|spot| {
        spot.call == raw.call
            && spot.band == raw.band
            && (spot.time - raw.time).abs() <= chrono::Duration::seconds(90)
    }) {
        existing.time = raw.time;
        existing.frequency_khz = raw.frequency_khz;
        existing.spotter = raw.spotter;
        existing.snr_db = raw.snr_db.or(existing.snr_db);
        existing.speed_wpm = raw.speed_wpm.or(existing.speed_wpm);
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
        spotter: raw.spotter,
        snr_db: raw.snr_db,
        speed_wpm: raw.speed_wpm,
        class,
        predicted_multiplier,
        reports: 1,
        stale: false,
    });
    runtime.spots.truncate(200);
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
        .filter_map(|qso| qso.location.as_deref())
        .find_map(naqp::normalize_multiplier)
    {
        let already_have = runtime.qsos.values().any(|qso| {
            qso.band == Some(band)
                && qso
                    .location
                    .as_deref()
                    .and_then(naqp::normalize_multiplier)
                    .as_deref()
                    == Some(&multiplier)
        });
        return if already_have {
            (SpotClass::NeededQso, Some(multiplier))
        } else {
            (SpotClass::VerifiedMultiplier, Some(multiplier))
        };
    }
    if matching.iter().any(|qso| qso.country.is_some()) {
        return (SpotClass::NeededQso, None);
    }
    (SpotClass::Unknown, None)
}

fn demo_runtime(spots_enabled: bool) -> Runtime {
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
            Some("NV?"),
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
    Runtime {
        qsos,
        spots,
        operations: vec![Operation {
            id: "demo-naqp".into(),
            title: "NAQP CW Demo Operation".into(),
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
        spot_status: if spots_enabled {
            "demo candidates; live connection starting".into()
        } else {
            "disabled — Single Operator safe".into()
        },
        spots_enabled,
        demo: true,
    }
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
        snr_db: Some(16),
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

    #[test]
    fn rbn_requires_a_login_callsign() {
        let error = validate_rbn_config(false, None).unwrap_err();
        assert!(error.to_string().contains("login callsign is required"));
        assert!(validate_rbn_config(false, Some("N1RWJ")).is_ok());
        assert!(validate_rbn_config(false, Some("   ")).is_err());
    }

    #[test]
    fn disabled_rbn_does_not_require_a_login_callsign() {
        assert!(validate_rbn_config(true, None).is_ok());
    }

    #[test]
    fn normal_runtime_restores_last_good_log_state() {
        let demo = demo_runtime(false);
        let persisted = storage::PersistedState::new(
            demo.qsos.clone(),
            Some("restored-operation".into()),
            Some(log_source::LogSourceKind::Lofi),
            "Live Ham2K LoFi sync".into(),
            demo.source_freshness,
            adif::ImportDiagnostics::default(),
        );

        let runtime = Runtime::normal(false, Some(persisted));

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
        let mut runtime = demo_runtime(true);

        update_cluster_status(
            &mut runtime,
            rbn::ConnectionState::Degraded,
            "disconnected".into(),
        );

        assert!(runtime.spots.iter().all(|spot| spot.stale));
    }

    #[test]
    fn refreshed_candidate_is_no_longer_stale() {
        let mut runtime = demo_runtime(true);
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
}
