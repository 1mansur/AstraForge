#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    if data.len() <= 262144 {
        let _request = serde_json::from_slice::<astraforge_core::service::Request>(data);
        let _envelope = serde_json::from_slice::<astraforge_core::service::RequestEnvelope>(data);
        let _tool = serde_json::from_slice::<astraforge_core::agent::ToolCall>(data);
        let _patch = serde_json::from_slice::<astraforge_core::patch::PatchProposal>(data);
        let _session = serde_json::from_slice::<astraforge_core::agent::AgentSession>(data);
    }
});
