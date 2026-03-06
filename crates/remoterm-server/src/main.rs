use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    thread,
};

use anyhow::{anyhow, Context};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::Utc;
use clap::Parser;
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use remoterm_proto::{
    ClientCapabilities, ClientFrame, CreateSessionRequest, KeyboardAction, OutputChunk,
    PatchSessionRequest, ServerFrame, SessionStatus, SessionSummary, PROTOCOL_VERSION,
};
use tokio::sync::{broadcast, RwLock as AsyncRwLock};
use tracing::{info, warn};
use uuid::Uuid;

mod storage;

use storage::{PersistedOutputEvent, PersistedSession, Storage};

#[derive(Debug, Parser)]
#[command(name = "remoterm-server")]
#[command(about = "Persistent remote terminal service scaffold")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: SocketAddr,
    #[arg(long, default_value_t = 8 * 1024 * 1024)]
    history_bytes: usize,
    #[arg(long, default_value_t = 80)]
    default_cols: u16,
    #[arg(long, default_value_t = 24)]
    default_rows: u16,
    #[arg(long, default_value = "remoterm.sqlite3")]
    db_path: PathBuf,
}

#[derive(Clone)]
struct AppState {
    sessions: Arc<AsyncRwLock<HashMap<Uuid, Arc<Session>>>>,
    storage: Arc<Storage>,
    history_bytes: usize,
    default_cols: u16,
    default_rows: u16,
}

#[derive(Debug, Clone)]
struct SessionConfig {
    name: String,
    cwd: String,
    shell: String,
    args: Vec<String>,
}

#[derive(Debug, Clone)]
struct OutputEvent {
    seq: u64,
    data: Vec<u8>,
}

#[derive(Debug)]
struct OutputHistory {
    chunks: VecDeque<OutputEvent>,
    total_bytes: usize,
    max_bytes: usize,
}

struct SessionRuntime {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

struct SpawnArtifacts {
    pid: Option<u32>,
    writer: Box<dyn Write + Send>,
    reader: Box<dyn Read + Send>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    child: Box<dyn Child + Send + Sync>,
}

struct Session {
    config: RwLock<SessionConfig>,
    summary: RwLock<SessionSummary>,
    runtime: Mutex<Option<SessionRuntime>>,
    history: Mutex<OutputHistory>,
    next_seq: AtomicU64,
    generation: AtomicU64,
    output_tx: broadcast::Sender<OutputEvent>,
    storage: Arc<Storage>,
}

impl OutputHistory {
    fn new(max_bytes: usize) -> Self {
        Self {
            chunks: VecDeque::new(),
            total_bytes: 0,
            max_bytes,
        }
    }

    fn from_persisted(max_bytes: usize, events: Vec<PersistedOutputEvent>) -> Self {
        let mut history = Self::new(max_bytes);
        for event in events {
            history.push(OutputEvent {
                seq: event.seq,
                data: event.data,
            });
        }
        history
    }

    fn clear(&mut self) {
        self.chunks.clear();
        self.total_bytes = 0;
    }

    fn push(&mut self, mut event: OutputEvent) {
        if self.max_bytes == 0 {
            return;
        }

        if event.data.len() > self.max_bytes {
            let keep_from = event.data.len() - self.max_bytes;
            event.data = event.data[keep_from..].to_vec();
        }

        self.total_bytes += event.data.len();
        self.chunks.push_back(event);

        while self.total_bytes > self.max_bytes {
            if let Some(front) = self.chunks.pop_front() {
                self.total_bytes = self.total_bytes.saturating_sub(front.data.len());
            } else {
                break;
            }
        }
    }

    fn snapshot_since(&self, resume_from_seq: Option<u64>) -> (u64, Vec<OutputEvent>) {
        let from = resume_from_seq.unwrap_or(0);
        let chunks = self
            .chunks
            .iter()
            .filter(|c| c.seq > from)
            .cloned()
            .collect::<Vec<_>>();
        let from_seq = chunks
            .first()
            .map(|c| c.seq)
            .unwrap_or(from.saturating_add(1));
        (from_seq, chunks)
    }
}

impl Session {
    fn new(
        id: Uuid,
        config: SessionConfig,
        history_bytes: usize,
        storage: Arc<Storage>,
    ) -> Arc<Self> {
        let now = Utc::now();
        let summary = SessionSummary {
            id,
            name: config.name.clone(),
            cwd: config.cwd.clone(),
            shell: config.shell.clone(),
            args: config.args.clone(),
            status: SessionStatus::Starting,
            pid: None,
            exit_code: None,
            archived: false,
            attached_clients: 0,
            last_activity_at: now,
            created_at: now,
            updated_at: now,
        };
        let (output_tx, _) = broadcast::channel(4096);
        Arc::new(Self {
            config: RwLock::new(config),
            summary: RwLock::new(summary),
            runtime: Mutex::new(None),
            history: Mutex::new(OutputHistory::new(history_bytes)),
            next_seq: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            output_tx,
            storage,
        })
    }

    fn from_persisted(
        persisted: PersistedSession,
        history_bytes: usize,
        storage: Arc<Storage>,
    ) -> anyhow::Result<Arc<Self>> {
        let (output_tx, _) = broadcast::channel(4096);
        let mut summary = persisted.summary;
        let persisted_history = storage.load_output_history(summary.id)?;
        let next_seq = persisted_history.last().map(|event| event.seq).unwrap_or(0);
        summary.attached_clients = 0;
        if matches!(
            summary.status,
            SessionStatus::Running | SessionStatus::Starting
        ) {
            summary.pid = None;
        }
        Ok(Arc::new(Self {
            config: RwLock::new(persisted.config),
            summary: RwLock::new(summary),
            runtime: Mutex::new(None),
            history: Mutex::new(OutputHistory::from_persisted(
                history_bytes,
                persisted_history,
            )),
            next_seq: AtomicU64::new(next_seq),
            generation: AtomicU64::new(0),
            output_tx,
            storage,
        }))
    }

    fn summary(&self) -> SessionSummary {
        self.summary.read().expect("summary lock poisoned").clone()
    }

    fn set_name(&self, name: String) -> anyhow::Result<SessionSummary> {
        {
            let mut cfg = self.config.write().expect("config lock poisoned");
            cfg.name = name.clone();
        }
        let mut summary = self.summary.write().expect("summary lock poisoned");
        summary.name = name;
        summary.updated_at = Utc::now();
        let updated = summary.clone();
        drop(summary);
        self.persist_snapshot()?;
        Ok(updated)
    }

    fn set_archived(&self, archived: bool) -> anyhow::Result<SessionSummary> {
        let mut summary = self.summary.write().expect("summary lock poisoned");
        let now = Utc::now();
        summary.archived = archived;
        summary.updated_at = now;
        if archived {
            summary.last_activity_at = now;
        }
        let updated = summary.clone();
        drop(summary);
        self.persist_snapshot()?;
        Ok(updated)
    }

    fn attach_client(&self) {
        let mut summary = self.summary.write().expect("summary lock poisoned");
        summary.attached_clients = summary.attached_clients.saturating_add(1);
        summary.updated_at = Utc::now();
    }

    fn detach_client(&self) {
        let mut summary = self.summary.write().expect("summary lock poisoned");
        summary.attached_clients = summary.attached_clients.saturating_sub(1);
        summary.updated_at = Utc::now();
    }

    fn note_activity(&self) {
        let mut summary = self.summary.write().expect("summary lock poisoned");
        let now = Utc::now();
        summary.updated_at = now;
        summary.last_activity_at = now;
    }

    fn subscribe_output(&self) -> broadcast::Receiver<OutputEvent> {
        self.output_tx.subscribe()
    }

    fn next_seq(&self) -> u64 {
        self.next_seq.load(Ordering::SeqCst)
    }

    fn snapshot_since(&self, resume_from_seq: Option<u64>) -> (u64, Vec<OutputEvent>) {
        self.history
            .lock()
            .expect("history lock poisoned")
            .snapshot_since(resume_from_seq)
    }

    fn write_input_blocking(&self, data: Vec<u8>) -> anyhow::Result<()> {
        {
            let mut runtime = self.runtime.lock().expect("runtime lock poisoned");
            let runtime = runtime
                .as_mut()
                .ok_or_else(|| anyhow!("session runtime is not available"))?;
            runtime
                .writer
                .write_all(&data)
                .context("write to PTY failed")?;
            runtime.writer.flush().context("flush PTY input failed")?;
        }
        self.note_activity();
        Ok(())
    }

    fn resize_blocking(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
        if cols == 0 || rows == 0 {
            return Err(anyhow!("invalid terminal size"));
        }
        {
            let runtime = self.runtime.lock().expect("runtime lock poisoned");
            let runtime = runtime
                .as_ref()
                .ok_or_else(|| anyhow!("session runtime is not available"))?;
            runtime
                .master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .context("resize PTY failed")?;
        }
        self.note_activity();
        Ok(())
    }

    fn kill_blocking(&self) -> anyhow::Result<()> {
        let killer = self.take_runtime_and_advance_generation();
        if let Some(mut killer) = killer {
            killer.kill().context("failed to kill process")?;
        }
        Ok(())
    }

    fn take_runtime_and_advance_generation(&self) -> Option<Box<dyn ChildKiller + Send + Sync>> {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.runtime
            .lock()
            .expect("runtime lock poisoned")
            .take()
            .map(|runtime| runtime.killer)
    }

    fn mark_starting(&self, generation: u64) -> anyhow::Result<()> {
        let mut summary = self.summary.write().expect("summary lock poisoned");
        if generation != self.generation.load(Ordering::SeqCst) {
            return Ok(());
        }
        let now = Utc::now();
        summary.status = SessionStatus::Starting;
        summary.pid = None;
        summary.exit_code = None;
        summary.updated_at = now;
        summary.last_activity_at = now;
        drop(summary);
        self.persist_snapshot()
    }

    fn mark_running(
        &self,
        generation: u64,
        pid: Option<u32>,
        clear_history: bool,
    ) -> anyhow::Result<()> {
        if clear_history {
            self.next_seq.store(0, Ordering::SeqCst);
            self.history.lock().expect("history lock poisoned").clear();
            self.storage.clear_output_history(self.summary().id)?;
        }

        let mut summary = self.summary.write().expect("summary lock poisoned");
        if generation != self.generation.load(Ordering::SeqCst) {
            return Ok(());
        }
        let now = Utc::now();
        summary.status = SessionStatus::Running;
        summary.pid = pid;
        summary.exit_code = None;
        summary.updated_at = now;
        summary.last_activity_at = now;
        drop(summary);
        self.persist_snapshot()
    }

    fn mark_exited(&self, generation: u64, exit_code: u32) -> anyhow::Result<()> {
        if generation != self.generation.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.runtime.lock().expect("runtime lock poisoned").take();
        let mut summary = self.summary.write().expect("summary lock poisoned");
        summary.status = SessionStatus::Exited;
        summary.pid = None;
        summary.exit_code = Some(exit_code);
        summary.updated_at = Utc::now();
        drop(summary);
        self.persist_snapshot()
    }

    fn mark_stopped(&self) -> anyhow::Result<()> {
        self.runtime.lock().expect("runtime lock poisoned").take();
        let mut summary = self.summary.write().expect("summary lock poisoned");
        let now = Utc::now();
        summary.status = SessionStatus::Stopped;
        summary.pid = None;
        summary.exit_code = None;
        summary.updated_at = now;
        summary.last_activity_at = now;
        drop(summary);
        self.persist_snapshot()
    }

    fn push_output(&self, generation: u64, data: Vec<u8>) {
        if generation != self.generation.load(Ordering::SeqCst) || data.is_empty() {
            return;
        }
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let event = OutputEvent { seq, data };
        self.history
            .lock()
            .expect("history lock poisoned")
            .push(event.clone());
        let now = Utc::now();
        {
            let mut summary = self.summary.write().expect("summary lock poisoned");
            summary.updated_at = now;
            summary.last_activity_at = now;
        }
        if let Err(err) = self.storage.append_output(
            self.summary().id,
            &PersistedOutputEvent {
                seq: event.seq,
                data: event.data.clone(),
            },
            self.history
                .lock()
                .expect("history lock poisoned")
                .max_bytes,
            now,
        ) {
            warn!(
                "failed to persist output event for session {}: {}",
                self.summary().id,
                err
            );
        }
        let _ = self.output_tx.send(event);
    }

    fn persist_snapshot(&self) -> anyhow::Result<()> {
        let config = self.config.read().expect("config lock poisoned").clone();
        let summary = self.summary.read().expect("summary lock poisoned").clone();
        self.storage.upsert_session(&summary, &config)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "remoterm_server=info".into()),
        )
        .init();

    let args = Args::parse();
    let storage = Arc::new(Storage::open(&args.db_path)?);
    let mut session_map = HashMap::new();
    let mut sessions_to_recover = Vec::new();
    for persisted in storage.load_sessions()? {
        let should_recover = !persisted.summary.archived
            && matches!(
                persisted.summary.status,
                SessionStatus::Running | SessionStatus::Starting
            );
        let id = persisted.summary.id;
        let session = Session::from_persisted(persisted, args.history_bytes, storage.clone())?;
        if should_recover {
            sessions_to_recover.push(session.clone());
        }
        session_map.insert(id, session);
    }

    let state = AppState {
        sessions: Arc::new(AsyncRwLock::new(session_map)),
        storage: storage.clone(),
        history_bytes: args.history_bytes,
        default_cols: args.default_cols,
        default_rows: args.default_rows,
    };

    recover_sessions(&sessions_to_recover, args.default_cols, args.default_rows).await;

    let app = Router::new()
        .route("/", get(index))
        .route("/healthz", get(healthz))
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route(
            "/api/sessions/{id}",
            get(get_session).patch(patch_session).delete(delete_session),
        )
        .route("/api/sessions/{id}/restart", post(restart_session))
        .route("/api/sessions/{id}/stop", post(stop_session))
        .route("/api/sessions/{id}/archive", post(archive_session))
        .route("/api/sessions/{id}/restore", post(restore_session))
        .route("/ws/{id}", get(ws_attach))
        .with_state(state);

    info!("listening on {}", args.listen);
    info!("sqlite metadata store {}", storage.path().display());
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn recover_sessions(sessions: &[Arc<Session>], cols: u16, rows: u16) {
    for session in sessions {
        let id = session.summary().id;
        if let Err(err) = spawn_session_process(session.clone(), cols, rows, false).await {
            warn!("failed to recover session {} after restart: {}", id, err);
            if let Err(mark_err) = session.mark_stopped() {
                warn!(
                    "failed to persist stopped state for unrecovered session {}: {}",
                    id, mark_err
                );
            }
        } else {
            info!("recovered session {}", id);
        }
    }
}

async fn healthz() -> &'static str {
    "ok"
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../../../static/index.html"))
}

async fn list_sessions(State(state): State<AppState>) -> Json<Vec<SessionSummary>> {
    let sessions = state.sessions.read().await;
    let mut out = sessions.values().map(|s| s.summary()).collect::<Vec<_>>();
    out.sort_by(|a, b| {
        a.archived
            .cmp(&b.archived)
            .then_with(|| a.created_at.cmp(&b.created_at))
    });
    Json(out)
}

async fn create_session(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<SessionSummary>), (StatusCode, String)> {
    validate_new_session(&req)?;

    let id = Uuid::new_v4();
    let session = Session::new(
        id,
        SessionConfig {
            name: req.name,
            cwd: req.cwd,
            shell: req.shell,
            args: req.args,
        },
        state.history_bytes,
        state.storage.clone(),
    );

    session.persist_snapshot().map_err(internal_error)?;

    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(id, session.clone());
    }

    if let Err(err) = spawn_session_process(
        session.clone(),
        state.default_cols,
        state.default_rows,
        false,
    )
    .await
    {
        let mut sessions = state.sessions.write().await;
        sessions.remove(&id);
        if let Err(delete_err) = state.storage.delete_session(id) {
            warn!(
                "failed to clean up persisted session {} after spawn failure: {}",
                id, delete_err
            );
        }
        return Err(internal_error(err));
    }

    Ok((StatusCode::CREATED, Json(session.summary())))
}

async fn get_session(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<SessionSummary>, (StatusCode, String)> {
    let sessions = state.sessions.read().await;
    let session = sessions
        .get(&id)
        .ok_or((StatusCode::NOT_FOUND, "session not found".into()))?;
    Ok(Json(session.summary()))
}

async fn patch_session(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Json(req): Json<PatchSessionRequest>,
) -> Result<Json<SessionSummary>, (StatusCode, String)> {
    if req.name.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "name must not be empty".into()));
    }
    let sessions = state.sessions.read().await;
    let session = sessions
        .get(&id)
        .ok_or((StatusCode::NOT_FOUND, "session not found".into()))?;
    Ok(Json(session.set_name(req.name).map_err(internal_error)?))
}

async fn delete_session(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<StatusCode, (StatusCode, String)> {
    let session = {
        let sessions = state.sessions.read().await;
        sessions.get(&id).cloned()
    };
    let Some(session) = session else {
        return Err((StatusCode::NOT_FOUND, "session not found".into()));
    };

    state.storage.delete_session(id).map_err(internal_error)?;

    {
        let mut sessions = state.sessions.write().await;
        sessions.remove(&id);
    }

    if let Err(err) = kill_session(session).await {
        warn!("failed to kill session {} during delete: {}", id, err);
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn restart_session(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<SessionSummary>, (StatusCode, String)> {
    let session = {
        let sessions = state.sessions.read().await;
        sessions
            .get(&id)
            .cloned()
            .ok_or((StatusCode::NOT_FOUND, "session not found".into()))?
    };

    if session.summary().archived {
        return Err((
            StatusCode::BAD_REQUEST,
            "cannot restart an archived session".into(),
        ));
    }

    if let Err(err) = kill_session(session.clone()).await {
        warn!("failed to kill session {} before restart: {}", id, err);
    }

    spawn_session_process(
        session.clone(),
        state.default_cols,
        state.default_rows,
        true,
    )
    .await
    .map_err(internal_error)?;
    Ok(Json(session.summary()))
}

async fn stop_session(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<SessionSummary>, (StatusCode, String)> {
    let session = {
        let sessions = state.sessions.read().await;
        sessions
            .get(&id)
            .cloned()
            .ok_or((StatusCode::NOT_FOUND, "session not found".into()))?
    };

    if let Err(err) = kill_session(session.clone()).await {
        warn!("failed to stop session {}: {}", id, err);
    }
    session.mark_stopped().map_err(internal_error)?;
    Ok(Json(session.summary()))
}

async fn archive_session(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<SessionSummary>, (StatusCode, String)> {
    let session = {
        let sessions = state.sessions.read().await;
        sessions
            .get(&id)
            .cloned()
            .ok_or((StatusCode::NOT_FOUND, "session not found".into()))?
    };

    if matches!(
        session.summary().status,
        SessionStatus::Running | SessionStatus::Starting
    ) {
        if let Err(err) = kill_session(session.clone()).await {
            warn!("failed to stop session {} before archive: {}", id, err);
        }
        session.mark_stopped().map_err(internal_error)?;
    }

    Ok(Json(session.set_archived(true).map_err(internal_error)?))
}

async fn restore_session(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<SessionSummary>, (StatusCode, String)> {
    let session = {
        let sessions = state.sessions.read().await;
        sessions
            .get(&id)
            .cloned()
            .ok_or((StatusCode::NOT_FOUND, "session not found".into()))?
    };

    Ok(Json(session.set_archived(false).map_err(internal_error)?))
}

async fn ws_attach(
    Path(id): Path<Uuid>,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> Response {
    let session = {
        let sessions = state.sessions.read().await;
        sessions.get(&id).cloned()
    };
    let Some(session) = session else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };

    ws.on_upgrade(move |socket| ws_session(session, socket))
}

async fn ws_session(session: Arc<Session>, socket: WebSocket) {
    session.attach_client();
    let (mut sink, mut stream) = socket.split();
    let mut output_rx = session.subscribe_output();

    if let Err(err) = handshake(&session, &mut sink, &mut stream).await {
        warn!("websocket handshake failed: {}", err);
        session.detach_client();
        return;
    }

    loop {
        tokio::select! {
            recv = stream.next() => {
                let Some(result) = recv else { break; };
                match result {
                    Ok(msg) => match handle_ws_message(msg, session.clone(), &mut sink).await {
                        Ok(continue_loop) => {
                            if !continue_loop {
                                break;
                            }
                        }
                        Err(err) => {
                            let _ = send_frame(&mut sink, &ServerFrame::Error {
                                code: "bad_request".into(),
                                message: err.to_string(),
                            }).await;
                            break;
                        }
                    },
                    Err(err) => {
                        warn!("ws receive error: {}", err);
                        break;
                    }
                }
            }
            out = output_rx.recv() => {
                match out {
                    Ok(event) => {
                        let frame = ServerFrame::Output {
                            seq: event.seq,
                            data_b64: STANDARD.encode(event.data),
                        };
                        if send_frame(&mut sink, &frame).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!("ws client lagged, skipped {} output chunks", skipped);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    session.detach_client();
}

async fn handshake(
    session: &Arc<Session>,
    sink: &mut SplitSink<WebSocket, Message>,
    stream: &mut SplitStream<WebSocket>,
) -> anyhow::Result<()> {
    loop {
        let msg = stream
            .next()
            .await
            .ok_or_else(|| anyhow!("websocket closed before hello"))??;
        match msg {
            Message::Text(text) => {
                let frame: ClientFrame =
                    serde_json::from_str(&text).context("invalid client hello frame")?;
                match frame {
                    ClientFrame::Hello {
                        cols,
                        rows,
                        resume_from_seq,
                        capabilities:
                            ClientCapabilities {
                                mobile: _,
                                keyboard_overlay: _,
                            },
                    } => {
                        if let Err(err) = resize_session(session.clone(), cols, rows).await {
                            warn!("failed to apply initial resize: {}", err);
                        }

                        let ack = ServerFrame::HelloAck {
                            protocol_version: PROTOCOL_VERSION,
                            session_id: session.summary().id,
                            next_seq: session.next_seq(),
                        };
                        send_frame(sink, &ack).await?;

                        let (from_seq, chunks) = session.snapshot_since(resume_from_seq);
                        let snapshot = ServerFrame::Snapshot {
                            from_seq,
                            chunks: chunks
                                .into_iter()
                                .map(|c| OutputChunk {
                                    seq: c.seq,
                                    data_b64: STANDARD.encode(c.data),
                                })
                                .collect(),
                        };
                        send_frame(sink, &snapshot).await?;

                        let summary = session.summary();
                        let status = ServerFrame::Status {
                            running: matches!(summary.status, SessionStatus::Running),
                            attached_clients: summary.attached_clients,
                        };
                        send_frame(sink, &status).await?;
                        return Ok(());
                    }
                    _ => return Err(anyhow!("first frame must be hello")),
                }
            }
            Message::Ping(payload) => {
                sink.send(Message::Pong(payload)).await?;
            }
            Message::Close(_) => {
                return Err(anyhow!("websocket closed before hello"));
            }
            Message::Binary(_) | Message::Pong(_) => {}
        }
    }
}

async fn handle_ws_message(
    msg: Message,
    session: Arc<Session>,
    sink: &mut SplitSink<WebSocket, Message>,
) -> anyhow::Result<bool> {
    match msg {
        Message::Text(text) => {
            let frame: ClientFrame = serde_json::from_str(&text)?;
            match frame {
                ClientFrame::Hello { .. } => return Err(anyhow!("hello can only be sent once")),
                ClientFrame::Input { data_b64 } => {
                    let bytes = STANDARD.decode(data_b64).context("invalid base64 input")?;
                    write_input(session, bytes).await?;
                }
                ClientFrame::Resize { cols, rows } => {
                    resize_session(session, cols, rows).await?;
                }
                ClientFrame::Keyboard { action } => {
                    if let Some(bytes) = keyboard_action_to_bytes(action) {
                        write_input(session, bytes).await?;
                    }
                }
                ClientFrame::Ping { ts_ms } => {
                    send_frame(sink, &ServerFrame::Pong { ts_ms }).await?;
                }
            }
        }
        Message::Ping(payload) => sink.send(Message::Pong(payload)).await?,
        Message::Close(_) => return Ok(false),
        Message::Binary(_) | Message::Pong(_) => {}
    }
    Ok(true)
}

fn keyboard_action_to_bytes(action: KeyboardAction) -> Option<Vec<u8>> {
    let bytes = match action {
        KeyboardAction::ToggleCtrlLock
        | KeyboardAction::ToggleAltLock
        | KeyboardAction::ToggleShiftLock => return None,
        KeyboardAction::SendEsc => b"\x1b".to_vec(),
        KeyboardAction::SendTab => b"\t".to_vec(),
        KeyboardAction::ArrowUp => b"\x1b[A".to_vec(),
        KeyboardAction::ArrowDown => b"\x1b[B".to_vec(),
        KeyboardAction::ArrowRight => b"\x1b[C".to_vec(),
        KeyboardAction::ArrowLeft => b"\x1b[D".to_vec(),
        KeyboardAction::Home => b"\x1b[H".to_vec(),
        KeyboardAction::End => b"\x1b[F".to_vec(),
        KeyboardAction::PageUp => b"\x1b[5~".to_vec(),
        KeyboardAction::PageDown => b"\x1b[6~".to_vec(),
        KeyboardAction::Function(n) => match n {
            1 => b"\x1bOP".to_vec(),
            2 => b"\x1bOQ".to_vec(),
            3 => b"\x1bOR".to_vec(),
            4 => b"\x1bOS".to_vec(),
            5 => b"\x1b[15~".to_vec(),
            6 => b"\x1b[17~".to_vec(),
            7 => b"\x1b[18~".to_vec(),
            8 => b"\x1b[19~".to_vec(),
            9 => b"\x1b[20~".to_vec(),
            10 => b"\x1b[21~".to_vec(),
            11 => b"\x1b[23~".to_vec(),
            12 => b"\x1b[24~".to_vec(),
            _ => return None,
        },
    };
    Some(bytes)
}

fn validate_new_session(req: &CreateSessionRequest) -> Result<(), (StatusCode, String)> {
    if req.name.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "name must not be empty".into()));
    }
    if req.cwd.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "cwd must not be empty".into()));
    }
    if req.shell.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "shell must not be empty".into()));
    }
    Ok(())
}

fn internal_error(err: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

async fn send_frame(
    sink: &mut SplitSink<WebSocket, Message>,
    frame: &ServerFrame,
) -> anyhow::Result<()> {
    sink.send(Message::Text(serde_json::to_string(frame)?.into()))
        .await
        .context("failed to send websocket frame")
}

async fn spawn_session_process(
    session: Arc<Session>,
    cols: u16,
    rows: u16,
    clear_history: bool,
) -> anyhow::Result<()> {
    let config = session.config.read().expect("config lock poisoned").clone();
    let generation = session.generation.fetch_add(1, Ordering::SeqCst) + 1;
    session.mark_starting(generation)?;

    let artifacts = tokio::task::spawn_blocking(move || spawn_blocking(config, cols, rows))
        .await
        .context("spawn task failed")??;

    {
        let mut runtime = session.runtime.lock().expect("runtime lock poisoned");
        *runtime = Some(SessionRuntime {
            writer: artifacts.writer,
            master: artifacts.master,
            killer: artifacts.killer,
        });
    }

    if let Err(err) = session.mark_running(generation, artifacts.pid, clear_history) {
        warn!(
            "failed to persist running state for session {}: {}",
            session.summary().id,
            err
        );
    }
    spawn_reader_thread(session.clone(), artifacts.reader, generation);
    spawn_wait_thread(session, artifacts.child, generation);
    Ok(())
}

fn spawn_blocking(config: SessionConfig, cols: u16, rows: u16) -> anyhow::Result<SpawnArtifacts> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new(&config.shell);
    if !config.args.is_empty() {
        cmd.args(&config.args);
    }
    cmd.cwd(&config.cwd);
    cmd.env("TERM", "xterm-256color");

    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let pid = child.process_id();
    let killer = child.clone_killer();
    let writer = pair.master.take_writer()?;
    let reader = pair.master.try_clone_reader()?;
    let master = pair.master;

    Ok(SpawnArtifacts {
        pid,
        writer,
        reader,
        master,
        killer,
        child,
    })
}

fn spawn_reader_thread(session: Arc<Session>, mut reader: Box<dyn Read + Send>, generation: u64) {
    thread::spawn(move || {
        let mut buf = vec![0_u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => session.push_output(generation, buf[..n].to_vec()),
                Err(err) => {
                    warn!(
                        "PTY read error for session {}: {}",
                        session.summary().id,
                        err
                    );
                    break;
                }
            }
        }
    });
}

fn spawn_wait_thread(
    session: Arc<Session>,
    mut child: Box<dyn Child + Send + Sync>,
    generation: u64,
) {
    thread::spawn(move || {
        let exit_code = match child.wait() {
            Ok(status) => status.exit_code(),
            Err(err) => {
                warn!(
                    "error waiting on child for session {}: {}",
                    session.summary().id,
                    err
                );
                1
            }
        };
        if let Err(err) = session.mark_exited(generation, exit_code) {
            warn!(
                "failed to persist exit state for session {}: {}",
                session.summary().id,
                err
            );
        }
    });
}

async fn write_input(session: Arc<Session>, data: Vec<u8>) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || session.write_input_blocking(data))
        .await
        .context("input task failed")?
}

async fn resize_session(session: Arc<Session>, cols: u16, rows: u16) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || session.resize_blocking(cols, rows))
        .await
        .context("resize task failed")?
}

async fn kill_session(session: Arc<Session>) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || session.kill_blocking())
        .await
        .context("kill task failed")?
}
