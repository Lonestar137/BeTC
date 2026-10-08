//! One serial disk writer, with one replaceable pending full-state save. Filesystem durability
//! waits never run on the gameplay thread except when draining the final save at shutdown.

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use benilla_game_core::GameCore;
use bevy::prelude::*;

use crate::local_state;

type SaveBatch = Vec<(PathBuf, String)>;

#[derive(Resource)]
pub(super) struct OfflinePersistence {
    writable: bool,
    worker: Option<SaveWorker>,
}

impl OfflinePersistence {
    pub(super) fn new(writable: bool) -> Self {
        Self {
            writable,
            worker: None,
        }
    }

    pub(super) fn queue(&mut self, core: &GameCore) {
        self.poll();
        if !self.writable {
            return;
        }
        let result = (|| {
            let profile_path = local_state::offline_profile_path()
                .ok_or_else(|| "local state root is unavailable".to_string())?;
            let mut batch = Vec::new();
            for id in core.character_ids() {
                let path = local_state::offline_character_path(id.as_str())
                    .ok_or_else(|| format!("invalid character id {}", id.as_str()))?;
                batch.push((
                    path,
                    core.encode_snapshot(id)
                        .map_err(|error| error.to_string())?,
                ));
            }
            batch.push((
                profile_path,
                core.encode_profile().map_err(|error| error.to_string())?,
            ));
            if self.worker.is_none() {
                self.worker = Some(SaveWorker::spawn(|batch| {
                    for (path, contents) in batch {
                        local_state::write_atomic_bytes_with_backup(&path, contents.as_bytes())
                            .map_err(|error| format!("writing {}: {error}", path.display()))?;
                    }
                    Ok(())
                })?);
            }
            self.worker.as_ref().unwrap().queue(batch)
        })();
        if let Err(error) = result {
            self.disable(error);
        }
    }

    pub(super) fn poll(&mut self) {
        if !self.writable {
            return;
        }
        if let Some(worker) = &self.worker {
            let stopped = worker
                .thread
                .as_ref()
                .is_some_and(|thread| thread.is_finished());
            let error = worker.shared.0.lock().unwrap().error.clone();
            if let Some(error) = error {
                self.disable(error);
            } else if stopped {
                self.disable("offline save worker stopped unexpectedly".into());
            }
        }
    }

    pub(super) fn finish(&mut self) {
        if let Some(worker) = &mut self.worker {
            if let Err(error) = worker.finish() {
                if self.writable {
                    self.disable(error);
                }
            }
        }
        self.writable = false;
    }

    fn disable(&mut self, error: String) {
        self.writable = false;
        error!("offline save failed; persistence disabled: {error}");
    }
}

#[derive(Default)]
struct SaveQueue {
    pending: Option<SaveBatch>,
    closing: bool,
    error: Option<String>,
}

struct SaveWorker {
    shared: Arc<(Mutex<SaveQueue>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl SaveWorker {
    fn spawn(
        mut write: impl FnMut(SaveBatch) -> Result<(), String> + Send + 'static,
    ) -> Result<Self, String> {
        let shared = Arc::new((Mutex::new(SaveQueue::default()), Condvar::new()));
        let state = shared.clone();
        let thread = std::thread::Builder::new()
            .name("offline-save".into())
            .spawn(move || loop {
                let batch = {
                    let (mutex, wake) = &*state;
                    let mut queue = mutex.lock().unwrap();
                    while queue.pending.is_none() && !queue.closing {
                        queue = wake.wait(queue).unwrap();
                    }
                    let Some(batch) = queue.pending.take() else {
                        return;
                    };
                    batch
                };
                // Never hold the mailbox lock across a write, copy, rename or fsync.
                if let Err(error) = write(batch) {
                    let mut queue = state.0.lock().unwrap();
                    queue.error = Some(error);
                    queue.pending = None;
                    return;
                }
            })
            .map_err(|error| format!("starting offline save worker: {error}"))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    fn queue(&self, batch: SaveBatch) -> Result<(), String> {
        let mut queue = self.shared.0.lock().unwrap();
        if let Some(error) = &queue.error {
            return Err(error.clone());
        }
        if queue.closing {
            return Err("offline save worker is closed".into());
        }
        // Every batch includes the whole roster, so a newer pending batch subsumes the old one.
        queue.pending = Some(batch);
        self.shared.1.notify_one();
        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        self.shared.0.lock().unwrap().closing = true;
        self.shared.1.notify_one();
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                return Err("offline save worker panicked".into());
            }
        }
        match self.shared.0.lock().unwrap().error.clone() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for SaveWorker {
    fn drop(&mut self) {
        if self.thread.is_some() {
            if let Err(error) = self.finish() {
                error!("offline save worker shutdown failed: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_secs(5);

    fn batch(contents: &str) -> SaveBatch {
        vec![(PathBuf::from("save.toml"), contents.to_string())]
    }

    #[test]
    fn blocked_writer_does_not_block_submissions_and_drop_drains_the_latest_save() {
        // The release sender must drop before the join-on-drop worker if an assertion fails.
        let worker;
        let (written, received) = crossbeam_channel::unbounded();
        let (release, blocked) = crossbeam_channel::bounded(0);
        worker = SaveWorker::spawn(move |batch| {
            let contents = batch[0].1.clone();
            written.send(contents.clone()).unwrap();
            if contents == "first" {
                blocked
                    .recv_timeout(TIMEOUT)
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })
        .unwrap();
        worker.queue(batch("first")).unwrap();
        assert_eq!(received.recv_timeout(TIMEOUT).unwrap(), "first");
        worker.queue(batch("superseded")).unwrap();
        worker.queue(batch("latest")).unwrap();
        assert!(received.try_recv().is_err());
        release.send_timeout((), TIMEOUT).unwrap();
        drop(worker);
        assert_eq!(received.try_iter().collect::<Vec<_>>(), ["latest"]);
    }

    #[test]
    fn write_failure_discards_pending_work_and_disables_persistence() {
        let worker;
        let (started, received) = crossbeam_channel::bounded(0);
        let (release, blocked) = crossbeam_channel::bounded(0);
        worker = SaveWorker::spawn(move |_| {
            started.send_timeout((), TIMEOUT).unwrap();
            blocked
                .recv_timeout(TIMEOUT)
                .map_err(|error| error.to_string())?;
            Err("disk full".into())
        })
        .unwrap();
        worker.queue(batch("first")).unwrap();
        received.recv_timeout(TIMEOUT).unwrap();
        worker.queue(batch("pending")).unwrap();
        release.send_timeout((), TIMEOUT).unwrap();
        let mut persistence = OfflinePersistence {
            writable: true,
            worker: Some(worker),
        };
        assert_eq!(
            persistence.worker.as_mut().unwrap().finish(),
            Err("disk full".into())
        );
        persistence.poll();
        assert!(!persistence.writable);
        assert!(persistence
            .worker
            .as_ref()
            .unwrap()
            .shared
            .0
            .lock()
            .unwrap()
            .pending
            .is_none());
        assert_eq!(
            persistence.worker.as_ref().unwrap().queue(batch("retry")),
            Err("disk full".into())
        );
    }
}
