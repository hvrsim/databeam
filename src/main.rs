//! Tokio-powered signaling service for WebRTC file transfer.
//! Serves static assets from `../web` and provides a pin-scoped WebSocket for signaling.

use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use axum::{
    extract::{
        Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::Method,
    response::IntoResponse,
    routing::{Router, get},
};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use tokio::{
    sync::{RwLock, broadcast},
    task::JoinHandle,
};
use tower_http::{cors::CorsLayer, services::ServeDir, trace::TraceLayer};
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    setup_tracing();

    let state = Arc::new(AppState::default());

    let app = Router::new()
        .route("/ws/:pin", get(ws_handler))
        .fallback_service(ServeDir::new("web").append_index_html_on_directories(true))
        .layer(
            CorsLayer::new()
                .allow_methods([Method::GET, Method::POST])
                .allow_headers(tower_http::cors::Any)
                .allow_origin(tower_http::cors::Any),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = "0.0.0.0:8080".parse()?;
    info!("listening on http://{}", addr);
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}

/// Shared application state that tracks WebSocket broadcast channels per pin.
#[derive(Default)]
struct AppState {
    rooms: RwLock<HashMap<String, broadcast::Sender<String>>>,
}

impl AppState {
    /// Get an existing broadcast sender for the pin or create a new one.
    async fn sender_for_pin(self: &Arc<Self>, pin: &str) -> broadcast::Sender<String> {
        {
            let map = self.rooms.read().await;
            if let Some(sender) = map.get(pin) {
                return sender.clone();
            }
        }
        let mut map = self.rooms.write().await;
        map.entry(pin.to_string())
            .or_insert_with(|| broadcast::channel(128).0)
            .clone()
    }

    /// Remove the room if there are no more subscribers.
    async fn prune_room(self: &Arc<Self>, pin: &str) {
        let mut map = self.rooms.write().await;
        if let Some(sender) = map.get(pin) {
            if sender.receiver_count() == 0 {
                map.remove(pin);
            }
        }
    }
}

/// Handle the WebSocket upgrade for a pin-protected room.
async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(pin): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !is_valid_pin(&pin) {
        return axum::response::Response::builder()
            .status(axum::http::StatusCode::BAD_REQUEST)
            .body(axum::body::Body::from("Invalid PIN"))
            .unwrap();
    }

    ws.on_upgrade(move |socket| handle_socket(socket, state, pin))
}

/// Process a single WebSocket connection, relaying messages to other peers in the same room.
async fn handle_socket(socket: WebSocket, state: Arc<AppState>, pin: String) {
    let peer_id = rand::thread_rng().r#gen::<u64>();
    info!("peer {} joined room {}", peer_id, pin);

    let sender = state.sender_for_pin(&pin).await;
    let mut room_rx = sender.subscribe();

    let (mut ws_tx, mut ws_rx) = socket.split();

    let forward_task: JoinHandle<()> = tokio::spawn(async move {
        while let Ok(msg) = room_rx.recv().await {
            if ws_tx.send(Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(msg)) = ws_rx.next().await {
        match msg {
            Message::Text(text) => {
                // Forward raw text; clients should include their own peer id for filtering.
                if sender.send(text).is_err() {
                    break;
                }
            }
            Message::Close(_) => break,
            Message::Binary(_) => {
                // Binary messages are not supported in signaling; ignore to keep surface small.
                continue;
            }
            _ => continue,
        }
    }

    forward_task.abort();
    state.prune_room(&pin).await;
    info!("peer {} left room {}", peer_id, pin);
}

/// Validate that the pin is exactly six ASCII digits.
fn is_valid_pin(pin: &str) -> bool {
    pin.len() == 6 && pin.chars().all(|c| c.is_ascii_digit())
}

/// Initialize structured logging with `RUST_LOG` support.
fn setup_tracing() {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,tower_http=info".into());
    tracing_subscriber::fmt().with_env_filter(env_filter).init();
}
