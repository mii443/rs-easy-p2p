use anyhow::Result;
use axum::{
    extract::{Path, State},
    response::{sse::Event, Sse},
    routing::{get, post},
    Json, Router,
};
use easyp2p::p2p::SessionDescription;
use futures::stream::Stream;
use rand::Rng;
use tower_http::cors::CorsLayer;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::{broadcast, Mutex};

struct AppState {
    sessions: Mutex<HashMap<String, String>>,
    broadcasters: Mutex<HashMap<String, broadcast::Sender<String>>>,
}

#[tokio::main]
async fn main() -> Result<()> {

    let app = app();

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;

    axum::serve(listener, app).await?;

    Ok(())
}

fn app() -> Router {
    let app_state = Arc::new(AppState {
        sessions: Mutex::new(HashMap::new()),
        broadcasters: Mutex::new(HashMap::new()),
    });
    
    Router::new()
        .route("/register", post(register))
        .route("/session/:connection_code", get(get_session).post(send_session))
        .layer(CorsLayer::permissive())
        .with_state(app_state)
}

fn generate_connection_code() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHJKLMNPRTUVWXYZ234678";
    const CODE_LEN: usize = 6;
    let mut rng = rand::thread_rng();
    
    (0..CODE_LEN)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

async fn get_unique_connection_code(sessions: &Mutex<HashMap<String, String>>) -> String {
    loop {
        let code = generate_connection_code();
        let exists = sessions.lock().await.contains_key(&code);
        if !exists {
            return code;
        }
    }
}

async fn register(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SessionDescription>,
) -> Sse<impl Stream<Item = Result<Event, axum::Error>>> {
    let connection_code = get_unique_connection_code(&state.sessions).await;
    let (tx, rx) = broadcast::channel(100);

    state
        .sessions
        .lock()
        .await
        .insert(connection_code.clone(), payload.session_description);
    state
        .broadcasters
        .lock()
        .await
        .insert(connection_code.clone(), tx);

    let stream = async_stream::stream! {
        yield Ok(Event::default().event("connection_code").data(connection_code.clone()));
        let mut receiver = rx;
        if let Ok(msg) = receiver.recv().await {
            yield Ok(Event::default().event("peer_description").data(msg));
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let mut sessions = state.sessions.lock().await;
                let mut broadcasters = state.broadcasters.lock().await;
                sessions.remove(&connection_code);
                broadcasters.remove(&connection_code);
            });
        }
    };

    Sse::new(stream)
}

async fn get_session(
    Path(connection_code): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<SessionDescription> {
    let sessions = state.sessions.lock().await;
    let session_description = sessions
        .get(&connection_code)
        .cloned()
        .unwrap_or_else(|| "".to_string());
    Json(SessionDescription { session_description })
}

async fn send_session(
    Path(connection_code): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SessionDescription>,
) {
    if let Some(tx) = state.broadcasters.lock().await.get(&connection_code) {
        let _ = tx.send(payload.session_description);
    }
}
