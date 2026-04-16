//! Brownshock — web edition.
//!
//! A local HTTP + WebSocket server that serves an ASCII game to the user's
//! browser. No remote deployment: everything runs on 127.0.0.1, the frontend
//! assets are embedded in the binary, and the default browser is opened
//! automatically on startup.
//!
//! Data flow per tick:
//!   browser  --keydown/keyup JSON-->  ws handler  --> State
//!   game loop --tick-->  State  --snapshot JSON-->  broadcast  -->  all ws clients
//!   browser  --render DOM grid from snapshot
//!
//! Everything UI-facing (rendering, input parsing) lives in `web/`; this file
//! is strictly the transport + lifecycle layer.

mod game;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State as AxumState,
    },
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use rust_embed::RustEmbed;
use serde::Deserialize;
use tokio::sync::{broadcast, Mutex};

use crate::game::{load_config, GameConfig, State};

// ---------------------------------------------------------------------------
// Embedded frontend
// ---------------------------------------------------------------------------

#[derive(RustEmbed)]
#[folder = "web/"]
struct WebAssets;

// ---------------------------------------------------------------------------
// Shared application state
// ---------------------------------------------------------------------------

struct AppState {
    game: Mutex<State>,
    config: GameConfig,
    /// Every connected WebSocket subscribes to this channel; the game loop
    /// publishes the per-tick snapshot JSON and each client forwards it.
    broadcaster: broadcast::Sender<String>,
}

// ---------------------------------------------------------------------------
// Client → server messages
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    /// Full current keyboard state. Browser re-sends on every keydown/keyup
    /// for whichever movement keys matter, so the server just mirrors.
    Input { left: bool, right: bool },
    /// Rising edge of W or S (dy = -1 or +1).
    Stair { dy: i32 },
    /// Q pressed: force Lose phase (placeholder until NPC detection lands).
    Lose,
    /// R pressed after win/lose (or any time): reset the world.
    Restart,
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let config = load_config();
    let state = State::new(&config);
    let (broadcaster, _) = broadcast::channel::<String>(16);

    let tick_ms = config.tick_ms;

    let app_state = Arc::new(AppState {
        game: Mutex::new(state),
        config,
        broadcaster,
    });

    // Build the HTTP/WS router.
    let app = Router::new()
        .route("/", get(serve_index))
        .route("/ws", get(ws_upgrade))
        .fallback(serve_asset)
        .with_state(app_state.clone());

    // Bind on all interfaces (0.0.0.0) so the server is reachable from the
    // host machine when running inside a Docker container with the port
    // published. Port is fixed (default 8080, overridable via $PORT) so
    // docker-compose can pre-publish it.
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);
    let bind_addr: SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {bind_addr}: {e}"));
    // URL the user's browser should hit. Always localhost — when running in
    // Docker, compose forwards host-localhost → container-0.0.0.0.
    let url = format!("http://127.0.0.1:{port}");
    println!("Brownshock listening on {url}  (bound on {bind_addr})");

    // Fire up the game loop in the background.
    tokio::spawn(game_loop(app_state.clone(), tick_ms));

    // Auto-open the default browser. Inside Docker this will fail (no GUI in
    // the container) — that's fine, we printed the URL above for the user to
    // open in their host browser.
    if let Err(e) = open::that(&url) {
        eprintln!("[startup] couldn't auto-open browser ({e})");
        eprintln!("[startup] open {url} in your browser manually");
    }

    // Stop on Ctrl+C.
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        println!("\nShutting down.");
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .expect("server error");
}

// ---------------------------------------------------------------------------
// Game loop task — single producer of Snapshot JSON.
// ---------------------------------------------------------------------------

async fn game_loop(app: Arc<AppState>, tick_ms: u64) {
    let mut interval = tokio::time::interval(Duration::from_millis(tick_ms.max(1)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        let payload = {
            let mut game = app.game.lock().await;
            game.on_player_tick();
            game.tick_world();
            match serde_json::to_string(&game.snapshot()) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[gameloop] failed to serialize snapshot: {e}");
                    continue;
                }
            }
        };

        // `send` only errors when there are no receivers — that's fine.
        let _ = app.broadcaster.send(payload);
    }
}

// ---------------------------------------------------------------------------
// HTTP routes
// ---------------------------------------------------------------------------

async fn serve_index() -> Response {
    serve_embedded("index.html")
}

async fn serve_asset(uri: Uri) -> Response {
    // `uri.path()` starts with `/`; strip it so it matches the embedded path.
    let path = uri.path().trim_start_matches('/');
    if path.is_empty() {
        return serve_embedded("index.html");
    }
    serve_embedded(path)
}

fn serve_embedded(path: &str) -> Response {
    match WebAssets::get(path) {
        Some(asset) => {
            let mime = mime_guess::from_path(path)
                .first_or_octet_stream()
                .as_ref()
                .to_string();
            ([(header::CONTENT_TYPE, mime)], asset.data.into_owned()).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    AxumState(app): AxumState<Arc<AppState>>,
) -> Response {
    ws.on_upgrade(move |socket| ws_session(socket, app))
}

async fn ws_session(mut socket: WebSocket, app: Arc<AppState>) {
    // Subscribe to snapshot broadcasts, and send the current state immediately
    // so the browser has something to render before the first tick fires.
    let mut rx = app.broadcaster.subscribe();
    let initial = {
        let game = app.game.lock().await;
        serde_json::to_string(&game.snapshot()).ok()
    };
    if let Some(s) = initial {
        if socket.send(Message::Text(s)).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            // Outbound: server → browser, one snapshot per tick.
            msg = rx.recv() => match msg {
                Ok(payload) => {
                    if socket.send(Message::Text(payload)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Client fell behind; skip. Next tick will catch them up.
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            // Inbound: browser → server, input messages.
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    handle_client_message(&app, &text).await;
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(_)) => break,
                _ => {}
            },
        }
    }
}

async fn handle_client_message(app: &AppState, text: &str) {
    let Ok(msg) = serde_json::from_str::<ClientMessage>(text) else {
        // Malformed payload — ignore rather than kill the connection.
        return;
    };
    let mut game = app.game.lock().await;
    match msg {
        ClientMessage::Input { left, right } => game.set_direction_input(left, right),
        ClientMessage::Stair { dy } => game.press_stair(dy),
        ClientMessage::Lose => game.force_lose(),
        ClientMessage::Restart => game.restart(&app.config),
    }
}
