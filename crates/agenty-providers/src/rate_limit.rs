//! Simple token-bucket rate limiter for provider requests.
//!
//! Wraps a [`ChatClient`] and ensures at most `requests_per_minute` calls are
//! dispatched in any sliding 60-second window.  When the bucket is empty the
//! caller sleeps until a token becomes available — no requests are dropped.

use std::sync::Arc;

use tokio::sync::{Semaphore, watch};
use tokio::time::{Duration, sleep};

use crate::{
    AgentError, AssistantResponse, ChatClient, ChatMessage, ChatProvider, Config,
    ProviderEventStream, ToolSpec,
};

/// Current state of the rate limiter, observable by the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateLimitStatus {
    /// No request is waiting — bucket has permits.
    Ready,
    /// A request is blocked waiting for a permit to become available.
    Throttled,
}

/// A rate-limited wrapper around [`ChatClient`].
pub struct RateLimitedClient {
    inner: ChatClient,
    semaphore: Arc<Semaphore>,
    refill_interval: Duration,
    /// Sends the current throttle status so the TUI (or any observer) can react.
    status_tx: watch::Sender<RateLimitStatus>,
    status_rx: watch::Receiver<RateLimitStatus>,
}

impl RateLimitedClient {
    /// Wrap `client` with a limit of `requests_per_minute`.
    ///
    /// The limiter starts fully charged (all tokens available).
    pub fn new(client: ChatClient, requests_per_minute: u32) -> Self {
        let rpm = requests_per_minute.max(1) as usize;
        let semaphore = Arc::new(Semaphore::new(rpm));
        let refill_interval = Duration::from_secs(60) / rpm as u32;

        let sem = Arc::clone(&semaphore);
        tokio::spawn(async move {
            loop {
                sleep(refill_interval).await;
                if sem.available_permits() < rpm {
                    sem.add_permits(1);
                }
            }
        });

        let (status_tx, status_rx) = watch::channel(RateLimitStatus::Ready);

        Self {
            inner: client,
            semaphore,
            refill_interval,
            status_tx,
            status_rx,
        }
    }

    /// Subscribe to throttle status changes.
    pub fn status_rx(&self) -> watch::Receiver<RateLimitStatus> {
        self.status_rx.clone()
    }

    /// Access the underlying (unwrapped) client.
    pub fn inner(&self) -> &ChatClient {
        &self.inner
    }

    /// Remaining permits in the bucket right now.
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }

    /// The refill interval (time between adding one token back).
    pub fn refill_interval(&self) -> Duration {
        self.refill_interval
    }

    async fn acquire(&self) -> Result<(), AgentError> {
        // Try without blocking first.
        match self.semaphore.try_acquire() {
            Ok(permit) => {
                permit.forget();
                return Ok(());
            }
            Err(_) => {
                // Bucket is empty — notify observers and wait.
                let _ = self.status_tx.send(RateLimitStatus::Throttled);
            }
        }

        let permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| AgentError::Other("rate limiter closed".into()))?;
        permit.forget();

        let _ = self.status_tx.send(RateLimitStatus::Ready);
        Ok(())
    }
}

impl ChatProvider for RateLimitedClient {
    async fn send_with_tools(
        &self,
        config: &Config,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<AssistantResponse, AgentError> {
        self.acquire().await?;
        self.inner.send_with_tools(config, messages, tools).await
    }

    async fn stream_with_tools(
        &self,
        config: &Config,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<ProviderEventStream, AgentError> {
        self.acquire().await?;
        self.inner.stream_with_tools(config, messages, tools).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn respects_rate_limit() {
        let sem = Arc::new(Semaphore::new(2));
        let refill = Duration::from_millis(500);

        let sem2 = Arc::clone(&sem);
        tokio::spawn(async move {
            loop {
                sleep(refill).await;
                if sem2.available_permits() < 2 {
                    sem2.add_permits(1);
                }
            }
        });

        let p1 = sem.acquire().await.unwrap();
        p1.forget();
        let p2 = sem.acquire().await.unwrap();
        p2.forget();
        assert_eq!(sem.available_permits(), 0);

        let start = tokio::time::Instant::now();
        let p3 = sem.acquire().await.unwrap();
        p3.forget();
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(400),
            "should have waited for refill"
        );
    }
}
