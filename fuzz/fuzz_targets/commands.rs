#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    if let Ok(mut command) = serde_json::from_slice::<astraforge_core::process::CommandSpec>(data) {
        let first = astraforge_core::process::classify(&command);
        command.approved = !command.approved;
        let second = astraforge_core::process::classify(&command);
        assert_eq!(first.level, second.level);
        assert!(["SAFE", "CAUTION", "DANGEROUS", "BLOCKED"].contains(&first.level.as_str()));
    }
});
