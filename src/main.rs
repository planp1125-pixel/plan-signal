mod db;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

// ── Shared State ────────────────────────────────────────────────────────────

/// One entry per connected WebSocket (device online)
struct WsSession {
    /// Channel to forward signaling messages to this device
    tx: mpsc::Sender<String>,
}

#[derive(Clone)]
struct AppState {
    db: db::Db,
    /// display_id → live WebSocket session
    sessions: Arc<DashMap<String, WsSession>>,
}

// ── Request / Response bodies ────────────────────────────────────────────────

#[derive(Deserialize)]
struct ClaimIdReq {
    name: String,
    os: Option<String>,
}

#[derive(Serialize)]
struct ClaimIdResp {
    device_id: String,
}

#[derive(Deserialize)]
struct OnlineReq {
    device_id: String,
    serial_ports: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct LookupResp {
    device_id: String,
    name: String,
    os: String,
    status: String,
    serial_ports: Vec<String>,
    last_seen: i64,
}

/// WebSocket message envelope (device ↔ server ↔ device)
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsMsg {
    /// Device sends every 30s to stay online
    Heartbeat {
        device_id: String,
    },
    /// Device going offline gracefully
    Goodbye {
        device_id: String,
    },
    /// Client wants to connect to a device
    Offer {
        from_id: String,
        to_id: String,
        sdp: String,
    },
    /// Host responds to an offer
    Answer {
        from_id: String,
        to_id: String,
        sdp: String,
    },
    /// ICE candidates (both directions)
    IceCandidate {
        from_id: String,
        to_id: String,
        candidate: String,
    },
    /// Reject a connection request
    Reject {
        from_id: String,
        to_id: String,
        reason: String,
    },
    // Server → client acks / errors
    Error {
        message: String,
    },
    Ok {
        message: String,
    },
}

// ── Main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let db_path = std::env::var("DATABASE_PATH").unwrap_or_else(|_| "plan_signal.db".into());
    let db = db::open(&db_path).expect("Failed to open SQLite DB");

    let state = AppState {
        db,
        sessions: Arc::new(DashMap::new()),
    };

    // Spawn stale-session cleanup (marks devices offline if no heartbeat for 90s)
    let state_for_cleanup = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let now = chrono::Utc::now().timestamp_millis();
            let cutoff = now - 90_000; // 90 seconds
            let db = &state_for_cleanup.db;
            let conn = db.lock().unwrap();
            let _ = conn.execute(
                "UPDATE devices SET status='offline' WHERE status='online' AND last_seen < ?",
                rusqlite::params![cutoff],
            );
        }
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(health))
        .route("/health", get(health))
        .route("/claim-id", post(claim_id))
        .route("/online", post(mark_online))
        .route("/offline", post(mark_offline))
        .route("/device/:id", get(lookup_device))
        .route("/ws", get(ws_handler))
        .layer(cors)
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "3000".into())
        .parse()
        .unwrap_or(3000);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("plan-signal listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

// ── Handlers ─────────────────────────────────────────────────────────────────

async fn health() -> &'static str {
    "plan-signal OK"
}

/// POST /claim-id  { name, os }
/// First-time registration — server assigns a unique display ID
async fn claim_id(State(state): State<AppState>, Json(req): Json<ClaimIdReq>) -> impl IntoResponse {
    let os = req.os.unwrap_or_else(|| "Unknown".into());
    match db::claim_id(&state.db, &req.name, &os) {
        Ok(display_id) => {
            info!("New device registered: {} ({})", display_id, req.name);
            (
                StatusCode::OK,
                Json(ClaimIdResp {
                    device_id: display_id,
                }),
            )
                .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")).into_response(),
    }
}

/// POST /online  { device_id, serial_ports: [] }
async fn mark_online(
    State(state): State<AppState>,
    Json(req): Json<OnlineReq>,
) -> impl IntoResponse {
    let ports_json = serde_json::to_string(&req.serial_ports).unwrap_or_default();
    match db::mark_online(&state.db, &req.device_id, &ports_json) {
        Ok(true) => StatusCode::OK,
        Ok(false) => StatusCode::NOT_FOUND,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// POST /offline  { device_id }
async fn mark_offline(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Some(id) = body["device_id"].as_str() {
        let _ = db::mark_offline(&state.db, id);
        state.sessions.remove(id);
    }
    StatusCode::OK
}

/// GET /device/:id
async fn lookup_device(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match db::lookup(&state.db, &id) {
        Ok(Some(row)) => {
            let ports: Vec<String> = serde_json::from_str(&row.serial_ports).unwrap_or_default();
            Json(LookupResp {
                device_id: row.display_id,
                name: row.name,
                os: row.os,
                status: row.status,
                serial_ports: ports,
                last_seen: row.last_seen,
            })
            .into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// GET /ws  — upgrade to WebSocket
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws(socket, state))
}

// ── WebSocket Session ─────────────────────────────────────────────────────────

async fn handle_ws(socket: WebSocket, state: AppState) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // mpsc channel: server pushes forwarded messages to this device via this
    let (send_tx, mut send_rx) = mpsc::channel::<String>(64);

    // Track which device_id this socket belongs to (set on first Heartbeat)
    let mut my_id: Option<String> = None;

    // Forward outgoing messages to the WebSocket
    let forward_task = tokio::spawn(async move {
        while let Some(msg) = send_rx.recv().await {
            if ws_tx.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    // Handle incoming messages
    while let Some(Ok(raw)) = ws_rx.next().await {
        let text = match raw {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            Message::Ping(p) => {
                // pong handled by axum automatically
                let _ = p;
                continue;
            }
            _ => continue,
        };

        let msg: WsMsg = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(_) => continue,
        };

        match msg {
            WsMsg::Heartbeat { device_id } => {
                // Register / refresh this session
                if my_id.is_none() {
                    my_id = Some(device_id.clone());
                    state.sessions.insert(
                        device_id.clone(),
                        WsSession {
                            tx: send_tx.clone(),
                        },
                    );
                    info!("Device connected via WS: {}", device_id);
                }
                let _ = db::mark_online(&state.db, &device_id, "[]");
            }

            WsMsg::Goodbye { device_id } => {
                let _ = db::mark_offline(&state.db, &device_id);
                state.sessions.remove(&device_id);
                break;
            }

            // Forward offer → target device
            WsMsg::Offer { ref to_id, .. } => {
                relay(&state, to_id, &text).await;
            }
            // Forward answer → original caller
            WsMsg::Answer { ref to_id, .. } => {
                relay(&state, to_id, &text).await;
            }
            // Forward ICE candidate → target
            WsMsg::IceCandidate { ref to_id, .. } => {
                relay(&state, to_id, &text).await;
            }
            // Forward reject → caller
            WsMsg::Reject { ref to_id, .. } => {
                relay(&state, to_id, &text).await;
            }

            _ => {}
        }
    }

    // Cleanup on disconnect
    if let Some(id) = my_id {
        info!("Device disconnected: {}", id);
        let _ = db::mark_offline(&state.db, &id);
        state.sessions.remove(&id);
    }

    forward_task.abort();
}

/// Relay a raw JSON string to whatever device has the given display_id connected
async fn relay(state: &AppState, to_id: &str, payload: &str) {
    if let Some(session) = state.sessions.get(to_id) {
        let _ = session.tx.send(payload.to_string()).await;
    }
}
