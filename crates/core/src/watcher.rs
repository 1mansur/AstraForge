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
}
impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}
pub fn watch(
    root: &Path,
    callback: Arc<dyn Fn(Vec<String>, bool) + Send + Sync>,
) -> Result<WatchHandle> {
    let (sender, receiver) = mpsc::sync_channel::<std::result::Result<Event, notify::Error>>(1024);
    let overflow = Arc::new(AtomicBool::new(false));
    let flag = overflow.clone();
    let mut watcher = notify::recommended_watcher(move |event| {
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
    std::thread::spawn(move || {
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
                        if matches!(event.kind, notify::EventKind::Access(_)) {
                            continue;
                        }
                        for path in event.paths {
                            if let Ok(path) = path.strip_prefix(&root) {
                                let relative = path.to_string_lossy().replace('\\', "/");
                                if !relative.split('/').any(|part| {
                                    matches!(
                                        part,
                                        ".git"
                                            | "node_modules"
                                            | "target"
                                            | "dist"
                                            | ".venv"
                                            | "__pycache__"
                                    )
                                }) && !relative.contains(".astraforge-")
                                {
                                    paths.insert(relative);
                                }
                            }
                        }
                    }
                    Err(_) => full = true,
                }
            }
            if full || !paths.is_empty() {
                callback(paths.into_iter().collect(), full);
            }
        }
    });
    Ok(WatchHandle {
        _watcher: watcher,
        stop,
    })
}
