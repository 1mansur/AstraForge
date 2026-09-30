use astraforge_core::provider::{
    ChatProvider, CompatibleProvider, EmbeddingProvider, ProviderConfig,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;
fn read_request(stream: &mut TcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut request = Vec::new();
    let mut buffer = [0u8; 2048];
    loop {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0 && request.len() < 262144);
        request.extend_from_slice(&buffer[..count]);
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]);
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if request.len() >= end + 4 + length {
                return;
            }
        }
    }
}
fn provider(endpoint: String) -> CompatibleProvider {
    CompatibleProvider::new(ProviderConfig {
        endpoint,
        model: "adversarial-fixture".into(),
        api_key_env: "ASTRAFORGE_ADVERSARIAL_UNUSED_KEY".into(),
        embedding_model: "fixture-vectors".into(),
    })
    .unwrap()
}
fn response(status: u16, body: Vec<u8>) -> (CompatibleProvider, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        let header = format!("HTTP/1.1 {status} Fixture\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        stream.write_all(header.as_bytes()).unwrap();
        match stream.write_all(&body) {
            Ok(()) => (),
            Err(error) => assert!(matches!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
            )),
        }
    });
    (provider(endpoint), server)
}
#[test]
fn incomplete_stream_cannot_complete_an_agent_action() {
    let body = b"data: {\"choices\":[{\"delta\":{\"content\":\"{\\\"kind\\\":\\\"finish\\\",\\\"summary\\\":\\\"unverified\\\"}\"}}]}\n\n".to_vec();
    let (provider, server) = response(200, body);
    let result = provider.stream_chat(&[], &AtomicBool::new(false), &mut |_| {});
    server.join().unwrap();
    assert!(
        result.is_err(),
        "EOF without a completion marker must not authorize a completed action"
    );
}
#[test]
fn duplicated_sse_event_ids_do_not_duplicate_generated_content() {
    let frame = "id: token-1\ndata: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n";
    let (provider, server) = response(200, format!("{frame}{frame}data: [DONE]\n\n").into_bytes());
    let completion = provider
        .stream_chat(&[], &AtomicBool::new(false), &mut |_| {})
        .unwrap();
    server.join().unwrap();
    assert_eq!(completion.content, "hello");
}
#[test]
fn length_truncation_is_not_success_even_with_done_marker() {
    let body = b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n".to_vec();
    let (provider, server) = response(200, body);
    let result = provider.stream_chat(&[], &AtomicBool::new(false), &mut |_| {});
    server.join().unwrap();
    assert!(result.is_err());
}
#[test]
fn cancelled_stalled_stream_releases_transport_promptly() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let (ready_sender, ready_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 1000\r\n\r\n").unwrap();
        ready_sender.send(()).unwrap();
        release_receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
    });
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let (completed_sender, completed_receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        completed_sender
            .send(
                provider(endpoint)
                    .stream_chat(&[], &worker_cancel, &mut |_| {})
                    .map(|completion| completion.content),
            )
            .unwrap();
    });
    ready_receiver
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    thread::sleep(Duration::from_millis(100));
    cancel.store(true, Ordering::SeqCst);
    let completed = completed_receiver.recv_timeout(Duration::from_secs(1));
    release_sender.send(()).unwrap();
    server.join().unwrap();
    worker.join().unwrap();
    assert!(
        completed.is_ok(),
        "Cancellation waited for the stalled socket instead of releasing it"
    );
    assert_eq!(completed.unwrap().unwrap_err().code, "cancelled");
}
#[test]
fn http_failures_and_malformed_frames_are_bounded_and_redacted() {
    for status in [400, 401, 403, 429, 500] {
        let (provider, server) =
            response(status, b"PRIVATE_PROVIDER_BODY_MUST_NOT_APPEAR".to_vec());
        let error = provider
            .stream_chat(&[], &AtomicBool::new(false), &mut |_| {})
            .err()
            .unwrap();
        server.join().unwrap();
        assert_eq!(error.code, "provider_http");
        assert!(!error.to_string().contains("PRIVATE_PROVIDER_BODY"));
    }
    for body in [
        b"data: {invalid}\n\n".to_vec(),
        vec![0xff, b'\n'],
        format!("data: {}\n\n", "x".repeat(262145)).into_bytes(),
    ] {
        let (provider, server) = response(200, body);
        let result = provider.stream_chat(&[], &AtomicBool::new(false), &mut |_| {});
        server.join().unwrap();
        assert!(result.is_err());
    }
}
#[test]
fn embedding_batch_rejects_mixed_dimensions() {
    let (provider, server) = response(
        200,
        br#"{"data":[{"index":0,"embedding":[1,0]},{"index":1,"embedding":[1,0,0]}]}"#.to_vec(),
    );
    let result = provider.embed(&["one".into(), "two".into()], &AtomicBool::new(false));
    server.join().unwrap();
    assert!(result.is_err());
}
#[test]
fn stalled_response_headers_are_cancellable_and_connection_failure_is_redacted() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let (ready_sender, ready_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        ready_sender.send(()).unwrap();
        release_receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
    });
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let (completed_sender, completed_receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        completed_sender
            .send(provider(endpoint).embed(&["fixture".into()], &worker_cancel))
            .unwrap();
    });
    ready_receiver
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    cancel.store(true, Ordering::SeqCst);
    let completed = completed_receiver.recv_timeout(Duration::from_secs(1));
    release_sender.send(()).unwrap();
    server.join().unwrap();
    worker.join().unwrap();
    assert_eq!(completed.unwrap().unwrap_err().code, "cancelled");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    drop(listener);
    let error = provider(endpoint)
        .embed(&["PRIVATE_INPUT".into()], &AtomicBool::new(false))
        .unwrap_err();
    assert_eq!(error.code, "provider_network");
    assert!(!error.to_string().contains("PRIVATE_INPUT"));
}
