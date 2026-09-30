#![no_main]
use astraforge_core::provider_stream::StreamDecoder;
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    if data.len() > 4_194_305 {
        return;
    }
    let width = data.first().map_or(1, |byte| usize::from(*byte) % 127 + 1);
    let mut decoder = StreamDecoder::default();
    let mut emitted = String::new();
    for chunk in data.chunks(width) {
        if decoder
            .feed(chunk, &mut |text| emitted.push_str(text))
            .is_err()
        {
            return;
        }
        assert!(emitted.len() <= 262_144);
    }
    if let Ok(completion) = decoder.finish() {
        assert_eq!(emitted, completion.content);
        assert!(completion.content.len() <= 262_144);
    }
});
