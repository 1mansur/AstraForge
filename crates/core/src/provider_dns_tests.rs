use super::*;
use std::net::SocketAddr;
use std::sync::{mpsc, Condvar, Mutex};
use std::time::Instant;
static TEST_LOCK: Mutex<()> = Mutex::new(());
struct ReleaseGate {
    state: Arc<(Mutex<bool>, Condvar)>,
}
impl ReleaseGate {
    fn new() -> Self {
        Self {
            state: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }
    fn release(&self) {
        let (lock, condition) = &*self.state;
        *lock.lock().expect("gate lock") = true;
        condition.notify_all();
    }
}
impl Drop for ReleaseGate {
    fn drop(&mut self) {
        self.release();
    }
}
fn addresses() -> Addrs {
    Box::new([SocketAddr::from(([127, 0, 0, 1], 443))].into_iter())
}
#[test]
fn provider_runtime_and_resolver_are_shared_and_bounded() {
    let _test = TEST_LOCK.lock().expect("test lock");
    let first = runtime().expect("transport runtime");
    assert!(std::ptr::eq(first, runtime().expect("same runtime")));
    assert_eq!(first.metrics().num_workers(), 2);
    assert!(Arc::ptr_eq(&dns_resolver(), &dns_resolver()));
}
#[test]
fn dropping_unpolled_resolution_releases_its_slot_without_starting_work() {
    let _test = TEST_LOCK.lock().expect("test lock");
    let resolver = BoundedResolver::default();
    let invoked = Arc::new(AtomicUsize::new(0));
    let pending: Vec<_> = (0..4)
        .map(|_| {
            let invoked = invoked.clone();
            resolver.resolve_with(move || {
                invoked.fetch_add(1, Ordering::SeqCst);
                Ok(addresses())
            })
        })
        .collect();
    assert_eq!(resolver.active.load(Ordering::Acquire), 4);
    let refused = resolver.resolve_with(|| Ok(addresses()));
    assert!(runtime().expect("runtime").block_on(refused).is_err());
    drop(pending);
    assert_eq!(resolver.active.load(Ordering::Acquire), 0);
    assert_eq!(invoked.load(Ordering::Acquire), 0);
}
#[test]
fn completed_and_failed_lookups_release_reservations() {
    let _test = TEST_LOCK.lock().expect("test lock");
    let resolver = BoundedResolver::default();
    let runtime = runtime().expect("runtime");
    let result = runtime
        .block_on(resolver.resolve_with(|| Ok(addresses())))
        .expect("controlled resolution");
    assert_eq!(result.count(), 1);
    assert_eq!(resolver.active.load(Ordering::Acquire), 0);
    let error = runtime.block_on(resolver.resolve_with(|| {
        Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "Controlled resolver failure",
        ))
    }));
    assert!(error.is_err());
    assert_eq!(resolver.active.load(Ordering::Acquire), 0);
}
#[test]
fn cancelled_waiters_keep_slots_until_blocking_resolution_really_finishes() {
    let _test = TEST_LOCK.lock().expect("test lock");
    let runtime = runtime().expect("runtime");
    let resolver = BoundedResolver::default();
    let gate = ReleaseGate::new();
    let (started, ready) = mpsc::channel();
    let mut waiters = Vec::new();
    for _ in 0..4 {
        let started = started.clone();
        let state = gate.state.clone();
        waiters.push(runtime.spawn(resolver.resolve_with(move || {
            started.send(()).expect("signal started");
            let (lock, condition) = &*state;
            let mut released = lock.lock().expect("gate lock");
            while !*released {
                released = condition.wait(released).expect("gate wait");
            }
            Ok(addresses())
        })));
    }
    for _ in 0..4 {
        ready
            .recv_timeout(Duration::from_secs(5))
            .expect("blocking lookup started");
    }
    for waiter in &waiters {
        waiter.abort();
    }
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            for waiter in waiters {
                assert!(waiter.await.is_err_and(|error| error.is_cancelled()));
            }
        })
        .await
        .expect("cancelled callers return while resolver gate remains closed");
    });
    assert!(!*gate.state.0.lock().expect("gate lock"));
    assert_eq!(resolver.active.load(Ordering::Acquire), 4);
    let invoked = Arc::new(AtomicUsize::new(0));
    for _ in 0..1000 {
        let invoked = invoked.clone();
        let refused = resolver.resolve_with(move || {
            invoked.fetch_add(1, Ordering::SeqCst);
            Ok(addresses())
        });
        assert!(runtime.block_on(refused).is_err());
    }
    assert_eq!(invoked.load(Ordering::Acquire), 0);
    assert_eq!(resolver.active.load(Ordering::Acquire), 4);
    gate.release();
    let deadline = Instant::now() + Duration::from_secs(5);
    while resolver.active.load(Ordering::Acquire) != 0 {
        assert!(Instant::now() < deadline, "finished lookups release slots");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(runtime
        .block_on(resolver.resolve_with(|| Ok(addresses())))
        .is_ok());
    assert_eq!(resolver.active.load(Ordering::Acquire), 0);
}
