use astraforge_core::provider::{
    ChatMessage, ChatProvider, CompatibleProvider, EmbeddingProvider, ProviderConfig,
};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
fn server(body: String) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let count = stream.read(&mut chunk).unwrap();
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
            assert!(bytes.len() < 131072 && count > 0);
        }
        let response=format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body);
        stream.write_all(response.as_bytes()).unwrap();
        String::from_utf8(bytes).unwrap()
    });
    (address, handle)
}
fn config(endpoint: String) -> ProviderConfig {
    ProviderConfig {
        endpoint,
        model: "protocol-fixture".into(),
        api_key_env: "ASTRAFORGE_UNUSED_TEST_KEY".into(),
        embedding_model: "embedding-fixture".into(),
    }
}
#[test]
fn actual_http_stream_decodes_tokens_and_usage() {
    let (endpoint,server)=server("data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\" world\"}}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n".into());
    let provider = CompatibleProvider::new(config(endpoint)).unwrap();
    let mut streamed = String::new();
    let completion = provider
        .stream_chat(
            &[ChatMessage {
                role: "user".into(),
                content: "request".into(),
            }],
            &AtomicBool::new(false),
            &mut |chunk| streamed.push_str(chunk),
        )
        .unwrap();
    assert_eq!(streamed, "hello world");
    assert_eq!(completion.content, streamed);
    assert_eq!(completion.usage.input_tokens, Some(8));
    let request = server.join().unwrap();
    assert!(request.starts_with("POST /v1/chat/completions"));
    assert!(request.contains("\"stream\":true"));
}
#[test]
fn embedding_protocol_reorders_and_validates_actual_http_response() {
    let (endpoint, server) = server(
        "{\"data\":[{\"index\":1,\"embedding\":[0,1]},{\"index\":0,\"embedding\":[1,0]}]}".into(),
    );
    let provider = CompatibleProvider::new(config(endpoint)).unwrap();
    assert_eq!(
        provider
            .embed(&["a".into(), "b".into()], &AtomicBool::new(false))
            .unwrap(),
        vec![vec![1.0, 0.0], vec![0.0, 1.0]]
    );
    assert!(server.join().unwrap().starts_with("POST /v1/embeddings"));
}
#[test]
fn cancelled_provider_request_never_connects() {
    let provider = CompatibleProvider::new(config("http://127.0.0.1:1/v1".into())).unwrap();
    assert!(provider
        .stream_chat(&[], &AtomicBool::new(true), &mut |_| {})
        .is_err());
}
