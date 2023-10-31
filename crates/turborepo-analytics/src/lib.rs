#![deny(clippy::all)]

use std::time::Duration;

use thiserror::Error;
use tokio::{
    select,
    sync::{mpsc, oneshot},
    task::{JoinError, JoinHandle},
};
use tracing::debug;
use turborepo_api_client::{APIAuth, APIClient};
pub use turborepo_vercel_api::AnalyticsEvent;
use uuid::Uuid;

const BUFFER_THRESHOLD: usize = 10;

static EVENT_TIMEOUT: Duration = Duration::from_millis(200);
static NO_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
static REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CHANNEL_SIZE: usize = 100;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed to send analytics event")]
    SendError(#[from] mpsc::error::SendError<AnalyticsEvent>),
    #[error("Failed to record analytics")]
    Join(#[from] JoinError),
}

// We have two different types because the AnalyticsSender should be shared
// across threads (i.e. Clone + Send), while the AnalyticsHandle cannot be
// shared since it contains the structs necessary to shut down the worker.
pub type AnalyticsSender = mpsc::Sender<AnalyticsEvent>;

pub struct AnalyticsHandle {
    exit_ch: oneshot::Receiver<()>,
    handle: JoinHandle<()>,
}

pub fn start_analytics(api_auth: APIAuth, client: APIClient) -> (AnalyticsSender, AnalyticsHandle) {
    let (tx, rx) = mpsc::channel(CHANNEL_SIZE);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let session_id = Uuid::new_v4();
    let worker = Worker {
        rx,
        buffer: Vec::new(),
        session_id,
        api_auth,
        exit_ch: cancel_tx,
        client,
    };
    let handle = worker.start();

    let analytics_handle = AnalyticsHandle {
        exit_ch: cancel_rx,
        handle,
    };

    (tx, analytics_handle)
}

impl AnalyticsHandle {
    async fn close(self) -> Result<(), Error> {
        drop(self.exit_ch);
        self.handle.await?;

        Ok(())
    }

    pub async fn close_with_timeout(self) {
        let _ = tokio::time::timeout(EVENT_TIMEOUT, self.close()).await;
    }
}

struct Worker {
    rx: mpsc::Receiver<AnalyticsEvent>,
    buffer: Vec<AnalyticsEvent>,
    session_id: Uuid,
    api_auth: APIAuth,
    // Used to cancel the worker
    exit_ch: oneshot::Sender<()>,
    client: APIClient,
}

impl Worker {
    pub fn start(mut self) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut timeout = tokio::time::sleep(NO_TIMEOUT);
            loop {
                select! {
                    event = self.rx.recv() => {
                        if let Some(event) = event {
                            self.buffer.push(event);
                        }
                        if self.buffer.len() == BUFFER_THRESHOLD {
                            self.flush();
                            timeout = tokio::time::sleep(NO_TIMEOUT);
                        } else {
                            timeout = tokio::time::sleep(REQUEST_TIMEOUT);
                        }
                    }
                    _ = timeout => {
                        self.flush();
                        timeout = tokio::time::sleep(NO_TIMEOUT);
                    }
                    _ = self.exit_ch.closed() => {
                        self.flush();
                        return;
                    }
                }
            }
        })
    }
    pub fn flush(&mut self) {
        if !self.buffer.is_empty() {
            let events = std::mem::take(&mut self.buffer);
            self.send_events(events);
        }
    }

    fn send_events(&self, mut events: Vec<AnalyticsEvent>) {
        let session_id = self.session_id.clone();
        let client = self.client.clone();
        let api_auth = self.api_auth.clone();
        tokio::spawn(async move {
            add_session_id(session_id, &mut events);
            // We don't log an error for a timeout because
            // that's what the Go code does.
            if let Err(err) =
                tokio::time::timeout(REQUEST_TIMEOUT, client.record_analytics(&api_auth, events))
                    .await
                    // If the request times out, we can panic here
                    // because there's no other work to be done
                    .unwrap()
            {
                debug!("failed to record cache usage analytics. error: {}", err)
            }
        });
    }
}

fn add_session_id(id: Uuid, events: &mut Vec<AnalyticsEvent>) {
    for event in events {
        event.set_session_id(id.to_string());
    }
}
