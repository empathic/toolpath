//! Async Conversation Watcher
//!
//! Provides event-driven file watching for Claude conversation JSONL files.
//! Uses the `notify` crate for filesystem events with a periodic fallback poll.

use crate::error::Result;
use crate::reader::ConversationReader;
use crate::types::ConversationEntry;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

/// Configuration for the async watcher
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// Fallback poll interval (safety net for missed events)
    pub poll_interval: Duration,
    /// Debounce duration for rapid file changes
    pub debounce: Duration,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            debounce: Duration::from_millis(100),
        }
    }
}

/// Async conversation watcher that uses filesystem events
/// with a periodic fallback poll for reliability.
pub struct AsyncConversationWatcher {
    /// Path to the JSONL conversation file
    file_path: PathBuf,
    /// Current byte offset in the file
    byte_offset: Arc<Mutex<u64>>,
    /// Configuration
    config: WatcherConfig,
}

impl AsyncConversationWatcher {
    /// Create a new async watcher for a conversation file.
    ///
    /// # Arguments
    /// * `file_path` - Path to the JSONL conversation file
    /// * `config` - Optional configuration (uses defaults if None)
    pub fn new(file_path: PathBuf, config: Option<WatcherConfig>) -> Self {
        Self {
            file_path,
            byte_offset: Arc::new(Mutex::new(0)),
            config: config.unwrap_or_default(),
        }
    }

    /// Create a watcher starting from a specific byte offset.
    /// Useful for resuming watching after a restart.
    pub fn with_offset(file_path: PathBuf, offset: u64, config: Option<WatcherConfig>) -> Self {
        Self {
            file_path,
            byte_offset: Arc::new(Mutex::new(offset)),
            config: config.unwrap_or_default(),
        }
    }

    /// Get the current byte offset
    pub async fn offset(&self) -> u64 {
        *self.byte_offset.lock().await
    }

    /// Check for new entries since last read (non-blocking poll).
    /// Returns new entries and updates internal offset.
    pub async fn poll(&self) -> Result<Vec<ConversationEntry>> {
        let mut offset = self.byte_offset.lock().await;
        let (entries, new_offset) = ConversationReader::read_from_offset(&self.file_path, *offset)?;
        *offset = new_offset;
        Ok(entries)
    }

    /// Start watching the file and send new entries to the provided channel.
    /// This spawns a background task that:
    /// 1. Watches for filesystem modify events
    /// 2. Polls periodically as a safety fallback
    ///
    /// Returns a handle that can be used to stop the watcher.
    pub async fn start(self, tx: mpsc::Sender<Vec<ConversationEntry>>) -> Result<WatcherHandle> {
        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        let file_path = self.file_path.clone();
        let byte_offset = self.byte_offset.clone();
        let poll_interval = self.config.poll_interval;
        let debounce = self.config.debounce;

        // Channel for filesystem events
        let (event_tx, mut event_rx) = mpsc::channel::<()>(16);

        // Set up the filesystem watcher
        let event_tx_clone = event_tx.clone();
        let file_path_clone = file_path.clone();

        // Create the watcher in a blocking context since notify isn't async
        let watcher_result: std::result::Result<RecommendedWatcher, notify::Error> =
            notify::recommended_watcher(move |res: std::result::Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    // Only trigger on modify events for our file
                    if event.kind.is_modify() {
                        for path in &event.paths {
                            if path == &file_path_clone {
                                let _ = event_tx_clone.blocking_send(());
                                break;
                            }
                        }
                    }
                }
            });

        // Watch the parent directory (notify works better with directories)
        let mut watcher = match watcher_result {
            Ok(mut w) => {
                if let Some(parent) = file_path.parent() {
                    let _ = w.watch(parent, RecursiveMode::NonRecursive);
                }
                Some(w)
            }
            Err(e) => {
                eprintln!(
                    "Warning: Failed to create file watcher: {}. Using poll-only mode.",
                    e
                );
                None
            }
        };

        // Spawn the main watch loop
        let handle = tokio::spawn(async move {
            let mut poll_timer = tokio::time::interval(poll_interval);
            let mut last_event = std::time::Instant::now();

            loop {
                tokio::select! {
                    // Stop signal
                    _ = stop_rx.recv() => {
                        break;
                    }

                    // Filesystem event (debounced)
                    Some(()) = event_rx.recv() => {
                        let now = std::time::Instant::now();
                        if now.duration_since(last_event) >= debounce {
                            last_event = now;
                            if let Ok(entries) = read_new_entries(&file_path, &byte_offset).await
                                && !entries.is_empty() && tx.send(entries).await.is_err()
                            {
                                break; // Receiver dropped
                            }
                        }
                    }

                    // Periodic fallback poll
                    _ = poll_timer.tick() => {
                        if let Ok(entries) = read_new_entries(&file_path, &byte_offset).await
                            && !entries.is_empty() && tx.send(entries).await.is_err()
                        {
                            break; // Receiver dropped
                        }
                    }
                }
            }

            // Clean up watcher
            drop(watcher.take());
        });

        Ok(WatcherHandle {
            stop_tx,
            _task: handle,
        })
    }
}

/// Read new entries from offset and update the offset
async fn read_new_entries(
    file_path: &PathBuf,
    byte_offset: &Arc<Mutex<u64>>,
) -> Result<Vec<ConversationEntry>> {
    let mut offset = byte_offset.lock().await;
    let (entries, new_offset) = ConversationReader::read_from_offset(file_path, *offset)?;
    *offset = new_offset;
    Ok(entries)
}

/// Handle to control a running watcher
pub struct WatcherHandle {
    stop_tx: mpsc::Sender<()>,
    _task: tokio::task::JoinHandle<()>,
}

impl WatcherHandle {
    /// Stop the watcher
    pub async fn stop(self) {
        let _ = self.stop_tx.send(()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use tokio::time::timeout;

    #[tokio::test]
    async fn test_poll_basic() {
        let mut temp = NamedTempFile::new().unwrap();
        writeln!(
            temp,
            r#"{{"type":"user","uuid":"123","timestamp":"2024-01-01T00:00:00Z","sessionId":"test","message":{{"role":"user","content":"Hello"}}}}"#
        )
        .unwrap();
        temp.flush().unwrap();

        let watcher = AsyncConversationWatcher::new(temp.path().to_path_buf(), None);

        // First poll should get the entry
        let entries = watcher.poll().await.unwrap();
        assert_eq!(entries.len(), 1);

        // Second poll should get nothing
        let entries = watcher.poll().await.unwrap();
        assert!(entries.is_empty());

        // Add another entry
        writeln!(
            temp,
            r#"{{"type":"assistant","uuid":"456","timestamp":"2024-01-01T00:00:01Z","sessionId":"test","message":{{"role":"assistant","content":"Hi"}}}}"#
        )
        .unwrap();
        temp.flush().unwrap();

        // Third poll should get the new entry
        let entries = watcher.poll().await.unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn test_watcher_start_and_stop() {
        let mut temp = NamedTempFile::new().unwrap();
        writeln!(
            temp,
            r#"{{"type":"user","uuid":"123","timestamp":"2024-01-01T00:00:00Z","sessionId":"test","message":{{"role":"user","content":"Hello"}}}}"#
        )
        .unwrap();
        temp.flush().unwrap();

        let config = WatcherConfig {
            poll_interval: Duration::from_millis(50),
            debounce: Duration::from_millis(10),
        };

        let watcher = AsyncConversationWatcher::new(temp.path().to_path_buf(), Some(config));
        let (tx, mut rx) = mpsc::channel(16);

        let handle = watcher.start(tx).await.unwrap();

        // Should receive initial entries from first poll
        let entries = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");
        assert_eq!(entries.len(), 1);

        // Stop the watcher
        handle.stop().await;
    }

    #[tokio::test]
    async fn test_offset_persistence() {
        let mut temp = NamedTempFile::new().unwrap();
        writeln!(
            temp,
            r#"{{"type":"user","uuid":"123","timestamp":"2024-01-01T00:00:00Z","sessionId":"test","message":{{"role":"user","content":"Hello"}}}}"#
        )
        .unwrap();
        temp.flush().unwrap();

        // Read once to get offset
        let watcher1 = AsyncConversationWatcher::new(temp.path().to_path_buf(), None);
        let _ = watcher1.poll().await.unwrap();
        let offset = watcher1.offset().await;
        assert!(offset > 0);

        // Add more content
        writeln!(
            temp,
            r#"{{"type":"assistant","uuid":"456","timestamp":"2024-01-01T00:00:01Z","sessionId":"test","message":{{"role":"assistant","content":"Hi"}}}}"#
        )
        .unwrap();
        temp.flush().unwrap();

        // Create new watcher starting from saved offset
        let watcher2 =
            AsyncConversationWatcher::with_offset(temp.path().to_path_buf(), offset, None);

        // Should only get the new entry
        let entries = watcher2.poll().await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].uuid, "456");
    }
}
