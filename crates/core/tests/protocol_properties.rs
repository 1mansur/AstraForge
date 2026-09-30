use astraforge_core::provider_stream::StreamDecoder;
use astraforge_core::service::{Engine, RequestEnvelope};
use serde_json::{json, Value};
use std::sync::Arc;
fn next(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    *seed
}
fn decode(bytes: &[u8], width: usize) -> Result<(String, Option<u64>, Option<u64>), String> {
    let mut decoder = StreamDecoder::default();
    let mut emitted = String::new();
    for chunk in bytes.chunks(width.max(1)) {
        decoder
            .feed(chunk, &mut |text| emitted.push_str(text))
            .map_err(|error| error.code)?;
    }
    let completion = decoder.finish().map_err(|error| error.code)?;
    assert_eq!(emitted, completion.content);
    assert!(completion.content.len() <= 262_144);
    Ok((
        completion.content,
        completion.usage.input_tokens,
        completion.usage.output_tokens,
    ))
}
fn frame(id: u64, text: &str) -> String {
    format!(
        "id: {id}\ndata: {}\n\n",
        json!({"choices":[{"delta":{"content":text},"finish_reason":null}]})
    )
}
#[test]
fn fragmented_utf8_streams_preserve_completion_and_usage() {
    let mut seed = 0x41f2_a619_u64;
    let alphabet = ['a', '\n', '"', '\\', '雪', 'я', '🚀', '\0', ' '];
    for case in 0..1000 {
        let text: String = (0..(next(&mut seed) % 40 + 1))
            .map(|_| alphabet[next(&mut seed) as usize % alphabet.len()])
            .collect();
        let first = frame(case, &text);
        let stop = json!({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":case,"completion_tokens":text.len()}});
        let stream = format!("{first}{first}data: {stop}\n\ndata: [DONE]\n\n");
        let expected = (text.clone(), Some(case), Some(text.len() as u64));
        assert_eq!(decode(stream.as_bytes(), 1), Ok(expected.clone()));
        assert_eq!(
            decode(stream.as_bytes(), (next(&mut seed) % 79 + 1) as usize),
            Ok(expected)
        );
    }
    println!("seed=0x41f2a619 generated_valid_utf8_cases=1000 chunkings_per_case=2");
}
#[test]
fn reused_event_identifiers_cannot_replace_already_emitted_content() {
    let mut seed = 0x227d_d64b_u64;
    for case in 0..500 {
        let id = next(&mut seed);
        let first = format!("original-{case}-雪");
        let stream = format!(
            "{}{}data: [DONE]\n\n",
            frame(id, &first),
            frame(id, "changed")
        );
        let mut decoder = StreamDecoder::default();
        let mut emitted = String::new();
        let mut failure = None;
        for chunk in stream
            .as_bytes()
            .chunks((next(&mut seed) % 47 + 1) as usize)
        {
            if let Err(error) = decoder.feed(chunk, &mut |text| emitted.push_str(text)) {
                failure = Some(error.code);
                break;
            }
        }
        assert_eq!(failure.as_deref(), Some("provider_protocol"));
        assert_eq!(emitted, first);
    }
    println!("seed=0x227dd64b generated_conflicting_identifier_cases=500");
}
#[test]
fn bounded_arbitrary_bytes_fail_consistently_across_chunk_boundaries() {
    let mut seed = 0x852d_77c1_u64;
    for case in 0..2000 {
        let length = (next(&mut seed) % 2049) as usize;
        let mut bytes: Vec<u8> = (0..length).map(|_| (next(&mut seed) >> 32) as u8).collect();
        if case % 3 == 0 {
            bytes.splice(0..0, b"data: ".iter().copied());
            bytes.extend_from_slice(b"\n\n");
        }
        let whole = decode(&bytes, bytes.len().max(1));
        let fragmented = decode(&bytes, (next(&mut seed) % 41 + 1) as usize);
        assert_eq!(whole, fragmented, "seeded case {case}");
        let _envelope = serde_json::from_slice::<RequestEnvelope>(&bytes);
    }
    println!("seed=0x852d77c1 generated_malformed_byte_cases=2000 max_random_bytes=2048");
}
#[test]
fn generated_invalid_envelopes_never_reach_repository_dispatch() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let engine = Arc::new(
        Engine::new(&temporary.path().join("state.sqlite"), Arc::new(|_, _| {}))
            .expect("real engine"),
    );
    let mut seed = 0x4cf1_e25b_u64;
    for case in 0..1000 {
        let path = format!("file-{}.ts", next(&mut seed));
        let value = match case % 4 {
            0 => {
                json!({"version":next(&mut seed)%65536+2,"repositoryId":null,"request":{"method":"repositories","params":{}}})
            }
            1 => {
                json!({"version":1,"repositoryId":null,"request":{"method":"read_file","params":{"path":path}}})
            }
            2 => {
                json!({"version":1,"repositoryId":format!("unknown-{case}"),"request":{"method":"read_file","params":{"path":path}}})
            }
            _ => {
                json!({"version":1,"repositoryId":null,"request":{"method":"read_file","params":{"path":path,"approved":true}}})
            }
        };
        let envelope = serde_json::from_value::<RequestEnvelope>(value);
        if case % 4 == 3 {
            assert!(envelope.is_err());
            continue;
        }
        let error = engine
            .handle_envelope(envelope.expect("syntactically valid envelope"))
            .expect_err("reject before dispatch");
        assert_eq!(
            error.code,
            match case % 4 {
                0 => "protocol_version",
                1 => "protocol_repository",
                _ => "stale_repository",
            }
        );
    }
    assert!(engine.workspace().is_err());
    println!("seed=0x4cf1e25b generated_rejected_envelope_cases=1000");
}
#[test]
fn shared_desktop_envelope_fixtures_match_rust_boundary() {
    let fixtures: Value = serde_json::from_str(include_str!("../../../fixtures/protocol-v1.json"))
        .expect("protocol fixture");
    for name in ["scopedRequest", "globalRequest"] {
        let envelope: RequestEnvelope =
            serde_json::from_value(fixtures[name].clone()).expect("native request envelope");
        assert_eq!(envelope.version, 1);
    }
    assert_eq!(fixtures["workspaceEvent"]["version"], 1);
    assert_eq!(fixtures["diagnosticEvent"]["sequence"], 2);
}
