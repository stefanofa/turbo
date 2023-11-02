#![deny(clippy::all)]

use std::time::Duration;

use futures::stream::FuturesUnordered;
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
pub type AnalyticsSender = mpsc::UnboundedSender<AnalyticsEvent>;

pub struct AnalyticsHandle {
    exit_ch: oneshot::Receiver<()>,
    handle: JoinHandle<()>,
}

pub fn start_analytics(api_auth: APIAuth, client: APIClient) -> (AnalyticsSender, AnalyticsHandle) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let session_id = Uuid::new_v4();
    let worker = Worker {
        rx,
        buffer: Vec::new(),
        session_id,
        api_auth,
        senders: FuturesUnordered::new(),
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
        if let Err(err) = tokio::time::timeout(EVENT_TIMEOUT, self.close()).await {
            debug!("failed to close analytics handle. error: {}", err)
        }
    }
}

struct Worker {
    rx: mpsc::UnboundedReceiver<AnalyticsEvent>,
    buffer: Vec<AnalyticsEvent>,
    session_id: Uuid,
    api_auth: APIAuth,
    senders: FuturesUnordered<JoinHandle<()>>,
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
                    // We want the events to be prioritized over closing
                    biased;
                    event = self.rx.recv() => {
                        if let Some(event) = event {
                            self.buffer.push(event);
                        }
                        if self.buffer.len() == BUFFER_THRESHOLD {
                            self.flush_events();
                            timeout = tokio::time::sleep(NO_TIMEOUT);
                        } else {
                            timeout = tokio::time::sleep(REQUEST_TIMEOUT);
                        }
                    }
                    _ = timeout => {
                        self.flush_events();
                        timeout = tokio::time::sleep(NO_TIMEOUT);
                    }
                    _ = self.exit_ch.closed() => {
                        self.flush_events();
                        for handle in self.senders {
                            if let Err(err) = handle.await {
                                debug!("failed to send analytics event. error: {}", err)
                            }
                        }
                        return;
                    }
                }
            }
        })
    }

    pub fn flush_events(&mut self) {
        if !self.buffer.is_empty() {
            let events = std::mem::take(&mut self.buffer);
            let handle = self.send_events(events);
            self.senders.push(handle);
        }
    }

    fn send_events(&self, mut events: Vec<AnalyticsEvent>) -> JoinHandle<()> {
        let session_id = self.session_id;
        let client = self.client.clone();
        let api_auth = self.api_auth.clone();
        add_session_id(session_id, &mut events);

        tokio::spawn(async move {
            // We don't log an error for a timeout because
            // that's what the Go code does.
            if let Ok(Err(err)) =
                tokio::time::timeout(REQUEST_TIMEOUT, client.record_analytics(&api_auth, events))
                    .await
            {
                debug!("failed to record cache usage analytics. error: {}", err)
            }
        })
    }
}

fn add_session_id(id: Uuid, events: &mut Vec<AnalyticsEvent>) {
    for event in events {
        event.set_session_id(id.to_string());
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;
    use turborepo_vercel_api::AnalyticsEvent;

    #[test_case(
      AnalyticsEvent {
        session_id: Some("session-id".to_string()),
        source: turborepo_vercel_api::CacheSource::Local,
        event: turborepo_vercel_api::CacheEvent::Hit,
        hash: "this-is-my-hash".to_string(),
        duration: 58,
      }
    )]
    #[test_case(
      AnalyticsEvent {
        session_id: Some("session-id".to_string()),
        source: turborepo_vercel_api::CacheSource::Remote,
        event: turborepo_vercel_api::CacheEvent::Miss,
        hash: "this-is-my-hash-2".to_string(),
        duration: 21,
      }
    )]
    #[test_case(
        AnalyticsEvent {
          session_id: None,
          source: turborepo_vercel_api::CacheSource::Remote,
          event: turborepo_vercel_api::CacheEvent::Miss,
          hash: "this-is-my-hash-2".to_string(),
          duration: 21,
        }
    )]
    fn test_serialize_analytics_event(event: AnalyticsEvent) {
        let json = serde_json::to_string(&event).unwrap();
        insta::assert_json_snapshot!(json);
    }
}
