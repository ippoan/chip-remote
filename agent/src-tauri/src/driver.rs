//! UIA access behind a trait, served by ONE dedicated OS thread.
//!
//! UIA COM objects are apartment-bound (`chip_uia::Uia` is not `Send`), so the driver is
//! created and used only on the `chip-uia` thread. The async side talks to it through
//! [`DriverHandle`] (unbounded channel in, oneshot reply out). The thread processes one
//! request at a time, which also serialises "raise Claude + press" against scans.

use std::time::Duration;

use chip_core::protocol::Action;
use chip_core::{ActionError, ChipInfo, Labels};
use tokio::sync::{mpsc, oneshot};

/// What the agent needs from UI Automation.
pub trait ChipDriver {
    /// Read-only listing of the chips visible right now. Must never move windows.
    fn list_chips(&self) -> Result<Vec<ChipInfo>, ActionError>;
    /// Raise Claude briefly, find the chip (paging if needed) and press the button.
    /// `session_title` (from the session file, when known) lets the driver bring that
    /// session's pane up first.
    fn act(
        &self,
        title: &str,
        tldr: Option<&str>,
        session_title: Option<&str>,
        action: Action,
        raise_wait: Duration,
    ) -> Result<(), ActionError>;
}

/// Creates drivers on the driver thread (so the driver itself never crosses threads).
pub trait DriverFactory: Send + 'static {
    type Driver: ChipDriver;
    fn create(&mut self, labels: &Labels) -> Result<Self::Driver, ActionError>;
    /// Block system sleep for as long as the calling (driver) thread lives.
    fn keep_awake(&mut self) -> bool;
}

enum Request {
    Configure {
        labels: Labels,
        prevent_sleep: bool,
    },
    List {
        reply: oneshot::Sender<Result<Vec<ChipInfo>, ActionError>>,
    },
    Act {
        title: String,
        tldr: Option<String>,
        session_title: Option<String>,
        action: Action,
        raise_wait: Duration,
        reply: oneshot::Sender<Result<(), ActionError>>,
    },
}

/// Cheap, cloneable, `Send` handle to the driver thread.
#[derive(Clone)]
pub struct DriverHandle {
    tx: mpsc::UnboundedSender<Request>,
}

fn thread_gone() -> ActionError {
    ActionError::InvokeFailed("driver thread is gone".into())
}

impl DriverHandle {
    /// Applies (possibly reloaded) config: labels (the driver is recreated when they
    /// change) and keep-awake (turned on once; turning it off needs a restart).
    pub fn configure(&self, labels: Labels, prevent_sleep: bool) {
        let _ = self.tx.send(Request::Configure {
            labels,
            prevent_sleep,
        });
    }

    pub async fn list_chips(&self) -> Result<Vec<ChipInfo>, ActionError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Request::List { reply })
            .map_err(|_| thread_gone())?;
        rx.await.map_err(|_| thread_gone())?
    }

    pub async fn act(
        &self,
        title: String,
        tldr: Option<String>,
        session_title: Option<String>,
        action: Action,
        raise_wait: Duration,
    ) -> Result<(), ActionError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Request::Act {
                title,
                tldr,
                session_title,
                action,
                raise_wait,
                reply,
            })
            .map_err(|_| thread_gone())?;
        rx.await.map_err(|_| thread_gone())?
    }
}

/// Starts the driver thread. It ends when every [`DriverHandle`] is dropped.
pub fn spawn<F: DriverFactory>(factory: F) -> DriverHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("chip-uia".into())
        .spawn(move || serve(factory, rx))
        .expect("failed to spawn the chip-uia thread");
    DriverHandle { tx }
}

fn serve<F: DriverFactory>(mut factory: F, mut rx: mpsc::UnboundedReceiver<Request>) {
    let mut labels = Labels::default();
    let mut driver: Option<F::Driver> = None;
    let mut awake = false;

    // Creates the driver lazily; a failed creation (e.g. COM init) is retried next time.
    fn ensure<'a, F: DriverFactory>(
        factory: &mut F,
        driver: &'a mut Option<F::Driver>,
        labels: &Labels,
    ) -> Result<&'a F::Driver, ActionError> {
        if driver.is_none() {
            *driver = Some(factory.create(labels).inspect_err(|e| {
                tracing::warn!(error = %e, "uia: driver init failed");
            })?);
        }
        Ok(driver.as_ref().expect("just set"))
    }

    while let Some(req) = rx.blocking_recv() {
        match req {
            Request::Configure {
                labels: l,
                prevent_sleep,
            } => {
                if prevent_sleep && !awake {
                    awake = factory.keep_awake();
                    if awake {
                        tracing::info!("keep-awake on (system sleep blocked while the agent runs)");
                    } else {
                        tracing::warn!("keep-awake request failed");
                    }
                }
                if l != labels {
                    tracing::info!("uia: labels changed; recreating driver");
                    labels = l;
                    driver = None;
                }
            }
            Request::List { reply } => {
                let r = ensure(&mut factory, &mut driver, &labels).and_then(|d| d.list_chips());
                let _ = reply.send(r);
            }
            Request::Act {
                title,
                tldr,
                session_title,
                action,
                raise_wait,
                reply,
            } => {
                let r = ensure(&mut factory, &mut driver, &labels).and_then(|d| {
                    d.act(
                        &title,
                        tldr.as_deref(),
                        session_title.as_deref(),
                        action,
                        raise_wait,
                    )
                });
                let _ = reply.send(r);
            }
        }
    }
    tracing::info!("uia: driver thread exiting");
}

#[cfg(test)]
pub mod fake {
    //! In-memory driver for tests: a shared list of "visible" chips and scripted results.
    use super::*;
    use std::sync::{Arc, Mutex};

    /// One recorded `act` call.
    #[derive(Debug, Clone, PartialEq)]
    pub struct ActCall {
        pub title: String,
        pub tldr: Option<String>,
        pub session_title: Option<String>,
        pub action: Action,
        pub raise_wait: Duration,
    }

    #[derive(Default)]
    pub struct FakeState {
        pub chips: Vec<ChipInfo>,
        /// Result of the next `act` calls (default Ok).
        pub act_result: Option<ActionError>,
        pub acts: Vec<ActCall>,
        /// How long `list_chips` / `act` block the driver thread (a slow / hung UIA).
        pub list_delay: Duration,
        pub act_delay: Duration,
        pub lists: usize,
        pub creates: Vec<Labels>,
        pub keep_awake_calls: usize,
        pub fail_create: bool,
        /// `thread::current().name()` seen by the driver (must be the driver thread).
        pub threads: Vec<Option<String>>,
    }

    #[derive(Clone, Default)]
    pub struct Fake(pub Arc<Mutex<FakeState>>);

    impl Fake {
        pub fn with_chips(chips: Vec<ChipInfo>) -> Fake {
            let f = Fake::default();
            f.0.lock().unwrap().chips = chips;
            f
        }
        pub fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
            self.0.lock().unwrap()
        }
    }

    pub struct FakeDriver(Fake);

    impl ChipDriver for FakeDriver {
        fn list_chips(&self) -> Result<Vec<ChipInfo>, ActionError> {
            let delay = self.0.state().list_delay;
            std::thread::sleep(delay);
            let mut s = self.0.state();
            s.lists += 1;
            s.threads
                .push(std::thread::current().name().map(str::to_string));
            Ok(s.chips.clone())
        }
        fn act(
            &self,
            title: &str,
            tldr: Option<&str>,
            session_title: Option<&str>,
            action: Action,
            raise_wait: Duration,
        ) -> Result<(), ActionError> {
            let delay = self.0.state().act_delay;
            std::thread::sleep(delay);
            let mut s = self.0.state();
            s.acts.push(ActCall {
                title: title.to_string(),
                tldr: tldr.map(str::to_string),
                session_title: session_title.map(str::to_string),
                action,
                raise_wait,
            });
            match &s.act_result {
                Some(e) => Err(e.clone()),
                None => Ok(()),
            }
        }
    }

    impl DriverFactory for Fake {
        type Driver = FakeDriver;
        fn create(&mut self, labels: &Labels) -> Result<FakeDriver, ActionError> {
            let mut s = self.state();
            if s.fail_create {
                return Err(ActionError::InvokeFailed("CoInitializeEx failed".into()));
            }
            s.creates.push(labels.clone());
            Ok(FakeDriver(self.clone()))
        }
        fn keep_awake(&mut self) -> bool {
            self.state().keep_awake_calls += 1;
            true
        }
    }

    pub fn chip(title: &str, tldr: &str) -> ChipInfo {
        ChipInfo {
            title: title.into(),
            tldr: tldr.into(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    #[tokio::test]
    async fn serves_requests_on_the_driver_thread() {
        let fake = Fake::with_chips(vec![chip("a", "")]);
        let h = spawn(fake.clone());
        h.configure(Labels::default(), true);
        h.configure(Labels::default(), true);
        let chips = h.list_chips().await.unwrap();
        assert_eq!(chips.len(), 1);
        h.act(
            "a".into(),
            Some("d".into()),
            Some("S".into()),
            Action::Start,
            Duration::from_millis(5),
        )
        .await
        .unwrap();
        let s = fake.state();
        assert_eq!(s.keep_awake_calls, 1, "keep-awake is requested once");
        assert_eq!(s.creates.len(), 1, "driver is created once and reused");
        assert_eq!(s.threads, vec![Some("chip-uia".to_string())]);
        assert_eq!(
            s.acts,
            vec![ActCall {
                title: "a".into(),
                tldr: Some("d".into()),
                session_title: Some("S".into()),
                action: Action::Start,
                raise_wait: Duration::from_millis(5)
            }]
        );
    }

    #[tokio::test]
    async fn no_keep_awake_when_disabled() {
        let fake = Fake::default();
        let h = spawn(fake.clone());
        h.configure(Labels::default(), false);
        h.list_chips().await.unwrap();
        assert_eq!(fake.state().keep_awake_calls, 0);
    }

    #[tokio::test]
    async fn label_change_recreates_driver() {
        let fake = Fake::default();
        let h = spawn(fake.clone());
        h.list_chips().await.unwrap();
        let labels = Labels {
            start: "Start".into(),
            ..Labels::default()
        };
        h.configure(labels.clone(), false);
        h.list_chips().await.unwrap();
        h.configure(labels.clone(), false);
        h.list_chips().await.unwrap();
        let s = fake.state();
        assert_eq!(s.creates, vec![Labels::default(), labels]);
    }

    #[tokio::test]
    async fn init_failure_is_reported_and_retried() {
        let fake = Fake::default();
        fake.state().fail_create = true;
        let h = spawn(fake.clone());
        let e = h.list_chips().await.unwrap_err();
        assert_eq!(e.code(), "invoke_failed");
        let e = h
            .act("x".into(), None, None, Action::Dismiss, Duration::ZERO)
            .await
            .unwrap_err();
        assert_eq!(e.code(), "invoke_failed");
        fake.state().fail_create = false;
        assert!(h.list_chips().await.is_ok());
    }

    #[tokio::test]
    async fn errors_pass_through() {
        let fake = Fake::default();
        fake.state().act_result = Some(ActionError::ButtonNotFound);
        let h = spawn(fake.clone());
        let e = h
            .act("x".into(), None, None, Action::Dismiss, Duration::ZERO)
            .await
            .unwrap_err();
        assert_eq!(e, ActionError::ButtonNotFound);
    }
}
