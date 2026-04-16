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
//! Config API (`/api/config`):
//!   GET  → current runtime config (JSON)
//!   POST → replace runtime config (JSON body, applied next tick)
//!   POST /api/config/save → persist runtime config to config.toml
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
    routing::{get, post},
    Json, Router,
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
    /// Runtime config — mutable so the live config editor can update it.
    /// Game loop reads it every tick and applies to State.
    config: Mutex<GameConfig>,
    /// Every connected WebSocket subscribes to this channel; the game loop
    /// publishes the per-tick snapshot JSON and each client forwards it.
    broadcaster: broadcast::Sender<String>,
}

// ---------------------------------------------------------------------------
// Client → server messages (WebSocket)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Input { left: bool, right: bool },
    Stair { dy: i32 },
    Lose,
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

    let app_state = Arc::new(AppState {
        game: Mutex::new(state),
        config: Mutex::new(config),
        broadcaster,
    });

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/ws", get(ws_upgrade))
        .route("/api/config", get(get_config).post(post_config))
        .route("/api/config/save", post(save_config_file))
        .fallback(serve_asset)
        .with_state(app_state.clone());

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);
    let bind_addr: SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {bind_addr}: {e}"));
    let url = format!("http://127.0.0.1:{port}");
    println!("Brownshock listening on {url}  (bound on {bind_addr})");

    tokio::spawn(game_loop(app_state.clone()));

    if let Err(e) = open::that(&url) {
        eprintln!("[startup] couldn't auto-open browser ({e})");
        eprintln!("[startup] open {url} in your browser manually");
    }

    tokio::select! {
        res = axum::serve(listener, app) => {
            if let Err(e) = res {
                eprintln!("[server] exited with error: {e}");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            println!("\nCtrl-C received — stopping.");
        }
    }
}

// ---------------------------------------------------------------------------
// Game loop — reads tick_ms from config each iteration so live changes to
// tick rate take effect immediately without restarting.
// ---------------------------------------------------------------------------

async fn game_loop(app: Arc<AppState>) {
    loop {
        // Apply config → tick → serialize → broadcast, then sleep.
        let (payload, tick_ms) = {
            let config = app.config.lock().await;
            let mut game = app.game.lock().await;
            game.apply_config(&config);
            game.on_player_tick();
            game.tick_world();
            let json = match serde_json::to_string(&game.snapshot()) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[gameloop] serialize error: {e}");
                    String::new()
                }
            };
            (json, config.tick_ms)
        };

        if !payload.is_empty() {
            let _ = app.broadcaster.send(payload);
        }

        tokio::time::sleep(Duration::from_millis(tick_ms.max(1))).await;
    }
}

// ---------------------------------------------------------------------------
// Config REST API
// ---------------------------------------------------------------------------

async fn get_config(
    AxumState(app): AxumState<Arc<AppState>>,
) -> Json<GameConfig> {
    let config = app.config.lock().await;
    Json(config.clone())
}

async fn post_config(
    AxumState(app): AxumState<Arc<AppState>>,
    Json(new_config): Json<GameConfig>,
) -> StatusCode {
    let mut config = app.config.lock().await;
    *config = new_config;
    // Applied on the very next tick via game_loop → apply_config.
    StatusCode::OK
}

async fn save_config_file(
    AxumState(app): AxumState<Arc<AppState>>,
) -> StatusCode {
    let config = app.config.lock().await;
    match config.save_to_file() {
        Ok(_) => {
            eprintln!("[config] saved config.toml");
            StatusCode::OK
        }
        Err(e) => {
            eprintln!("[config] save failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

// ---------------------------------------------------------------------------
// Static asset serving
// ---------------------------------------------------------------------------

async fn serve_index() -> Response {
    serve_embedded("index.html")
}

async fn serve_asset(uri: Uri) -> Response {
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
            msg = rx.recv() => match msg {
                Ok(payload) => {
                    if socket.send(Message::Text(payload)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
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
        return;
    };
    match msg {
        ClientMessage::Input { left, right } => {
            app.game.lock().await.set_direction_input(left, right);
        }
        ClientMessage::Stair { dy } => {
            app.game.lock().await.press_stair(dy);
        }
        ClientMessage::Lose => {
            app.game.lock().await.force_lose();
        }
        ClientMessage::Restart => {
            let config = app.config.lock().await;
            app.game.lock().await.restart(&config);
        }
    }
}
