//! Worker connection: reconnect loop, WS session, read-only locate queue and actions.
//! Behaviour mirrors windows-agent/chip-remote-agent.ps1 (docs/PROTOCOL.md).
//!
//! - config is reloaded before every connect; incomplete → [`Status::NoConfig`], retry 30 s
//! - `Authorization: Bearer <token>` on the upgrade; the token is never logged
//! - backoff 1 → 2 → … → 60 s, reset after a session that got `hello`;
//!   close 4000 (another agent took over) → wait `takeoverBackoffSec`
//! - `hello` resyncs the locate queue (only `located_pending` chips), `chip.new` adds,
//!   `chip.withdrawn` / `action` drop; every `scanIntervalSec` the queue is matched against
//!   a read-only UIA listing → `chip.located`, or `chip.not_found` after `locateTimeoutSec`
//! - `action` → UIA (raises Claude) → `action.result`; `ping` → `pong`
//! - UIA calls (scans and actions) run off the WS loop: a slow or hung UIA call never
//!   delays `chip.not_found` (deadlines are checked on every tick, independent of the
//!   scan), pongs or other messages
//! - an `action` for a chip the session-file watcher knows ([`ChipBook`]) uses the file's
//!   exact title / tldr and passes the session title to the driver

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use chip_core::protocol::{Action, ClientMsg, ServerMsg, WireChip, CLOSE_REPLACED};
use chip_core::{select_chip, ActionError, ChipInfo, Config};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{header::AUTHORIZATION, HeaderValue};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::book::ChipBook;
use crate::driver::DriverHandle;
use crate::logging::log_view;
use crate::status::{Status, StatusSink};

/// Retry interval while config.json is missing / incomplete.
pub const NO_CONFIG_RETRY: Duration = Duration::from_secs(30);
const MAX_BACKOFF_SEC: u64 = 60;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Timings of one session (from config; tests use milliseconds).
#[derive(Debug, Clone)]
pub struct SessionParams {
    pub locate_timeout: Duration,
    pub scan_interval: Duration,
    pub raise_wait: Duration,
    /// WS Ping frame interval (keeps NATs / proxies from dropping an idle connection).
    pub keepalive: Duration,
    /// Once the server has answered a Ping, silence this long means the link is dead.
    pub dead_after: Duration,
}

impl SessionParams {
    pub fn from_config(cfg: &Config) -> SessionParams {
        SessionParams {
            locate_timeout: Duration::from_secs(cfg.locate_timeout_sec),
            scan_interval: Duration::from_secs(cfg.scan_interval_sec.max(1)),
            raise_wait: Duration::from_secs_f64(cfg.raise_wait_sec.clamp(0.0, 60.0)),
            keepalive: Duration::from_secs(30),
            dead_after: Duration::from_secs(90),
        }
    }
}

// ------------------------------------------------------------------ locate queue

#[derive(Debug, Clone)]
struct LocateItem {
    title: String,
    tldr: String,
    deadline: Instant,
}

/// Chips waiting for a read-only locate (task_id → item).
#[derive(Debug, Default)]
pub struct Locator {
    items: BTreeMap<String, LocateItem>,
}

impl Locator {
    /// Queues a chip unless it is not `located_pending` (a missing status counts as
    /// pending) or already queued.
    pub fn add(&mut self, chip: &WireChip, now: Instant, timeout: Duration) -> bool {
        if chip.task_id.is_empty()
            || (!chip.status.is_empty() && chip.status != "located_pending")
            || self.items.contains_key(&chip.task_id)
        {
            return false;
        }
        self.items.insert(
            chip.task_id.clone(),
            LocateItem {
                title: chip.title.clone(),
                tldr: chip.tldr.clone(),
                deadline: now + timeout,
            },
        );
        tracing::info!("queued {}", chip.task_id);
        true
    }

    /// `hello`: replaces the queue with the Worker's view.
    pub fn resync(&mut self, chips: &[WireChip], now: Instant, timeout: Duration) {
        self.items.clear();
        for c in chips {
            self.add(c, now, timeout);
        }
    }

    pub fn remove(&mut self, task_id: &str) -> bool {
        self.items.remove(task_id).is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn task_ids(&self) -> Vec<String> {
        self.items.keys().cloned().collect()
    }

    /// Deadline passed → `chip.not_found` (leaves the queue). Runs on every tick without
    /// waiting for UIA, so a slow scan cannot hold a report back.
    pub fn expire(&mut self, now: Instant) -> Vec<ClientMsg> {
        let mut out = Vec::new();
        self.items.retain(|id, item| {
            if now >= item.deadline {
                out.push(ClientMsg::ChipNotFound {
                    task_id: id.clone(),
                });
                false
            } else {
                true
            }
        });
        out
    }

    /// Matches the queue against the chips visible now: found → `chip.located`,
    /// deadline passed → `chip.not_found`; both leave the queue.
    pub fn resolve(&mut self, visible: &[ChipInfo], now: Instant) -> Vec<ClientMsg> {
        let mut out = Vec::new();
        self.items.retain(|id, item| {
            let tldr = (!item.tldr.is_empty()).then_some(item.tldr.as_str());
            if !visible.is_empty() && select_chip(visible, &item.title, tldr).is_some() {
                out.push(ClientMsg::ChipLocated {
                    task_id: id.clone(),
                });
                false
            } else if now >= item.deadline {
                out.push(ClientMsg::ChipNotFound {
                    task_id: id.clone(),
                });
                false
            } else {
                true
            }
        });
        out
    }
}

// ------------------------------------------------------------------ message handling

/// What to do with one text message from the Worker.
#[derive(Debug, PartialEq)]
pub enum Step {
    Nothing,
    Reply(ClientMsg),
    Act {
        request_id: String,
        task_id: String,
        title: String,
        tldr: String,
        action: Action,
    },
}

/// Pure part of message handling (queue updates + what to answer). `hello` sets
/// `*hello_received`.
pub fn on_text(
    loc: &mut Locator,
    text: &str,
    now: Instant,
    locate_timeout: Duration,
    hello_received: &mut bool,
) -> Step {
    let msg = match ServerMsg::parse(text) {
        Ok(m) => m,
        Err(e) => return on_unparsed(text, e),
    };
    match msg {
        ServerMsg::Hello { chips } => {
            loc.resync(&chips, now, locate_timeout);
            *hello_received = true;
            Step::Nothing
        }
        ServerMsg::ChipNew { chip } => {
            loc.add(&chip, now, locate_timeout);
            Step::Nothing
        }
        ServerMsg::ChipWithdrawn { task_id } => {
            if loc.remove(&task_id) {
                tracing::info!("dropped {task_id} (withdrawn)");
            }
            Step::Nothing
        }
        ServerMsg::Action {
            request_id,
            task_id,
            action,
            title,
            tldr,
        } => {
            loc.remove(&task_id);
            Step::Act {
                request_id,
                task_id,
                title,
                tldr,
                action,
            }
        }
        ServerMsg::Ping => Step::Reply(ClientMsg::Pong),
        ServerMsg::Unknown => {
            tracing::info!("ignored message type");
            Step::Nothing
        }
    }
}

/// An `action` with an unknown action name fails to parse as [`ServerMsg`]; answer it
/// with `invoke_failed` like the PowerShell agent did. Anything else is just logged.
fn on_unparsed(text: &str, err: serde_json::Error) -> Step {
    let v: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!("bad json: {err}");
            return Step::Nothing;
        }
    };
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    if s("type").as_deref() == Some("action") {
        if let Some(request_id) = s("request_id") {
            let task_id = s("task_id").unwrap_or_default();
            tracing::warn!("action {task_id}: unsupported ({err})");
            return Step::Reply(ClientMsg::ActionResult {
                request_id,
                task_id,
                ok: false,
                error: Some(ActionError::InvokeFailed(String::new()).code().to_string()),
            });
        }
    }
    tracing::warn!("unhandled message: {err}");
    Step::Nothing
}

// ------------------------------------------------------------------ session

#[derive(Debug, Default, Clone, PartialEq)]
pub struct SessionEnd {
    /// WebSocket close code from the server (None: dropped / error / no close frame).
    pub close_code: Option<u16>,
    pub hello_received: bool,
}

pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Opens the WebSocket with the bearer token. Errors never contain the token.
pub async fn connect(cfg: &Config) -> Result<WsStream, String> {
    let mut req = cfg
        .ws_url()
        .into_client_request()
        .map_err(|e| format!("bad url: {e}"))?;
    let auth = HeaderValue::from_str(&format!("Bearer {}", cfg.token))
        .map_err(|_| "token contains characters not allowed in a header".to_string())?;
    req.headers_mut().insert(AUTHORIZATION, auth);
    match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(req)).await {
        Err(_) => Err("connect timed out".into()),
        Ok(Err(WsError::Http(resp))) => Err(format!("upgrade rejected: HTTP {}", resp.status())),
        Ok(Err(e)) => Err(e.to_string()),
        Ok(Ok((ws, _))) => Ok(ws),
    }
}

// tungstenite::Error is large, but these are thin wrappers over its own API; boxing
// would only add allocations on every send.
#[allow(clippy::result_large_err)]
async fn send<S>(ws: &mut WebSocketStream<S>, msg: &ClientMsg) -> Result<(), WsError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let json = msg.to_json();
    tracing::info!("> {}", log_view(&json));
    ws.send(Message::Text(json)).await
}

/// Result of UIA work running off the WS loop.
enum Done {
    Scan(Result<Vec<ChipInfo>, ActionError>),
    Act {
        request_id: String,
        task_id: String,
        action: Action,
        result: Result<(), ActionError>,
    },
}

/// Title / tldr / session title for an action: the session file's values when the
/// watcher knows the chip (exact text), else the Worker's.
pub fn action_target(
    book: &ChipBook,
    task_id: &str,
    title: String,
    tldr: String,
) -> (String, Option<String>, Option<String>) {
    let (title, tldr, session) = match book.get(task_id) {
        Some(e) if !e.title.is_empty() => (e.title, e.tldr, Some(e.session_title)),
        Some(e) => (title, tldr, Some(e.session_title)),
        None => (title, tldr, None),
    };
    let tldr = (!tldr.is_empty()).then_some(tldr);
    let session = session.filter(|s| !s.is_empty());
    (title, tldr, session)
}

/// Logs the scan outcome only when it changes (a scan runs every tick while chips wait).
#[derive(Default)]
struct ScanLog(Option<String>);

impl ScanLog {
    fn note(&mut self, r: &Result<Vec<ChipInfo>, ActionError>, waiting: &[String]) {
        let line = match r {
            Ok(c) => format!("scan: {} chip(s) visible; waiting {:?}", c.len(), waiting),
            Err(e) => format!("scan: {e}; waiting {waiting:?}"),
        };
        if self.0.as_deref() != Some(line.as_str()) {
            match r {
                Ok(_) | Err(ActionError::ClaudeNotRunning) => tracing::info!("{line}"),
                Err(_) => tracing::warn!("{line}"),
            }
            self.0 = Some(line);
        }
    }
}

/// Runs one connected session until the socket closes or fails.
pub async fn run_session<S>(
    ws: &mut WebSocketStream<S>,
    driver: &DriverHandle,
    book: &ChipBook,
    p: &SessionParams,
) -> SessionEnd
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut end = SessionEnd::default();
    let mut loc = Locator::default();
    let mut scan = tokio::time::interval(p.scan_interval);
    scan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut keepalive = tokio::time::interval_at(Instant::now() + p.keepalive, p.keepalive);
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_rx = Instant::now();
    // Only rely on silence detection once the server is known to answer Ping frames.
    let mut server_pongs = false;
    // UIA runs on the driver thread; results come back here so the loop never waits on
    // it (it used to await list_chips / act inline, blocking not_found, pongs and reads).
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<Done>();
    let mut scanning = false;
    let mut scan_log = ScanLog::default();

    let result: Result<(), WsError> = async {
        loop {
            tokio::select! {
                incoming = ws.next() => {
                    let Some(incoming) = incoming else {
                        tracing::info!("connection ended");
                        return Ok(());
                    };
                    last_rx = Instant::now();
                    match incoming? {
                        Message::Text(text) => {
                            tracing::info!("< {}", log_view(&text));
                            match on_text(&mut loc, &text, Instant::now(), p.locate_timeout, &mut end.hello_received) {
                                Step::Nothing => {}
                                Step::Reply(m) => send(ws, &m).await?,
                                Step::Act { request_id, task_id, title, tldr, action } => {
                                    let (title, tldr, session) = action_target(book, &task_id, title, tldr);
                                    book.mark_acting(&task_id);
                                    let (d, tx, raise_wait) = (driver.clone(), done_tx.clone(), p.raise_wait);
                                    tokio::spawn(async move {
                                        let result = d.act(title, tldr, session, action, raise_wait).await;
                                        let _ = tx.send(Done::Act { request_id, task_id, action, result });
                                    });
                                }
                            }
                        }
                        Message::Close(frame) => {
                            end.close_code = frame.as_ref().map(|f| u16::from(f.code));
                            tracing::info!(
                                "server closed: {} {}",
                                end.close_code.map_or("-".to_string(), |c| c.to_string()),
                                frame.as_ref().map_or("", |f| f.reason.as_ref())
                            );
                            return Ok(());
                        }
                        Message::Pong(_) => server_pongs = true,
                        _ => {}
                    }
                }
                _ = scan.tick() => {
                    for m in loc.expire(Instant::now()) {
                        send(ws, &m).await?;
                    }
                    // Read-only: never move the window here. A chip appears while the user
                    // is at the PC, so raising Claude would steal the screen. If the window
                    // is covered the chip is simply not found and the phone gets
                    // located=false; the raise happens only for an action from the phone.
                    if !loc.is_empty() && !scanning {
                        scanning = true;
                        let (d, tx) = (driver.clone(), done_tx.clone());
                        tokio::spawn(async move {
                            let _ = tx.send(Done::Scan(d.list_chips().await));
                        });
                    }
                }
                Some(done) = done_rx.recv() => match done {
                    Done::Scan(r) => {
                        scanning = false;
                        if !loc.is_empty() {
                            scan_log.note(&r, &loc.task_ids());
                        }
                        let chips = r.unwrap_or_default();
                        for m in loc.resolve(&chips, Instant::now()) {
                            send(ws, &m).await?;
                        }
                    }
                    Done::Act { request_id, task_id, action, result } => {
                        match &result {
                            Ok(()) => tracing::info!("action {action:?} {task_id} ok"),
                            Err(e) => {
                                book.unmark_acting(&task_id);
                                tracing::warn!("action {action:?} {task_id} failed: {e}");
                            }
                        }
                        let m = ClientMsg::ActionResult {
                            request_id,
                            task_id,
                            ok: result.is_ok(),
                            error: result.err().map(|e| e.code().to_string()),
                        };
                        send(ws, &m).await?;
                    }
                },
                _ = keepalive.tick() => {
                    if server_pongs && last_rx.elapsed() > p.dead_after {
                        tracing::warn!("no traffic for {:?}; reconnecting", last_rx.elapsed());
                        return Ok(());
                    }
                    ws.send(Message::Ping(Vec::new())).await?;
                }
            }
        }
    }
    .await;

    if let Err(e) = result {
        tracing::warn!("session error: {e}");
    }
    if !loc.is_empty() {
        tracing::info!("dropping locate queue {:?}", loc.task_ids());
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), ws.close(None)).await;
    end
}

// ------------------------------------------------------------------ reconnect loop

/// State to show and how long to wait after a session. Mutates the backoff (seconds).
pub fn plan_retry(
    end: &SessionEnd,
    backoff: &mut u64,
    takeover_backoff_sec: u64,
) -> (Status, Duration) {
    if end.hello_received {
        *backoff = 1;
    }
    if end.close_code == Some(CLOSE_REPLACED) {
        (Status::Replaced, Duration::from_secs(takeover_backoff_sec))
    } else {
        let wait = (*backoff).max(1);
        *backoff = (wait * 2).min(MAX_BACKOFF_SEC);
        (Status::Disconnected, Duration::from_secs(wait))
    }
}

pub struct AgentEnv {
    pub config_path: PathBuf,
    pub driver: DriverHandle,
    pub status: StatusSink,
    /// Chips known from the session files (filled by the watcher).
    pub book: ChipBook,
}

/// The agent's main loop. Never returns.
pub async fn run_forever(env: AgentEnv) {
    let mut backoff = 1u64;
    let mut last_config_error = String::new();
    loop {
        let cfg = match Config::load(&env.config_path) {
            Ok(c) => {
                last_config_error.clear();
                c
            }
            Err(e) => {
                let msg = e.to_string();
                if msg != last_config_error {
                    tracing::error!("config: {msg}; retry every {}s", NO_CONFIG_RETRY.as_secs());
                    last_config_error = msg;
                }
                (env.status)(Status::NoConfig);
                tokio::time::sleep(NO_CONFIG_RETRY).await;
                continue;
            }
        };
        env.driver.configure(cfg.labels.clone(), cfg.prevent_sleep);
        let params = SessionParams::from_config(&cfg);
        let url = cfg.ws_url();
        tracing::info!("connecting {url}");
        let end = match connect(&cfg).await {
            Ok(mut ws) => {
                tracing::info!("connected");
                (env.status)(Status::Connected);
                run_session(&mut ws, &env.driver, &env.book, &params).await
            }
            Err(e) => {
                tracing::warn!("connect failed: {e}");
                SessionEnd::default()
            }
        };
        let (status, wait) = plan_retry(&end, &mut backoff, cfg.takeover_backoff_sec);
        (env.status)(status);
        match status {
            Status::Replaced => tracing::warn!(
                "replaced by another agent ({CLOSE_REPLACED}); retry in {}s",
                wait.as_secs()
            ),
            _ => tracing::info!(
                "disconnected (code {}); retry in {}s",
                end.close_code.map_or("-".to_string(), |c| c.to_string()),
                wait.as_secs()
            ),
        }
        tokio::time::sleep(wait).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::BookEntry;
    use crate::driver::fake::{chip, ActCall, Fake};
    use crate::driver::spawn;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;

    fn wire(id: &str, title: &str, status: &str) -> WireChip {
        WireChip {
            task_id: id.into(),
            title: title.into(),
            tldr: String::new(),
            status: status.into(),
        }
    }

    // ---------------- pure

    #[test]
    fn hello_resync_queues_only_located_pending() {
        let now = Instant::now();
        let mut loc = Locator::default();
        loc.add(&wire("old", "o", ""), now, Duration::from_secs(5));
        let mut hello = false;
        let text = json!({"type":"hello","chips":[
            {"task_id":"a","title":"A","status":"located_pending"},
            {"task_id":"b","title":"B","status":"notified"},
            {"task_id":"c","title":"C","status":"failed"},
            {"task_id":"d","title":"D"},
            {"task_id":"","title":"E","status":"located_pending"}
        ]})
        .to_string();
        let step = on_text(&mut loc, &text, now, Duration::from_secs(5), &mut hello);
        assert_eq!(step, Step::Nothing);
        assert!(hello);
        assert_eq!(loc.task_ids(), vec!["a".to_string(), "d".to_string()]);
    }

    #[test]
    fn chip_new_dedups_and_withdrawn_drops() {
        let now = Instant::now();
        let mut loc = Locator::default();
        let mut hello = false;
        let t = Duration::from_secs(5);
        let new = json!({"type":"chip.new","chip":{"task_id":"x","title":"X","status":"located_pending"}}).to_string();
        on_text(&mut loc, &new, now, t, &mut hello);
        on_text(&mut loc, &new, now, t, &mut hello);
        assert_eq!(loc.task_ids(), vec!["x".to_string()]);
        on_text(
            &mut loc,
            r#"{"type":"chip.withdrawn","task_id":"x"}"#,
            now,
            t,
            &mut hello,
        );
        assert!(loc.is_empty());
        assert!(!hello);
    }

    #[test]
    fn resolve_located_then_not_found_after_deadline() {
        let now = Instant::now();
        let t = Duration::from_secs(5);
        let mut loc = Locator::default();
        loc.add(&wire("a", "A", "located_pending"), now, t);
        loc.add(&wire("b", "B", "located_pending"), now, t);
        let visible = vec![chip("A", "")];
        assert_eq!(
            loc.resolve(&visible, now),
            vec![ClientMsg::ChipLocated {
                task_id: "a".into()
            }]
        );
        assert!(loc
            .resolve(&visible, now + Duration::from_secs(4))
            .is_empty());
        assert_eq!(
            loc.resolve(&[], now + t),
            vec![ClientMsg::ChipNotFound {
                task_id: "b".into()
            }]
        );
        assert!(loc.is_empty());
    }

    #[test]
    fn resolve_uses_tldr_and_normalized_title() {
        let now = Instant::now();
        let mut loc = Locator::default();
        let mut c = wire("a", "fix  the\nbadge", "located_pending");
        c.tldr = "two".into();
        loc.add(&c, now, Duration::from_secs(5));
        let visible = vec![chip("fix the badge", "one"), chip("fix the badge", "two")];
        assert_eq!(loc.resolve(&visible, now).len(), 1);
    }

    #[test]
    fn action_and_ping_steps() {
        let now = Instant::now();
        let t = Duration::from_secs(5);
        let mut loc = Locator::default();
        let mut hello = false;
        loc.add(&wire("a", "A", "located_pending"), now, t);
        let step = on_text(
            &mut loc,
            r#"{"type":"action","request_id":"r1","task_id":"a","action":"start","title":"A","tldr":"d"}"#,
            now,
            t,
            &mut hello,
        );
        assert_eq!(
            step,
            Step::Act {
                request_id: "r1".into(),
                task_id: "a".into(),
                title: "A".into(),
                tldr: "d".into(),
                action: Action::Start
            }
        );
        assert!(
            loc.is_empty(),
            "action drops the chip from the locate queue"
        );
        assert_eq!(
            on_text(&mut loc, r#"{"type":"ping"}"#, now, t, &mut hello),
            Step::Reply(ClientMsg::Pong)
        );
        assert_eq!(
            on_text(
                &mut loc,
                r#"{"type":"action","request_id":"r2","task_id":"a","action":"explode"}"#,
                now,
                t,
                &mut hello
            ),
            Step::Reply(ClientMsg::ActionResult {
                request_id: "r2".into(),
                task_id: "a".into(),
                ok: false,
                error: Some("invoke_failed".into())
            })
        );
        assert_eq!(
            on_text(&mut loc, "not json", now, t, &mut hello),
            Step::Nothing
        );
        assert_eq!(
            on_text(&mut loc, r#"{"type":"future.x"}"#, now, t, &mut hello),
            Step::Nothing
        );
    }

    #[test]
    fn backoff_and_takeover() {
        let mut b = 1;
        let fail = SessionEnd::default();
        let waits: Vec<u64> = (0..8)
            .map(|_| plan_retry(&fail, &mut b, 300).1.as_secs())
            .collect();
        assert_eq!(waits, vec![1, 2, 4, 8, 16, 32, 60, 60]);
        // A session that got hello resets the backoff.
        let ok = SessionEnd {
            close_code: Some(1006),
            hello_received: true,
        };
        assert_eq!(
            plan_retry(&ok, &mut b, 300),
            (Status::Disconnected, Duration::from_secs(1))
        );
        assert_eq!(b, 2);
        // 4000: another agent took over.
        let replaced = SessionEnd {
            close_code: Some(4000),
            hello_received: true,
        };
        assert_eq!(
            plan_retry(&replaced, &mut b, 300),
            (Status::Replaced, Duration::from_secs(300))
        );
        assert_eq!(b, 1);
    }

    #[test]
    fn params_from_config() {
        let cfg = Config {
            scan_interval_sec: 0,
            raise_wait_sec: 2.5,
            ..Config::default()
        };
        let p = SessionParams::from_config(&cfg);
        assert_eq!(p.scan_interval, Duration::from_secs(1));
        assert_eq!(p.locate_timeout, Duration::from_secs(5));
        assert_eq!(p.raise_wait, Duration::from_millis(2500));
    }

    // ---------------- in-process WS server

    type ServerWs = WebSocketStream<TcpStream>;

    struct Server {
        cfg: Config,
        accepted: tokio::sync::oneshot::Receiver<(ServerWs, Option<String>)>,
    }

    // The handshake callback's signature (large ErrorResponse) is fixed by tungstenite.
    #[allow(clippy::result_large_err)]
    async fn server() -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, accepted) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let auth = Arc::new(Mutex::new(None));
            let a2 = auth.clone();
            let cb = move |req: &Request, resp: Response| {
                assert_eq!(req.uri().path(), "/v1/agent/ws");
                *a2.lock().unwrap() = req
                    .headers()
                    .get("authorization")
                    .map(|v| v.to_str().unwrap().to_string());
                Ok(resp)
            };
            let ws = tokio_tungstenite::accept_hdr_async(stream, cb)
                .await
                .unwrap();
            let a = auth.lock().unwrap().clone();
            let _ = tx.send((ws, a));
        });
        let cfg = Config {
            url: format!("http://127.0.0.1:{port}"),
            token: "secret-token".into(),
            ..Config::default()
        };
        Server { cfg, accepted }
    }

    fn fast() -> SessionParams {
        SessionParams {
            locate_timeout: Duration::from_millis(300),
            scan_interval: Duration::from_millis(20),
            raise_wait: Duration::from_millis(7),
            keepalive: Duration::from_secs(30),
            dead_after: Duration::from_secs(90),
        }
    }

    /// Connects an agent session (fake UIA) to a fresh in-process server.
    async fn start(
        fake: &Fake,
        p: SessionParams,
    ) -> (ServerWs, tokio::task::JoinHandle<SessionEnd>) {
        start_with_book(fake, p, ChipBook::default()).await
    }

    async fn start_with_book(
        fake: &Fake,
        p: SessionParams,
        book: ChipBook,
    ) -> (ServerWs, tokio::task::JoinHandle<SessionEnd>) {
        let srv = server().await;
        let driver = spawn(fake.clone());
        let mut client = connect(&srv.cfg).await.unwrap();
        let (ws, auth) = srv.accepted.await.unwrap();
        assert_eq!(auth.as_deref(), Some("Bearer secret-token"));
        let task = tokio::spawn(async move { run_session(&mut client, &driver, &book, &p).await });
        (ws, task)
    }

    async fn push(ws: &mut ServerWs, v: Value) {
        ws.send(Message::Text(v.to_string())).await.unwrap();
    }

    /// Next text message from the agent, or None if nothing arrives within `within`.
    async fn next_json(ws: &mut ServerWs, within: Duration) -> Option<Value> {
        let deadline = Instant::now() + within;
        loop {
            let m = tokio::time::timeout_at(deadline, ws.next()).await.ok()??;
            if let Message::Text(t) = m.unwrap() {
                return Some(serde_json::from_str(&t).unwrap());
            }
        }
    }

    async fn expect_json(ws: &mut ServerWs) -> Value {
        next_json(ws, Duration::from_secs(5))
            .await
            .expect("expected a message from the agent")
    }

    #[tokio::test]
    async fn hello_locates_visible_chip_only() {
        let fake = Fake::with_chips(vec![chip("T1", ""), chip("T2", "")]);
        let (mut ws, task) = start(&fake, fast()).await;
        push(
            &mut ws,
            json!({"type":"hello","chips":[
                {"task_id":"task_1","title":"T1","status":"located_pending"},
                {"task_id":"task_2","title":"T2","status":"notified"}
            ]}),
        )
        .await;
        assert_eq!(
            expect_json(&mut ws).await,
            json!({"type":"chip.located","task_id":"task_1"})
        );
        // task_2 is already notified: never reported.
        assert_eq!(next_json(&mut ws, Duration::from_millis(500)).await, None);
        ws.close(None).await.unwrap();
        let end = task.await.unwrap();
        assert!(end.hello_received);
        assert!(fake.state().lists >= 1);
    }

    #[tokio::test]
    async fn not_found_after_locate_timeout() {
        let fake = Fake::default();
        let (mut ws, task) = start(&fake, fast()).await;
        let t0 = Instant::now();
        push(
            &mut ws,
            json!({"type":"chip.new","chip":{"task_id":"task_9","title":"T9","status":"located_pending"}}),
        )
        .await;
        assert_eq!(
            expect_json(&mut ws).await,
            json!({"type":"chip.not_found","task_id":"task_9"})
        );
        assert!(t0.elapsed() >= Duration::from_millis(300));
        // The chip appearing later does not produce a second report.
        fake.state().chips = vec![chip("T9", "")];
        assert_eq!(next_json(&mut ws, Duration::from_millis(200)).await, None);
        ws.close(None).await.unwrap();
        task.await.unwrap();
    }

    #[test]
    fn expire_reports_only_past_deadlines() {
        let now = Instant::now();
        let mut loc = Locator::default();
        loc.add(&wire("a", "A", ""), now, Duration::from_secs(1));
        loc.add(&wire("b", "B", ""), now, Duration::from_secs(5));
        assert!(loc.expire(now).is_empty());
        assert_eq!(
            loc.expire(now + Duration::from_secs(1)),
            vec![ClientMsg::ChipNotFound {
                task_id: "a".into()
            }]
        );
        assert_eq!(loc.task_ids(), vec!["b".to_string()]);
    }

    #[test]
    fn action_target_prefers_the_session_file() {
        let book = ChipBook::default();
        assert_eq!(
            action_target(&book, "task_1", "ws title".into(), String::new()),
            ("ws title".to_string(), None, None)
        );
        book.upsert(
            "task_1",
            BookEntry {
                session_title: "Session A".into(),
                title: "file title".into(),
                tldr: "file tldr".into(),
            },
        );
        assert_eq!(
            action_target(&book, "task_1", "ws title".into(), "ws tldr".into()),
            (
                "file title".to_string(),
                Some("file tldr".to_string()),
                Some("Session A".to_string())
            )
        );
        book.upsert(
            "task_2",
            BookEntry {
                session_title: String::new(),
                title: String::new(),
                tldr: String::new(),
            },
        );
        assert_eq!(
            action_target(&book, "task_2", "ws".into(), "d".into()),
            ("ws".to_string(), Some("d".to_string()), None)
        );
    }

    /// Regression: after `chip.new` nothing was reported for a while. The scan awaited
    /// UIA inside the WS loop, so a slow list_chips (cold Chromium a11y tree, a big
    /// sidebar) or a running action held `chip.not_found` (and every other message)
    /// back until UIA returned. Deadlines are now checked on every tick while UIA runs
    /// on its own.
    #[tokio::test]
    async fn not_found_on_time_even_while_uia_is_slow() {
        let fake = Fake::default();
        fake.state().list_delay = Duration::from_secs(3);
        let (mut ws, task) = start(&fake, fast()).await;
        let t0 = Instant::now();
        push(
            &mut ws,
            json!({"type":"chip.new","chip":{"task_id":"task_s","title":"TS","status":"located_pending"}}),
        )
        .await;
        assert_eq!(
            next_json(&mut ws, Duration::from_millis(1500)).await,
            Some(json!({"type":"chip.not_found","task_id":"task_s"}))
        );
        let took = t0.elapsed();
        assert!(took >= Duration::from_millis(300), "{took:?}");
        assert!(took < Duration::from_millis(1500), "{took:?}");
        // The loop stays responsive while the listing is still running.
        push(&mut ws, json!({"type":"ping"})).await;
        assert_eq!(
            next_json(&mut ws, Duration::from_millis(500)).await,
            Some(json!({"type":"pong"}))
        );
        ws.close(None).await.unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn not_found_and_pong_while_an_action_runs() {
        let fake = Fake::default();
        fake.state().act_delay = Duration::from_secs(2);
        let (mut ws, task) = start(&fake, fast()).await;
        push(
            &mut ws,
            json!({"type":"action","request_id":"r1","task_id":"task_a","action":"start","title":"A"}),
        )
        .await;
        push(
            &mut ws,
            json!({"type":"chip.new","chip":{"task_id":"task_b","title":"B","status":"located_pending"}}),
        )
        .await;
        push(&mut ws, json!({"type":"ping"})).await;
        assert_eq!(expect_json(&mut ws).await, json!({"type":"pong"}));
        assert_eq!(
            next_json(&mut ws, Duration::from_millis(1200)).await,
            Some(json!({"type":"chip.not_found","task_id":"task_b"}))
        );
        assert_eq!(
            expect_json(&mut ws).await,
            json!({"type":"action.result","request_id":"r1","task_id":"task_a","ok":true,"error":null})
        );
        ws.close(None).await.unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn action_uses_session_file_title_and_marks_acting() {
        let fake = Fake::default();
        let book = ChipBook::default();
        book.upsert(
            "task_f",
            BookEntry {
                session_title: "Session F".into(),
                title: "exact title".into(),
                tldr: "exact tldr".into(),
            },
        );
        let (mut ws, task) = start_with_book(&fake, fast(), book.clone()).await;
        push(
            &mut ws,
            json!({"type":"action","request_id":"r1","task_id":"task_f","action":"start","title":"exact  title (ws)","tldr":"x"}),
        )
        .await;
        assert_eq!(expect_json(&mut ws).await["ok"], true);
        assert_eq!(
            fake.state().acts[0],
            ActCall {
                title: "exact title".into(),
                tldr: Some("exact tldr".into()),
                session_title: Some("Session F".into()),
                action: Action::Start,
                raise_wait: Duration::from_millis(7)
            }
        );
        assert!(
            book.take_acting("task_f"),
            "a successful action keeps the mark (no DELETE)"
        );
        // A failed action clears the mark again.
        fake.state().act_result = Some(ActionError::ChipNotFound);
        push(
            &mut ws,
            json!({"type":"action","request_id":"r2","task_id":"task_f","action":"dismiss"}),
        )
        .await;
        assert_eq!(expect_json(&mut ws).await["ok"], false);
        assert!(!book.take_acting("task_f"));
        ws.close(None).await.unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn withdrawn_chip_is_never_reported() {
        let fake = Fake::default();
        let (mut ws, task) = start(&fake, fast()).await;
        push(
            &mut ws,
            json!({"type":"chip.new","chip":{"task_id":"task_5","title":"T5","status":"located_pending"}}),
        )
        .await;
        push(&mut ws, json!({"type":"chip.withdrawn","task_id":"task_5"})).await;
        assert_eq!(next_json(&mut ws, Duration::from_millis(700)).await, None);
        push(&mut ws, json!({"type":"ping"})).await;
        assert_eq!(expect_json(&mut ws).await, json!({"type":"pong"}));
        ws.close(None).await.unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn actions_report_ok_and_error_codes() {
        let fake = Fake::default();
        let (mut ws, task) = start(&fake, fast()).await;
        // Queued chip + action: the action result is sent, the locate is dropped.
        push(
            &mut ws,
            json!({"type":"hello","chips":[{"task_id":"task_7","title":"T7","status":"located_pending"}]}),
        )
        .await;
        push(
            &mut ws,
            json!({"type":"action","request_id":"r1","task_id":"task_7","action":"start","title":"T7","tldr":"d7"}),
        )
        .await;
        assert_eq!(
            expect_json(&mut ws).await,
            json!({"type":"action.result","request_id":"r1","task_id":"task_7","ok":true,"error":null})
        );
        fake.state().act_result = Some(ActionError::ChipNotFound);
        push(
            &mut ws,
            json!({"type":"action","request_id":"r2","task_id":"task_8","action":"dismiss","title":"T8","tldr":""}),
        )
        .await;
        assert_eq!(
            expect_json(&mut ws).await,
            json!({"type":"action.result","request_id":"r2","task_id":"task_8","ok":false,"error":"chip_not_found"})
        );
        fake.state().act_result = Some(ActionError::ClaudeNotRunning);
        push(
            &mut ws,
            json!({"type":"action","request_id":"r3","task_id":"task_8","action":"start","title":"T8"}),
        )
        .await;
        assert_eq!(expect_json(&mut ws).await["error"], "claude_not_running");
        push(
            &mut ws,
            json!({"type":"action","request_id":"r4","task_id":"task_8","action":"explode"}),
        )
        .await;
        assert_eq!(
            expect_json(&mut ws).await,
            json!({"type":"action.result","request_id":"r4","task_id":"task_8","ok":false,"error":"invoke_failed"})
        );
        // task_7 was dropped from the queue by the action: no not_found for it.
        assert_eq!(next_json(&mut ws, Duration::from_millis(500)).await, None);
        {
            let s = fake.state();
            assert_eq!(s.acts.len(), 3);
            assert_eq!(
                s.acts[0],
                ActCall {
                    title: "T7".into(),
                    tldr: Some("d7".into()),
                    session_title: None,
                    action: Action::Start,
                    raise_wait: Duration::from_millis(7)
                }
            );
            assert_eq!(s.acts[1].tldr, None, "empty tldr is passed as None");
            assert_eq!(s.acts[1].action, Action::Dismiss);
        }
        ws.close(None).await.unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn ping_pong() {
        let fake = Fake::default();
        let (mut ws, task) = start(&fake, fast()).await;
        push(&mut ws, json!({"type":"ping"})).await;
        assert_eq!(expect_json(&mut ws).await, json!({"type":"pong"}));
        ws.close(None).await.unwrap();
        let end = task.await.unwrap();
        assert!(!end.hello_received);
    }

    #[tokio::test]
    async fn close_4000_means_takeover() {
        let fake = Fake::default();
        let (mut ws, task) = start(&fake, fast()).await;
        push(&mut ws, json!({"type":"hello","chips":[]})).await;
        ws.close(Some(CloseFrame {
            code: CloseCode::from(4000),
            reason: "replaced".into(),
        }))
        .await
        .unwrap();
        let end = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            end,
            SessionEnd {
                close_code: Some(4000),
                hello_received: true
            }
        );
        let mut b = 8;
        assert_eq!(
            plan_retry(&end, &mut b, 300),
            (Status::Replaced, Duration::from_secs(300))
        );
    }

    #[tokio::test]
    async fn keepalive_ping_and_dead_link_detection() {
        let fake = Fake::default();
        let p = SessionParams {
            keepalive: Duration::from_millis(50),
            dead_after: Duration::from_millis(200),
            ..fast()
        };
        let (mut ws, task) = start(&fake, p).await;
        // The server (tungstenite) answers the agent's Ping while we read.
        let got = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(Ok(Message::Ping(_))) = ws.next().await {
                    // Push out the auto-queued Pong.
                    ws.flush().await.unwrap();
                    return;
                }
            }
        })
        .await;
        assert!(got.is_ok(), "agent sends WS pings");
        // Stop reading: pongs stop, the agent gives up on the silent link.
        let end = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("session ends on a silent link")
            .unwrap();
        assert_eq!(end.close_code, None);
        drop(ws);
    }

    #[tokio::test]
    async fn run_forever_connects_and_reports_takeover() {
        let srv = server().await;
        let dir = std::env::temp_dir().join(format!("chip-remote-agent-rf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            json!({"url": srv.cfg.url, "token": "secret-token", "takeoverBackoffSec": 300})
                .to_string(),
        )
        .unwrap();
        let (st_tx, mut st_rx) = tokio::sync::mpsc::unbounded_channel();
        let env = AgentEnv {
            config_path: path,
            driver: spawn(Fake::default()),
            status: Arc::new(move |s| {
                let _ = st_tx.send(s);
            }),
            book: ChipBook::default(),
        };
        let agent = tokio::spawn(run_forever(env));
        let (mut ws, auth) = srv.accepted.await.unwrap();
        assert_eq!(auth.as_deref(), Some("Bearer secret-token"));
        assert_eq!(st_rx.recv().await, Some(Status::Connected));
        push(&mut ws, json!({"type":"hello","chips":[]})).await;
        ws.close(Some(CloseFrame {
            code: CloseCode::from(4000),
            reason: "replaced".into(),
        }))
        .await
        .unwrap();
        let s = tokio::time::timeout(Duration::from_secs(5), st_rx.recv())
            .await
            .unwrap();
        assert_eq!(s, Some(Status::Replaced));
        agent.abort();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test(start_paused = true)]
    async fn missing_config_sets_status_and_retries_every_30s() {
        let dir =
            std::env::temp_dir().join(format!("chip-remote-agent-nocfg-{}", std::process::id()));
        let (st_tx, mut st_rx) = tokio::sync::mpsc::unbounded_channel();
        let env = AgentEnv {
            config_path: dir.join("does-not-exist.json"),
            driver: spawn(Fake::default()),
            status: Arc::new(move |s| {
                let _ = st_tx.send(s);
            }),
            book: ChipBook::default(),
        };
        let t0 = Instant::now();
        let agent = tokio::spawn(run_forever(env));
        assert_eq!(st_rx.recv().await, Some(Status::NoConfig));
        assert_eq!(st_rx.recv().await, Some(Status::NoConfig));
        assert!(t0.elapsed() >= NO_CONFIG_RETRY);
        assert_eq!(st_rx.recv().await, Some(Status::NoConfig));
        assert!(t0.elapsed() >= NO_CONFIG_RETRY * 2);
        agent.abort();
    }

    #[tokio::test]
    async fn connect_errors_do_not_leak_the_token() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf).await;
            let _ = s
                .write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n")
                .await;
        });
        let cfg = Config {
            url: format!("http://127.0.0.1:{port}"),
            token: "secret-token".into(),
            ..Config::default()
        };
        let e = connect(&cfg).await.unwrap_err();
        assert!(e.contains("401"), "{e}");
        assert!(!e.contains("secret-token"));
    }
}
