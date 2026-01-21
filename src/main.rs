//! # databeam
//!
//! A fast, memory-efficient WebRTC file transfar application. This crate serves as the middleman,
//! allowing for multiple clients to communicate with each other via "pincodes", and establish
//! WebRTC connections for data transfers.
//!
//! Below are 2 diagrams showing detailed views of the handshake process.
//!
//! ## Architecture Overview
//!
//! ```
//! ┌─────────────────┐    HTTP GET /pin      ┌──────────────────┐
//! │                 │◄──────────────────────│                  │
//! │   Client A      │                       │                  │
//! │  (Initiator)    │─────────────────────► │                  │
//! │                 │    JSON {pin: "123456"}                 │
//! └─────────────────┘                       │                  │
//!          │                                │   Signaling      │
//!          │ WS /ws/123456                  │    Server        │
//!          ▼                                │                  │
//! ┌─────────────────┐                       │                  │
//! │   WebSocket     │◄────────────────────►│  ┌─────────────┐ │
//! │   Connection    │   Signaling Messages  │  │   Room      │ │
//! │     Pool        │                       │  │  "123456"   │ │
//! └─────────────────┘                       │  └─────────────┘ │
//!          ▲                                │                  │
//!          │ WS /ws/123456                  │                  │
//! ┌─────────────────┐                       │                  │
//! │                 │─────────────────────► │                  │
//! │   Client B      │                       │                  │
//! │  (Receiver)     │◄───────────────────── │                  │
//! │                 │    Relayed Messages   └──────────────────┘
//! └─────────────────┘
//! ```
//!
//! ## WebSocket Message Flow
//!
//! ```
//! Client A                Server                Client B
//!    │                      │                     │
//!    │──── WS Connect ─────►│                     │
//!    │     /ws/123456       │                     │
//!    │                      │◄──── WS Connect ───│
//!    │                      │      /ws/123456    │
//!    │                      │                     │
//!    │──── SDP Offer ──────►│                     │
//!    │                      │──── SDP Offer ────►│
//!    │                      │                     │
//!    │                      │◄──── SDP Answer ───│
//!    │◄──── SDP Answer ────│                     │
//!    │                      │                     │
//!    │──── ICE Candidate ──►│                     │
//!    │                      │──── ICE Candidate ─►│
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use axum::{
    Json,
    body::Body,
    extract::{
        Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{Router, get},
};
use fastrand;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio::{
    sync::{RwLock, broadcast},
    task::JoinHandle,
};
use tower_http::{cors::CorsLayer, services::ServeDir, trace::TraceLayer};
use tracing::{debug, info};

/// Total number of possible 6-digit PINs (000000 to 999999)
const MAX_PINS: usize = 1_000_000;

/// Number of u64 chunks needed to represent all PINs as bits
const BITMAP_CHUNKS: usize = MAX_PINS / 64;

/// Size of broadcast channel buffer for each room
const ROOM_BUFFER_SIZE: usize = 128;

/// High-performance bitmap for tracking PIN allocation using atomic bit operations.
///
/// Uses a lock-free design with atomic compare-and-swap for concurrent PIN allocation.
/// Each bit represents one PIN: 0 = available, 1 = allocated.
///
/// Memory layout: 15,625 × 8 bytes = 125,000 bytes (125KB)
struct PinBitmap {
    /// Array of atomic u64 chunks, each representing 64 consecutive PINs
    chunks: [AtomicU64; BITMAP_CHUNKS],
}

impl PinBitmap {
    /// Creates a new bitmap with all PINs marked as available (all bits = 0).
    const fn new() -> Self {
        const INIT: AtomicU64 = AtomicU64::new(0);
        Self {
            chunks: [INIT; BITMAP_CHUNKS],
        }
    }

    /// Atomically allocates the first available PIN using fast bit manipulation.
    ///
    /// Uses trailing_zeros() on inverted bitmask for O(1) bit finding, then
    /// compare-and-swap for lock-free allocation.
    fn allocate_pin(&self) -> Option<u32> {
        for (chunk_idx, chunk) in self.chunks.iter().enumerate() {
            let mut current = chunk.load(Ordering::Acquire);

            loop {
                if current == u64::MAX {
                    break;
                }

                let first_zero_bit = (!current).trailing_zeros() as usize;

                let pin = chunk_idx * 64 + first_zero_bit;
                if pin >= MAX_PINS {
                    break; // Exceeded valid PIN range
                }

                let new_value = current | (1u64 << first_zero_bit);
                match chunk.compare_exchange_weak(
                    current,
                    new_value,
                    Ordering::Release,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return Some(pin as u32),
                    Err(actual) => current = actual,
                }
            }
        }

        // NOTE: If we are here, that means all 1 million pins are in use.
        //       At that scale, other parts of the application (like compute)
        //       would fail before we even reach this scenario, so we can ignore this.
        None
    }

    /// Atomically releases a PIN back to the available pool.
    ///
    /// Uses atomic bit-clear operation to mark PIN as available.
    /// Safe to call multiple times on the same PIN.
    fn free_pin(&self, pin: u32) {
        let pin = pin as usize;
        if pin >= MAX_PINS {
            return; // Ignore invalid PINs
        }

        let chunk_idx = pin / 64;
        let bit_idx = pin % 64;
        let clear_mask = !(1u64 << bit_idx);

        self.chunks[chunk_idx].fetch_and(clear_mask, Ordering::Release);
    }
}

/// Central application state managing room channels and PIN allocation.
struct AppState {
    /// Maps PIN strings to their broadcast channels for message distribution
    rooms: RwLock<HashMap<String, broadcast::Sender<String>>>,

    /// Bitmap for fast PIN allocation and deallocation
    pin_bitmap: PinBitmap,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            rooms: RwLock::new(HashMap::new()),
            pin_bitmap: PinBitmap::new(),
        }
    }
}

impl AppState {
    /// Retrieves or creates a broadcast sender for the specified PIN.
    ///
    /// Uses optimistic read-first strategy: attempt fast read access first,
    /// only acquire write lock if sender doesn't exist.
    async fn get_or_create_sender(&self, pin: &str) -> broadcast::Sender<String> {
        if let Some(sender) = self.rooms.read().await.get(pin).cloned() {
            return sender;
        }

        self.rooms
            .write()
            .await
            .entry(pin.to_string())
            .or_insert_with(|| broadcast::channel(ROOM_BUFFER_SIZE).0)
            .clone()
    }

    /// Removes empty rooms and releases their PINs to prevent memory leaks.
    async fn cleanup_if_empty(&self, pin: &str) {
        let mut rooms = self.rooms.write().await;

        if let Some(sender) = rooms.get(pin) {
            if sender.receiver_count() == 0 {
                rooms.remove(pin);

                // Release PIN back to available pool
                if let Ok(pin_num) = pin.parse::<u32>() {
                    self.pin_bitmap.free_pin(pin_num);
                }

                info!("Cleaned up empty room: {}", pin);
            }
        }
    }

    /// Generates a unique PIN using the bitmap.
    async fn generate_pin(&self) -> Option<String> {
        let pin_num = self.pin_bitmap.allocate_pin()?;
        let pin = format!("{:06}", pin_num);

        // Prepare room channel to avoid races
        self.rooms
            .write()
            .await
            .entry(pin.clone())
            .or_insert_with(|| broadcast::channel(ROOM_BUFFER_SIZE).0);

        Some(pin)
    }
}

/// Handles PIN generation requests.
///
/// Generates a unique 6-digit PIN and reserves the corresponding room.
/// Returns JSON response with the allocated PIN.
///
/// # HTTP Responses
/// - `200 OK`: `{"pin": "123456"}` - Successfully allocated PIN
/// - `503 Service Unavailable`: No PINs available (virtually impossible)
async fn pin_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    #[derive(Serialize)]
    struct PinResponse {
        pin: String,
    }

    match state.generate_pin().await {
        Some(pin) => Json(PinResponse { pin }).into_response(),
        None => Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .body(Body::from("No PINs available"))
            .unwrap(),
    }
}

/// Handles WebSocket upgrade requests for PIN-scoped rooms.
///
/// Validates PIN format and upgrades HTTP connection to WebSocket
/// for real-time signaling message relay.
///
/// ## HTTP Responses
/// - `101 Switching Protocols` - Successful WebSocket upgrade
/// - `400 Bad Request` - Invalid PIN format
async fn websocket_handler(
    ws: WebSocketUpgrade,
    Path(pin): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    if !is_valid_pin(&pin) {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from("Invalid PIN: must be exactly 6 digits"))
            .unwrap();
    }

    ws.on_upgrade(move |socket| handle_websocket_connection(socket, state, pin))
}

/// Manages individual WebSocket connections within PIN-scoped rooms.
///
/// Responsibilities:
/// 1. Subscribe to room's broadcast channel
/// 2. Spawn forwarding task for incoming room messages
/// 3. Process outgoing messages from client
/// 4. Handle connection cleanup on disconnect
async fn handle_websocket_connection(socket: WebSocket, state: Arc<AppState>, pin: String) {
    let peer_id = fastrand::u64(..);
    debug!("peer {} joined room {}", peer_id, pin);

    let sender = state.get_or_create_sender(&pin).await;
    let mut receiver = sender.subscribe();
    let (mut ws_sender, mut ws_receiver) = socket.split();

    let forward_task: JoinHandle<()> = tokio::spawn(async move {
        while let Ok(message) = receiver.recv().await {
            if ws_sender.send(Message::Text(message)).await.is_err() {
                break; // Client disconnected
            }
        }
    });

    while let Some(message_result) = ws_receiver.next().await {
        match message_result {
            Ok(Message::Text(text)) => {
                // Broadcast client message to all room members
                if sender.send(text).is_err() {
                    break; // No receivers (room empty)
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(Message::Binary(_)) => continue,
            Ok(_) => continue, // Ignore ping/pong and other message types
            Err(_) => break,   // Connection error
        }
    }

    forward_task.abort();
    state.cleanup_if_empty(&pin).await;
    debug!("peer {} left room {}", peer_id, pin);
}

/// Small helper function for validating pincodes.
#[inline]
fn is_valid_pin(pin: &str) -> bool {
    pin.len() == 6 && pin.bytes().all(|b| b.is_ascii_digit())
}

/// Creates CORS middleware allowing all origins and common HTTP methods.
///
/// Enables cross-origin requests from web frontends during development
/// and production deployment.
fn create_cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_methods([Method::GET, Method::POST])
        .allow_headers(tower_http::cors::Any)
        .allow_origin(tower_http::cors::Any)
}

/// Configures structured logging with environment variable support.
///
/// Supports `RUST_LOG` environment variable for log level configuration.
/// Defaults to info-level logging for application and HTTP middleware.
fn setup_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,tower_http=info".into());

    tracing_subscriber::fmt().with_env_filter(filter).init();
}

/// Creates the main application router with all routes and middleware.
///
/// Route structure:
/// - `GET /pin` - Generate new PIN
/// - `GET /ws/:pin` - WebSocket upgrade for room
/// - `/*` - Static file serving from web/ directory
fn create_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/pin", get(pin_handler))
        .route("/ws/:pin", get(websocket_handler))
        .fallback_service(ServeDir::new("web").append_index_html_on_directories(true))
        .layer(create_cors_layer())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Main application entry point.
///
/// Initializes logging, creates application state, configures router,
/// and starts the HTTP server with WebSocket support.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    setup_logging();

    let state = Arc::new(AppState::default());
    let app = create_router(state);
    let addr: SocketAddr = "0.0.0.0:8080".parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;

    info!("listening on {}", addr);
    axum::serve(listener, app).await?;

    Ok(())
}
