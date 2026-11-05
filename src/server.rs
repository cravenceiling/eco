use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;
use transcribe_cpp::{Model, RunOptions, Session};

use crate::config::{Config, tilde};
use crate::network::{HomeNetwork, Verdict};

const MAX_UPLOAD_BYTES: usize = 50 * 1024 * 1024;
// The upload cap limits compressed bytes; this limits decoded PCM (64 KB/s).
const MAX_AUDIO_SECONDS: usize = 15 * 60;
// Checking the router can run subprocesses, so one answer serves a burst of connections.
const ROUTER_CHECK_TTL: Duration = Duration::from_secs(5);
const ROUTER_CHECK_TIMEOUT: Duration = Duration::from_secs(2);
const SAMPLE_RATE: usize = 16_000;

struct AppState {
    token: String,
    model_id: String,
    ffmpeg: PathBuf,
    // A session runs one transcription at a time.
    session: Mutex<Session>,
}

type Shared = Arc<AppState>;

pub fn run(config: Config) -> Result<(), String> {
    env_logger::Builder::new().filter_level(log::LevelFilter::Warn).filter_module("eco", log::LevelFilter::Info).init();
    // The library logs buffer sizes and decoder stats on every run at info/debug;
    // the filter above keeps only its warnings and errors.
    transcribe_cpp::init_logging();

    let home = config.home_network;
    let ffmpeg = config.ffmpeg()?;
    let model = Model::load(&config.model).map_err(|e| format!("Could not load {}: {e}", tilde(&config.model)))?;
    let state = Arc::new(AppState {
        token: config.token,
        model_id: config.model.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
        ffmpeg,
        session: Mutex::new(model.session().map_err(|e| e.to_string())?),
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/audio/transcriptions", post(transcribe))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES))
        .with_state(state.clone());

    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let listener = TcpListener::bind(config.listen)
            .await
            .map_err(|e| format!("Could not listen on {}: {e}", config.listen))?;
        match home {
            Some(_) => log::info!("serving {} on http://{} (home network only)", state.model_id, config.listen),
            None => log::warn!("serving {} on http://{} on every network", state.model_id, config.listen),
        }
        axum::serve(Gate { listener, home, last_router_check: None }, app).await.map_err(|e| e.to_string())
    })
}

/// Drops connections the home-network check refuses, before any HTTP is read.
struct Gate {
    listener: TcpListener,
    home: Option<HomeNetwork>,
    last_router_check: Option<(Instant, bool)>,
}

impl Gate {
    async fn is_home(&mut self, home: HomeNetwork) -> bool {
        if let Some((at, result)) = self.last_router_check
            && at.elapsed() < ROUTER_CHECK_TTL
        {
            return result;
        }
        let lookup = tokio::task::spawn_blocking(move || home.is_current());
        let result = matches!(tokio::time::timeout(ROUTER_CHECK_TIMEOUT, lookup).await, Ok(Ok(true)));
        self.last_router_check = Some((Instant::now(), result));
        result
    }
}

impl axum::serve::Listener for Gate {
    type Io = TcpStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (TcpStream, SocketAddr) {
        loop {
            let (stream, peer) = axum::serve::Listener::accept(&mut self.listener).await;
            let Some(home) = self.home else { return (stream, peer) };
            let Ok(local) = stream.local_addr() else { continue };
            let allowed = match home.judge(peer.ip(), local.ip()) {
                Verdict::Allow => true,
                Verdict::AllowIfHome => self.is_home(home).await,
                Verdict::Refuse => false,
            };
            if allowed {
                return (stream, peer);
            }
            log::info!("{peer} refused: not on the home network");
        }
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

fn api_error(status: StatusCode, message: impl Into<String>) -> Response {
    let body = json!({ "error": { "message": message.into(), "type": "invalid_request_error" } });
    (status, Json(body)).into_response()
}

fn token_matches(state: &AppState, headers: &HeaderMap) -> bool {
    let given = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or("");
    let given = given.strip_prefix("Bearer ").unwrap_or("").as_bytes();
    let expected = state.token.as_bytes();
    // Constant-time so response timing doesn't reveal how much of the key matched.
    given.len() == expected.len() && given.iter().zip(expected).fold(0, |acc, (a, b)| acc | (a ^ b)) == 0
}

fn unauthorized() -> Response {
    api_error(StatusCode::UNAUTHORIZED, "Missing or wrong API key.")
}

async fn health(State(state): State<Shared>) -> Response {
    Json(json!({ "ok": true, "model": state.model_id })).into_response()
}

async fn models(State(state): State<Shared>, headers: HeaderMap) -> Response {
    if !token_matches(&state, &headers) {
        return unauthorized();
    }
    Json(json!({ "object": "list", "data": [{ "id": state.model_id, "object": "model", "owned_by": "local" }] }))
        .into_response()
}

async fn transcribe(State(state): State<Shared>, headers: HeaderMap, mut multipart: Multipart) -> Response {
    if !token_matches(&state, &headers) {
        return unauthorized();
    }
    let mut audio = None;
    let mut response_format = String::from("json");
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(e) => return api_error(e.status(), e.body_text()),
        };
        let name = field.name().unwrap_or("").to_owned();
        let bytes = match field.bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return api_error(e.status(), e.body_text()),
        };
        match name.as_str() {
            "file" => audio = Some(bytes),
            "response_format" => response_format = String::from_utf8_lossy(&bytes).into_owned(),
            _ => {}
        }
    }
    let Some(audio) = audio else {
        return api_error(StatusCode::BAD_REQUEST, r#"Multipart field "file" is required."#);
    };

    let started = Instant::now();
    let pcm = match decode_to_pcm(&state.ffmpeg, &audio).await {
        Ok(pcm) => pcm,
        Err(e) => return api_error(StatusCode::BAD_REQUEST, format!("Could not decode audio: {e}")),
    };
    let duration = pcm.len() as f64 / SAMPLE_RATE as f64;
    let (text, language) = if pcm.is_empty() {
        (String::new(), None)
    } else {
        let run = tokio::task::spawn_blocking(move || {
            let mut session = state.session.lock().unwrap_or_else(PoisonError::into_inner);
            session.run(&pcm, &RunOptions::default()).map(|t| (t.text.trim().to_owned(), t.language))
        });
        match run.await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("Transcription failed: {e}")),
            Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("Transcription crashed: {e}")),
        }
    };
    log::info!("transcribed {duration:.1}s of audio in {:.2}s", started.elapsed().as_secs_f64());

    match response_format.as_str() {
        "text" => ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response(),
        "verbose_json" => {
            Json(json!({ "task": "transcribe", "language": language, "duration": duration, "text": text }))
                .into_response()
        }
        _ => Json(json!({ "text": text })).into_response(),
    }
}

/// Decodes any container ffmpeg understands into 16 kHz mono f32 PCM, up to
/// `MAX_AUDIO_SECONDS`.
async fn decode_to_pcm(ffmpeg: &Path, audio: &[u8]) -> Result<Vec<f32>, String> {
    // A file, not stdin: MP4/M4A as Android records it keeps its index at the
    // end, which ffmpeg can't seek back to in a pipe, and then it exits 0 with
    // no audio.
    let input = tempfile::NamedTempFile::new().map_err(|e| format!("could not create a temp file: {e}"))?;
    tokio::fs::write(input.path(), audio).await.map_err(|e| format!("could not write a temp file: {e}"))?;
    let output = Command::new(ffmpeg)
        .args(["-v", "error", "-xerror", "-i"])
        .arg(input.path())
        .args([
            "-t",
            &MAX_AUDIO_SECONDS.to_string(),
            "-f",
            "f32le",
            "-ac",
            "1",
            "-ar",
            &SAMPLE_RATE.to_string(),
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !output.status.success() || (output.stdout.is_empty() && !stderr.is_empty()) {
        return Err(if stderr.is_empty() { "ffmpeg could not decode the audio".into() } else { stderr });
    }
    if output.stdout.len() >= MAX_AUDIO_SECONDS * SAMPLE_RATE * size_of::<f32>() {
        log::warn!("audio longer than {} minutes was cut off", MAX_AUDIO_SECONDS / 60);
    }
    let (samples, _) = output.stdout.as_chunks::<4>();
    Ok(samples.iter().map(|b| f32::from_le_bytes(*b)).collect())
}
