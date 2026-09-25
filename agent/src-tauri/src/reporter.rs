//! Reports chips found in the session files to the Worker over the same HTTP API the
//! hooks use (docs/PROTOCOL.md): `POST /v1/chips` (201 new / 200 already known) and
//! `DELETE /v1/chips/:task_id` (404 = already unknown, fine). Both are idempotent, so
//! the hooks may keep reporting the same chips.
//!
//! Every attempt re-reads config.json (a fixed token / url / Access service token takes
//! effect without a restart). Network errors, 5xx, 408, 429 and 401 / 403 are retried
//! with a backoff (1 → 2 → … → 60 s); other 4xx give up. The token and the Access
//! secret are never logged.
//!
//! Cloudflare Access (docs/PROTOCOL.md 「認証」): `CF-Access-Client-Id` /
//! `CF-Access-Client-Secret` go with every request when both are configured. Redirects
//! are not followed, so Access sending us to its login page (or answering 401 / 403
//! itself) is recognised, logged as [`ACCESS_DENIED_MESSAGE`] and retried.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chip_core::access::{is_access_rejection, ACCESS_DENIED_MESSAGE};
use chip_core::Config;
use reqwest::header::{HeaderValue, CONTENT_TYPE, LOCATION};
use reqwest::Client;
use serde_json::json;

/// A chip as `POST /v1/chips` takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewChip {
    pub task_id: String,
    pub title: String,
    pub tldr: String,
    pub cwd: String,
    pub host: String,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    Post(NewChip),
    Delete(String),
}

impl Job {
    pub fn task_id(&self) -> &str {
        match self {
            Job::Post(c) => &c.task_id,
            Job::Delete(id) => id,
        }
    }
    fn verb(&self) -> &'static str {
        match self {
            Job::Post(_) => "POST",
            Job::Delete(_) => "DELETE",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub initial: Duration,
    pub max: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(60),
        }
    }
}

/// What to do with an HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Done,
    Retry,
    GiveUp,
}

pub fn classify(job: &Job, status: u16) -> Verdict {
    match status {
        200..=299 => Verdict::Done,
        // DELETE of a chip the Worker never saw (or already forgot).
        404 if matches!(job, Job::Delete(_)) => Verdict::Done,
        // 401 / 403: the token may be fixed in config.json while we wait.
        401 | 403 | 408 | 429 | 500..=599 => Verdict::Retry,
        _ => Verdict::GiveUp,
    }
}

/// `host` for chips from this PC: config `host`, else the machine name.
pub fn host_name(cfg: &Config) -> String {
    let h = cfg.host.trim();
    if !h.is_empty() {
        return h.to_string();
    }
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "desktop".to_string())
}

pub fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(10))
        // An Access login redirect must surface as a 3xx, not as the login page's 200.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_else(|e| {
            tracing::warn!("http client: {e}; using defaults");
            Client::new()
        })
}

/// One attempt. Ok(status) or Err(reason without the token / Access secret).
async fn attempt(client: &Client, cfg: &Config, job: &Job) -> Result<u16, String> {
    let base = cfg.url.trim_end_matches('/');
    let req = match job {
        Job::Post(c) => client.post(format!("{base}/v1/chips")).json(&json!({
            "task_id": c.task_id,
            "title": c.title,
            "tldr": c.tldr,
            "cwd": c.cwd,
            "host": c.host,
            "session_id": c.session_id,
        })),
        Job::Delete(id) => client.delete(format!("{base}/v1/chips/{id}")),
    };
    let mut req = req.bearer_auth(&cfg.token);
    if let Some(pairs) = cfg.access_headers() {
        for (name, value) in pairs {
            let mut v = HeaderValue::from_str(value)
                .map_err(|_| format!("{name} contains characters not allowed in a header"))?;
            v.set_sensitive(true);
            req = req.header(name, v);
        }
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let h = resp.headers();
            let location = h.get(LOCATION).and_then(|v| v.to_str().ok());
            let content_type = h.get(CONTENT_TYPE).and_then(|v| v.to_str().ok());
            if is_access_rejection(status, location, content_type) {
                return Err(format!("{ACCESS_DENIED_MESSAGE} (HTTP {status})"));
            }
            Ok(status)
        }
        // reqwest errors carry the URL but never request headers.
        Err(e) => Err(e.without_url().to_string()),
    }
}

/// Delivers `job`, retrying as described in the module docs. `load_config` is called
/// before every attempt (None: config missing / incomplete → wait and retry).
/// Returns true when the Worker accepted it.
pub async fn deliver<F>(client: &Client, load_config: F, job: &Job, policy: RetryPolicy) -> bool
where
    F: Fn() -> Option<Config>,
{
    let mut wait = policy.initial;
    loop {
        let reason = match load_config() {
            None => "config.json has no url / token".to_string(),
            Some(cfg) => match attempt(client, &cfg, job).await {
                Ok(status) => match classify(job, status) {
                    Verdict::Done => {
                        tracing::info!("{} {}: HTTP {status}", job.verb(), job.task_id());
                        return true;
                    }
                    Verdict::GiveUp => {
                        tracing::warn!(
                            "{} {}: HTTP {status}; giving up",
                            job.verb(),
                            job.task_id()
                        );
                        return false;
                    }
                    Verdict::Retry => format!("HTTP {status}"),
                },
                Err(e) => e,
            },
        };
        tracing::warn!(
            "{} {}: {reason}; retry in {:?}",
            job.verb(),
            job.task_id(),
            wait
        );
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(policy.max);
    }
}

/// Fire-and-forget `DELETE /v1/chips/:task_id` for the reconcile (hello / full scan /
/// an action on a chip already resolved on the PC). Each runs [`deliver`] on its own
/// task (so a retrying DELETE never blocks the WS loop or the watcher); a task_id
/// already being delivered is not queued twice.
#[derive(Clone)]
pub struct Withdrawer(Arc<WithdrawInner>);

struct WithdrawInner {
    config_path: PathBuf,
    client: Client,
    retry: RetryPolicy,
    in_flight: Mutex<HashSet<String>>,
}

impl Withdrawer {
    pub fn new(config_path: PathBuf, retry: RetryPolicy) -> Withdrawer {
        Withdrawer(Arc::new(WithdrawInner {
            config_path,
            client: client(),
            retry,
            in_flight: Mutex::new(HashSet::new()),
        }))
    }

    /// Queues the DELETE (needs a tokio runtime). `why` is for the log.
    pub fn withdraw(&self, task_id: String, why: &str) {
        {
            let mut f = self.0.in_flight.lock().unwrap_or_else(|e| e.into_inner());
            if !f.insert(task_id.clone()) {
                return;
            }
        }
        tracing::info!("reconcile: withdrawing {task_id} ({why})");
        let inner = self.0.clone();
        tokio::spawn(async move {
            let load = || Config::load(&inner.config_path).ok();
            let job = Job::Delete(task_id);
            deliver(&inner.client, load, &job, inner.retry).await;
            inner
                .in_flight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(job.task_id());
        });
    }
}

#[cfg(test)]
pub mod fake_http {
    //! Minimal in-process HTTP/1.1 server: records requests, answers scripted statuses
    //! (0 = drop the connection without answering).
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[derive(Debug, Clone, PartialEq)]
    pub struct Recorded {
        pub method: String,
        pub path: String,
        pub auth: Option<String>,
        /// `CF-Access-Client-Id` / `CF-Access-Client-Secret`.
        pub access_id: Option<String>,
        pub access_secret: Option<String>,
        pub body: String,
    }

    /// A scripted answer. Status 0 = drop the connection without answering.
    #[derive(Debug, Clone)]
    pub struct Reply {
        pub status: u16,
        pub content_type: &'static str,
        pub location: Option<&'static str>,
    }

    /// Where Access sends a request without a valid service token (login page).
    pub const ACCESS_LOGIN: &str =
        "https://ippoan.cloudflareaccess.com/cdn-cgi/access/login/chip-remote.ippoan.org";

    impl Reply {
        /// The Worker's JSON answer.
        pub fn json(status: u16) -> Reply {
            Reply {
                status,
                content_type: "application/json",
                location: None,
            }
        }
        /// Access redirecting to its login page.
        pub fn access_redirect() -> Reply {
            Reply {
                status: 302,
                content_type: "text/html",
                location: Some(ACCESS_LOGIN),
            }
        }
        /// Access refusing a bad service token itself (HTML, not the Worker's JSON).
        pub fn access_forbidden() -> Reply {
            Reply {
                status: 403,
                content_type: "text/html; charset=UTF-8",
                location: None,
            }
        }
    }

    #[derive(Clone, Default)]
    pub struct FakeHttp {
        pub requests: Arc<Mutex<Vec<Recorded>>>,
        script: Arc<Mutex<VecDeque<Reply>>>,
        pub url: String,
    }

    impl FakeHttp {
        pub fn requests(&self) -> Vec<Recorded> {
            self.requests.lock().unwrap().clone()
        }
        /// Statuses (JSON answers) for the next requests; afterwards 201 for POST, 200
        /// otherwise.
        pub fn script(&self, statuses: &[u16]) {
            self.replies(statuses.iter().map(|&s| Reply::json(s)).collect());
        }
        /// Like [`FakeHttp::script`] with full answers.
        pub fn replies(&self, replies: Vec<Reply>) {
            self.script.lock().unwrap().extend(replies);
        }
    }

    pub async fn start() -> FakeHttp {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let f = FakeHttp {
            url,
            ..FakeHttp::default()
        };
        let srv = f.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else {
                    return;
                };
                let srv = srv.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let head_end = loop {
                        let Ok(n) = s.read(&mut chunk).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let mut lines = head.lines();
                    let mut first = lines.next().unwrap_or_default().split(' ');
                    let method = first.next().unwrap_or_default().to_string();
                    let path = first.next().unwrap_or_default().to_string();
                    let mut auth = None;
                    let mut access_id = None;
                    let mut access_secret = None;
                    let mut len = 0usize;
                    for l in lines {
                        if let Some((k, v)) = l.split_once(':') {
                            match k.trim().to_ascii_lowercase().as_str() {
                                "authorization" => auth = Some(v.trim().to_string()),
                                "cf-access-client-id" => access_id = Some(v.trim().to_string()),
                                "cf-access-client-secret" => {
                                    access_secret = Some(v.trim().to_string())
                                }
                                "content-length" => len = v.trim().parse().unwrap_or(0),
                                _ => {}
                            }
                        }
                    }
                    while buf.len() < head_end + len {
                        let Ok(n) = s.read(&mut chunk).await else {
                            return;
                        };
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let body = String::from_utf8_lossy(&buf[head_end..]).to_string();
                    let reply =
                        srv.script.lock().unwrap().pop_front().unwrap_or_else(|| {
                            Reply::json(if method == "POST" { 201 } else { 200 })
                        });
                    srv.requests.lock().unwrap().push(Recorded {
                        method,
                        path,
                        auth,
                        access_id,
                        access_secret,
                        body,
                    });
                    if reply.status == 0 {
                        return; // drop: the client sees a network error
                    }
                    let body = if reply.content_type.contains("json") {
                        "{}"
                    } else {
                        "<html>Forbidden</html>"
                    };
                    let location = reply
                        .location
                        .map(|l| format!("location: {l}\r\n"))
                        .unwrap_or_default();
                    let resp = format!(
                        "HTTP/1.1 {} X\r\ncontent-type: {}\r\n{location}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                        reply.status,
                        reply.content_type,
                        body.len()
                    );
                    let _ = s.write_all(resp.as_bytes()).await;
                    let _ = s.shutdown().await;
                });
            }
        });
        f
    }
}

#[cfg(test)]
mod tests {
    use super::fake_http::start;
    use super::*;

    fn cfg(url: &str) -> Config {
        Config {
            url: url.into(),
            token: "secret-token".into(),
            ..Config::default()
        }
    }

    fn fast() -> RetryPolicy {
        RetryPolicy {
            initial: Duration::from_millis(5),
            max: Duration::from_millis(20),
        }
    }

    fn chip() -> NewChip {
        NewChip {
            task_id: "task_0a1b".into(),
            title: "タイトル".into(),
            tldr: "d".into(),
            cwd: "C:\\w".into(),
            host: "PC1".into(),
            session_id: Some("cli-1".into()),
        }
    }

    #[test]
    fn classify_statuses() {
        let p = Job::Post(chip());
        let d = Job::Delete("task_1".into());
        assert_eq!(classify(&p, 201), Verdict::Done);
        assert_eq!(classify(&p, 200), Verdict::Done);
        assert_eq!(classify(&p, 404), Verdict::GiveUp);
        assert_eq!(classify(&d, 404), Verdict::Done);
        assert_eq!(classify(&p, 400), Verdict::GiveUp);
        for s in [401, 403, 408, 429, 500, 503] {
            assert_eq!(classify(&p, s), Verdict::Retry, "{s}");
        }
    }

    #[test]
    fn host_from_config_or_machine() {
        let mut c = Config::default();
        assert!(!host_name(&c).is_empty());
        c.host = " mini ".into();
        assert_eq!(host_name(&c), "mini");
    }

    #[tokio::test]
    async fn posts_chip_with_bearer_and_body() {
        let srv = start().await;
        let c = cfg(&format!("{}/", srv.url));
        assert!(deliver(&client(), || Some(c.clone()), &Job::Post(chip()), fast()).await);
        let r = srv.requests();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].method, "POST");
        assert_eq!(r[0].path, "/v1/chips");
        assert_eq!(r[0].auth.as_deref(), Some("Bearer secret-token"));
        // Access not configured: no Access headers at all.
        assert_eq!(r[0].access_id, None);
        assert_eq!(r[0].access_secret, None);
        let body: serde_json::Value = serde_json::from_str(&r[0].body).unwrap();
        assert_eq!(
            body,
            json!({"task_id":"task_0a1b","title":"タイトル","tldr":"d","cwd":"C:\\w",
                   "host":"PC1","session_id":"cli-1"})
        );
    }

    #[tokio::test]
    async fn retries_network_errors_and_5xx_then_succeeds() {
        let srv = start().await;
        srv.script(&[0, 503, 401, 200]);
        let c = cfg(&srv.url);
        assert!(deliver(&client(), || Some(c.clone()), &Job::Post(chip()), fast()).await);
        assert_eq!(srv.requests().len(), 4);
    }

    #[tokio::test]
    async fn delete_404_is_fine_and_400_gives_up() {
        let srv = start().await;
        srv.script(&[404, 400]);
        let c = cfg(&srv.url);
        assert!(
            deliver(
                &client(),
                || Some(c.clone()),
                &Job::Delete("task_9".into()),
                fast()
            )
            .await
        );
        assert!(!deliver(&client(), || Some(c.clone()), &Job::Post(chip()), fast()).await);
        let r = srv.requests();
        assert_eq!(r[0].method, "DELETE");
        assert_eq!(r[0].path, "/v1/chips/task_9");
        assert_eq!(r.len(), 2);
    }

    #[tokio::test]
    async fn withdrawer_deletes_once_per_task_while_in_flight() {
        let srv = start().await;
        let dir = std::env::temp_dir().join(format!(
            "chip-remote-withdraw-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            json!({"url": srv.url, "token": "secret-token"}).to_string(),
        )
        .unwrap();
        // First attempt 503 → retried; a second request for the same task meanwhile is
        // not queued again.
        srv.script(&[503]);
        let w = Withdrawer::new(path, fast());
        w.withdraw("task_1".into(), "test");
        w.withdraw("task_1".into(), "test");
        let wait = |n: usize| {
            let srv = srv.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(10), async {
                    while srv.requests().len() < n {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("requests")
            }
        };
        wait(2).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let r = srv.requests();
        assert_eq!(r.len(), 2, "{r:?}");
        assert!(r
            .iter()
            .all(|x| x.method == "DELETE" && x.path == "/v1/chips/task_1"));
        assert_eq!(r[0].auth.as_deref(), Some("Bearer secret-token"));
        // Delivered: a later withdraw is sent again (the Worker answers 200 / 404).
        w.withdraw("task_1".into(), "test");
        wait(3).await;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn waits_for_config_and_errors_do_not_leak_the_token() {
        let srv = start().await;
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let c = cfg(&srv.url);
        let load = || {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (n >= 2).then(|| c.clone())
        };
        assert!(deliver(&client(), load, &Job::Delete("task_1".into()), fast()).await);
        assert_eq!(srv.requests().len(), 1);
        // A connection error message never contains the token.
        let dead = cfg("http://127.0.0.1:1");
        let e = attempt(&client(), &dead, &Job::Post(chip()))
            .await
            .unwrap_err();
        assert!(!e.contains("secret-token"), "{e}");
    }

    fn cfg_with_access(url: &str) -> Config {
        Config {
            access_client_id: "0123abcd.access".into(),
            access_client_secret: "access-secret-value".into(),
            ..cfg(url)
        }
    }

    #[tokio::test]
    async fn sends_access_headers_on_post_and_delete() {
        let srv = start().await;
        let c = cfg_with_access(&srv.url);
        assert!(deliver(&client(), || Some(c.clone()), &Job::Post(chip()), fast()).await);
        assert!(
            deliver(
                &client(),
                || Some(c.clone()),
                &Job::Delete("task_1".into()),
                fast()
            )
            .await
        );
        let r = srv.requests();
        assert_eq!(r.len(), 2);
        for x in &r {
            assert_eq!(x.auth.as_deref(), Some("Bearer secret-token"), "{x:?}");
            assert_eq!(x.access_id.as_deref(), Some("0123abcd.access"), "{x:?}");
            assert_eq!(x.access_secret.as_deref(), Some("access-secret-value"));
        }
        // Only one of the two set: neither is sent.
        let half = Config {
            access_client_secret: String::new(),
            ..c.clone()
        };
        assert!(deliver(&client(), || Some(half.clone()), &Job::Post(chip()), fast()).await);
        let r = srv.requests();
        assert_eq!(
            (r[2].access_id.clone(), r[2].access_secret.clone()),
            (None, None)
        );
    }

    #[tokio::test]
    async fn access_rejection_is_logged_and_retried_without_leaking_the_secret() {
        use crate::logging::capture;
        use fake_http::Reply;
        use tracing::instrument::WithSubscriber as _;

        let srv = start().await;
        // Access login redirect (not followed), Access 403 page, the Worker's own JSON
        // 401, then accepted.
        srv.replies(vec![
            Reply::access_redirect(),
            Reply::access_forbidden(),
            Reply::json(401),
            Reply::json(201),
        ]);
        let c = cfg_with_access(&srv.url);
        let (sub, logs) = capture::subscriber();
        let ok = deliver(&client(), || Some(c.clone()), &Job::Post(chip()), fast())
            .with_subscriber(sub)
            .await;
        assert!(ok);
        assert_eq!(srv.requests().len(), 4, "the redirect is not followed");
        let text = logs.text();
        assert_eq!(
            text.matches(ACCESS_DENIED_MESSAGE).count(),
            2,
            "302 to Access and Access 403 (not the Worker's 401): {text}"
        );
        assert!(
            text.contains("HTTP 302") && text.contains("HTTP 403"),
            "{text}"
        );
        assert!(text.contains("HTTP 401; retry"), "{text}");
        for secret in ["access-secret-value", "secret-token", "0123abcd.access"] {
            assert!(!text.contains(secret), "{secret} leaked: {text}");
        }
    }
}
