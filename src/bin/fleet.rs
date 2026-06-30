//! viroh-fleet: a small web app to start, stop, and monitor a fleet of
//! `viroh-sender` agents on one host.
//!
//! It spawns each agent as a child process, captures its node id and logs, and
//! exposes a JSON API plus a single-page UI. Optionally serves HTTPS using a
//! TLS certificate (e.g. Let's Encrypt) and guards the API with a bearer token.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::oneshot;

const LOG_CAP: usize = 300;
const UI_HTML: &str = include_str!("../fleet_ui.html");

#[derive(Parser, Debug)]
#[command(about = "Web fleet manager for viroh sender agents")]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0:8080")]
    bind: SocketAddr,
    /// Path to the viroh-sender binary (defaults to one next to this binary).
    #[arg(long)]
    sender_bin: Option<PathBuf>,
    /// TLS certificate chain (PEM). Enables HTTPS when set together with --tls-key.
    #[arg(long)]
    tls_cert: Option<PathBuf>,
    /// TLS private key (PEM).
    #[arg(long)]
    tls_key: Option<PathBuf>,
    /// If set, API requests must present `Authorization: Bearer <token>`.
    #[arg(long, env = "VIROH_FLEET_TOKEN")]
    token: Option<String>,
    /// Custom iroh relay URL passed to every agent (e.g. https://server.viroh.net).
    #[arg(long)]
    relay_url: Option<String>,
}

/// Per-agent runtime state, shared with the reader/monitor tasks.
struct Runtime {
    status: String, // "running" | "stopped" | "exited"
    node_id: Option<String>,
    pid: Option<u32>,
    logs: VecDeque<String>,
    kill: Option<oneshot::Sender<()>>,
}

/// One managed agent: immutable config plus shared runtime.
struct Agent {
    id: u64,
    cfg: AgentCfg,
    started_at: String,
    rt: Arc<Mutex<Runtime>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AgentCfg {
    name: String,
    #[serde(default = "default_fps")]
    fps: u32,
    #[serde(default = "default_quality")]
    quality: u8,
    #[serde(default = "default_width")]
    width: usize,
    #[serde(default = "default_height")]
    height: usize,
}
fn default_fps() -> u32 { 30 }
fn default_quality() -> u8 { 90 }
fn default_width() -> usize { 640 }
fn default_height() -> usize { 480 }

#[derive(Serialize)]
struct AgentView {
    id: u64,
    name: String,
    fps: u32,
    quality: u8,
    width: usize,
    height: usize,
    status: String,
    node_id: Option<String>,
    pid: Option<u32>,
    started_at: String,
    log_tail: Vec<String>,
}

#[derive(Clone)]
struct AppState {
    inner: Arc<Inner>,
}
struct Inner {
    agents: Mutex<HashMap<u64, Agent>>,
    next_id: AtomicU64,
    sender_bin: PathBuf,
    token: Option<String>,
    relay_url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let sender_bin = match args.sender_bin {
        Some(p) => p,
        None => default_sender_bin().context("locating viroh-sender")?,
    };
    if !sender_bin.exists() {
        anyhow::bail!("viroh-sender not found at {}", sender_bin.display());
    }

    let state = AppState {
        inner: Arc::new(Inner {
            agents: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            sender_bin,
            token: args.token,
            relay_url: args.relay_url,
        }),
    };

    let api = Router::new()
        .route("/agents", get(list_agents).post(create_agent))
        .route("/agents/{id}", axum::routing::delete(delete_agent))
        .route("/agents/{id}/start", post(start_agent))
        .route("/agents/{id}/stop", post(stop_agent))
        .route("/agents/{id}/logs", get(agent_logs))
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state.clone());

    let app = Router::new()
        .route("/", get(|| async { Html(UI_HTML) }))
        .nest("/api", api);

    match (args.tls_cert, args.tls_key) {
        (Some(cert), Some(key)) => {
            rustls::crypto::ring::default_provider()
                .install_default()
                .ok();
            let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key)
                .await
                .context("loading TLS cert/key")?;
            eprintln!("viroh-fleet listening on https://{}", args.bind);
            axum_server::bind_rustls(args.bind, config)
                .serve(app.into_make_service())
                .await?;
        }
        _ => {
            eprintln!("viroh-fleet listening on http://{}", args.bind);
            let listener = tokio::net::TcpListener::bind(args.bind).await?;
            axum::serve(listener, app).await?;
        }
    }
    Ok(())
}

fn default_sender_bin() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().context("exe has no parent dir")?;
    Ok(dir.join("viroh-sender"))
}

/// Bearer-token gate for `/api/*`. No-op when no token is configured.
async fn auth(State(state): State<AppState>, req: axum::extract::Request, next: Next) -> Response {
    if let Some(expected) = &state.inner.token {
        let ok = req
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|t| t == expected)
            .unwrap_or(false);
        if !ok {
            return (StatusCode::UNAUTHORIZED, "missing or invalid token").into_response();
        }
    }
    next.run(req).await
}

async fn list_agents(State(state): State<AppState>) -> Json<Vec<AgentView>> {
    let agents = state.inner.agents.lock().unwrap();
    let mut views: Vec<AgentView> = agents.values().map(view_of).collect();
    views.sort_by_key(|v| v.id);
    Json(views)
}

async fn create_agent(
    State(state): State<AppState>,
    Json(cfg): Json<AgentCfg>,
) -> Result<Json<AgentView>, ApiError> {
    let id = state.inner.next_id.fetch_add(1, Ordering::SeqCst);
    let rt = spawn_agent(&state, id, &cfg)?;
    let agent = Agent {
        id,
        cfg,
        started_at: chrono::Utc::now().to_rfc3339(),
        rt,
    };
    let view = view_of(&agent);
    state.inner.agents.lock().unwrap().insert(id, agent);
    Ok(Json(view))
}

async fn start_agent(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> Result<Json<AgentView>, ApiError> {
    // Read the config and current status without holding the lock across spawn.
    let cfg = {
        let agents = state.inner.agents.lock().unwrap();
        let agent = agents.get(&id).ok_or(ApiError::NotFound)?;
        if agent.rt.lock().unwrap().status == "running" {
            return Err(ApiError::Conflict("agent already running".into()));
        }
        agent.cfg.clone()
    };
    let new_rt = spawn_agent(&state, id, &cfg)?;
    let mut agents = state.inner.agents.lock().unwrap();
    let agent = agents.get_mut(&id).ok_or(ApiError::NotFound)?;
    agent.rt = new_rt;
    agent.started_at = chrono::Utc::now().to_rfc3339();
    Ok(Json(view_of(agent)))
}

async fn stop_agent(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> Result<Json<AgentView>, ApiError> {
    let agents = state.inner.agents.lock().unwrap();
    let agent = agents.get(&id).ok_or(ApiError::NotFound)?;
    {
        let mut rt = agent.rt.lock().unwrap();
        if let Some(kill) = rt.kill.take() {
            let _ = kill.send(());
        }
        rt.status = "stopped".into();
    }
    Ok(Json(view_of(agent)))
}

async fn delete_agent(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> Result<StatusCode, ApiError> {
    let agent = state.inner.agents.lock().unwrap().remove(&id);
    let agent = agent.ok_or(ApiError::NotFound)?;
    if let Some(kill) = agent.rt.lock().unwrap().kill.take() {
        let _ = kill.send(());
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn agent_logs(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> Result<Json<Vec<String>>, ApiError> {
    let agents = state.inner.agents.lock().unwrap();
    let agent = agents.get(&id).ok_or(ApiError::NotFound)?;
    let logs = agent.rt.lock().unwrap().logs.iter().cloned().collect();
    Ok(Json(logs))
}

/// Spawns a `viroh-sender` child and wires up log capture + lifecycle.
fn spawn_agent(state: &AppState, id: u64, cfg: &AgentCfg) -> Result<Arc<Mutex<Runtime>>, ApiError> {
    let mut cmd = Command::new(&state.inner.sender_bin);
    cmd.arg("--name")
        .arg(&cfg.name)
        .arg("--fps")
        .arg(cfg.fps.to_string())
        .arg("--quality")
        .arg(cfg.quality.to_string())
        .arg("--width")
        .arg(cfg.width.to_string())
        .arg("--height")
        .arg(cfg.height.to_string());
    if let Some(url) = &state.inner.relay_url {
        cmd.arg("--relay-url").arg(url);
    }
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| ApiError::Internal(format!("spawn failed: {e}")))?;

    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let rt = Arc::new(Mutex::new(Runtime {
        status: "running".into(),
        node_id: None,
        pid,
        logs: VecDeque::new(),
        kill: None,
    }));

    // Reader tasks: append lines to the ring buffer; sniff the node id.
    if let Some(out) = stdout {
        spawn_reader(rt.clone(), out, true);
    }
    if let Some(err) = stderr {
        spawn_reader(rt.clone(), err, false);
    }

    // Monitor task owns the child: kill on request, otherwise record exit.
    let (kill_tx, kill_rx) = oneshot::channel();
    rt.lock().unwrap().kill = Some(kill_tx);
    let rt_mon = rt.clone();
    tokio::spawn(async move {
        let killed = tokio::select! {
            _ = kill_rx => { let _ = child.start_kill(); true }
            _ = child.wait() => false,
        };
        let _ = child.wait().await;
        let mut rt = rt_mon.lock().unwrap();
        rt.status = if killed { "stopped".into() } else { "exited".into() };
        rt.pid = None;
        rt.kill = None;
        let _ = id; // id captured for clarity in logs/debugging
    });

    Ok(rt)
}

fn spawn_reader<R>(rt: Arc<Mutex<Runtime>>, reader: R, is_stdout: bool)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut rt = rt.lock().unwrap();
            if is_stdout {
                if let Some(rest) = line.split("node id:").nth(1) {
                    rt.node_id = Some(rest.trim().to_string());
                }
            }
            rt.logs.push_back(line);
            while rt.logs.len() > LOG_CAP {
                rt.logs.pop_front();
            }
        }
    });
}

fn view_of(agent: &Agent) -> AgentView {
    let rt = agent.rt.lock().unwrap();
    AgentView {
        id: agent.id,
        name: agent.cfg.name.clone(),
        fps: agent.cfg.fps,
        quality: agent.cfg.quality,
        width: agent.cfg.width,
        height: agent.cfg.height,
        status: rt.status.clone(),
        node_id: rt.node_id.clone(),
        pid: rt.pid,
        started_at: agent.started_at.clone(),
        log_tail: rt.logs.iter().rev().take(6).rev().cloned().collect(),
    }
}

/// API error type that renders as a JSON-ish HTTP error.
enum ApiError {
    NotFound,
    Conflict(String),
    Internal(String),
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (code, msg) = match self {
            ApiError::NotFound => (StatusCode::NOT_FOUND, "agent not found".to_string()),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, m),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (code, msg).into_response()
    }
}
