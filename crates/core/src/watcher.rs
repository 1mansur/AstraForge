use crate::error::{AppError, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
pub struct WatchHandle {
    _watcher: RecommendedWatcher,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            if worker.thread().id() != std::thread::current().id() && worker.join().is_err() {
                eprintln!("Filesystem watcher worker panicked during shutdown");
            }
        }
    }
}
pub type WatchCallback = dyn Fn(Vec<String>, bool, &AtomicBool) + Send + Sync;
pub fn watch(root: &Path, callback: Arc<WatchCallback>) -> Result<WatchHandle> {
    let (sender, receiver) = mpsc::sync_channel::<std::result::Result<Event, notify::Error>>(1024);
    let overflow = Arc::new(AtomicBool::new(false));
    let flag = overflow.clone();
    let watched_root = root.to_path_buf();
    let mut watcher =
        notify::recommended_watcher(move |event: std::result::Result<Event, notify::Error>| {
            let event = match event {
                Ok(mut event) => {
                    let rescan = event.need_rescan();
                    if !rescan && matches!(event.kind, notify::EventKind::Access(_)) {
                        return;
                    }
                    let directory_metadata = matches!(
                        event.kind,
                        notify::EventKind::Modify(
                            notify::event::ModifyKind::Any
                                | notify::event::ModifyKind::Data(_)
                                | notify::event::ModifyKind::Metadata(_)
                        )
                    );
                    event.paths.retain(|path| {
                        if directory_metadata && path.is_dir() {
                            return false;
                        }
                        path.strip_prefix(&watched_root).is_ok_and(|relative| {
                            !crate::index::excluded_path(
                                &relative.to_string_lossy().replace('\\', "/"),
                            )
                        })
                    });
                    if event.paths.is_empty() && !rescan {
                        return;
                    }
                    Ok(event)
                }
                Err(error) => Err(error),
            };
            if sender.try_send(event).is_err() {
                flag.store(true, Ordering::Relaxed);
            }
        })
        .map_err(|_| {
            AppError::new(
                "watcher_unavailable",
                "Could not create a filesystem watcher",
            )
        })?;
    watcher
        .watch(root, RecursiveMode::Recursive)
        .map_err(|_| AppError::new("watcher_unavailable", "Could not watch this repository"))?;
    let root = root.to_path_buf();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let worker = std::thread::Builder::new()
        .name("astraforge-watcher".to_owned())
        .spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let first = match receiver.recv_timeout(Duration::from_millis(250)) {
                    Ok(event) => event,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                let start = Instant::now();
                let mut paths = BTreeSet::new();
                let mut full = overflow.swap(false, Ordering::Relaxed);
                let mut events = vec![first];
                while start.elapsed() < Duration::from_millis(120) && events.len() < 1024 {
                    match receiver.recv_timeout(Duration::from_millis(20)) {
                        Ok(event) => events.push(event),
                        Err(_) => break,
                    }
                }
                for event in events {
                    match event {
                        Ok(event) => {
                            full |= event.need_rescan();
                            if matches!(event.kind, notify::EventKind::Access(_)) {
                                continue;
                            }
                            for path in event.paths {
                                if let Ok(path) = path.strip_prefix(&root) {
                                    let relative = path.to_string_lossy().replace('\\', "/");
                                    if !crate::index::excluded_path(&relative) {
                                        paths.insert(relative);
                                    }
                                }
                            }
                        }
                        Err(_) => full = true,
                    }
                }
                if !stopped.load(Ordering::Acquire) && (full || !paths.is_empty()) {
                    callback(paths.into_iter().collect(), full, &stopped);
                }
            }
        })
        .map_err(|error| {
            AppError::new(
                "watcher_unavailable",
                format!("Could not start filesystem watcher worker: {error}"),
            )
        })?;
    Ok(WatchHandle {
        _watcher: watcher,
        stop,
        worker: Some(worker),
    })
}
