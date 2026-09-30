use crate::error::{AppError, Result};
use crate::provider::{Completion, Usage};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
#[derive(Default)]
pub struct StreamDecoder {
    line: Vec<u8>,
    data: String,
    id: Option<String>,
    seen: HashMap<String, [u8; 32]>,
    content: String,
    usage: Usage,
    wire_bytes: usize,
    stopped: bool,
    done: bool,
}
impl StreamDecoder {
    pub fn feed(&mut self, bytes: &[u8], on_delta: &mut dyn FnMut(&str)) -> Result<()> {
        self.wire_bytes = self.wire_bytes.saturating_add(bytes.len());
        if self.wire_bytes > 4_194_304 {
            return Err(AppError::new(
                "provider_limit",
                "AI stream exceeds the size limit",
            ));
        }
        for &byte in bytes {
            if self.done {
                break;
            }
            if byte == b'\n' {
                let line = std::mem::take(&mut self.line);
                let line = std::str::from_utf8(&line)
                    .map_err(|_| protocol("AI response is not UTF-8"))?
                    .trim_end_matches('\r');
                if line.is_empty() {
                    self.event(on_delta)?;
                } else if let Some(value) = line.strip_prefix("data:") {
                    let value = value.strip_prefix(' ').unwrap_or(value);
                    if self.data.len() + value.len() + 1 > 262_144 {
                        return Err(AppError::new(
                            "provider_limit",
                            "AI stream event exceeds the size limit",
                        ));
                    }
                    if !self.data.is_empty() {
                        self.data.push('\n');
                    }
                    self.data.push_str(value);
                } else if let Some(value) = line.strip_prefix("id:") {
                    let value = value.strip_prefix(' ').unwrap_or(value);
                    if value.len() > 256 || value.contains('\0') {
                        return Err(protocol("Invalid AI stream event identifier"));
                    }
                    self.id = Some(value.into());
                }
            } else {
                if self.line.len() >= 262_144 {
                    return Err(AppError::new(
                        "provider_limit",
                        "AI stream line exceeds the size limit",
                    ));
                }
                self.line.push(byte);
            }
        }
        Ok(())
    }
    fn event(&mut self, on_delta: &mut dyn FnMut(&str)) -> Result<()> {
        let data = std::mem::take(&mut self.data);
        let id = self.id.take();
        if data.is_empty() {
            return Ok(());
        }
        if let Some(id) = id.filter(|id| !id.is_empty()) {
            let hash: [u8; 32] = Sha256::digest(data.as_bytes()).into();
            if let Some(previous) = self.seen.get(&id) {
                if *previous != hash {
                    return Err(protocol(
                        "AI stream reused an identifier for different data",
                    ));
                }
                return Ok(());
            }
            if self.seen.len() >= 4096 {
                return Err(AppError::new(
                    "provider_limit",
                    "AI stream has too many identified events",
                ));
            }
            self.seen.insert(id, hash);
        }
        if data.trim() == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let frame: Value = serde_json::from_str(&data)
            .map_err(|_| protocol("AI provider returned a malformed stream frame"))?;
        if frame.get("error").is_some() {
            return Err(protocol("AI provider reported a stream error"));
        }
        let choices = frame
            .get("choices")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol("AI stream frame has no choices"))?;
        if choices.len() > 1 {
            return Err(protocol("AI stream returned multiple choices"));
        }
        if let Some(choice) = choices.first() {
            if choice.pointer("/delta/tool_calls").is_some_and(|value| {
                !value.is_null() && !value.as_array().is_some_and(Vec::is_empty)
            }) || choice
                .pointer("/delta/function_call")
                .is_some_and(|value| !value.is_null())
            {
                return Err(protocol("Native provider tool calls are not supported"));
            }
            if let Some(text) = choice
                .pointer("/delta/content")
                .filter(|value| !value.is_null())
            {
                let text = text
                    .as_str()
                    .ok_or_else(|| protocol("AI stream content is not text"))?;
                if self.stopped && !text.is_empty() {
                    return Err(protocol("AI stream returned text after completion"));
                }
                if self.content.len() + text.len() > 262_144 {
                    return Err(AppError::new(
                        "provider_limit",
                        "AI response exceeds the size limit",
                    ));
                }
                self.content.push_str(text);
                if !text.is_empty() {
                    on_delta(text);
                }
            }
            if let Some(reason) = choice.get("finish_reason").filter(|value| !value.is_null()) {
                if reason.as_str() != Some("stop") {
                    return Err(protocol("AI response ended without successful completion"));
                }
                self.stopped = true;
            }
        }
        if let Some(tokens) = frame.get("usage") {
            self.usage.input_tokens = tokens.get("prompt_tokens").and_then(Value::as_u64);
            self.usage.output_tokens = tokens.get("completion_tokens").and_then(Value::as_u64);
        }
        Ok(())
    }
    pub fn is_done(&self) -> bool {
        self.done
    }
    pub fn finish(self) -> Result<Completion> {
        if !self.done && (!self.stopped || !self.line.is_empty() || !self.data.is_empty()) {
            return Err(protocol("AI stream ended before a complete response"));
        }
        if self.content.is_empty() {
            return Err(AppError::new(
                "provider_empty",
                "AI provider returned no content",
            ));
        }
        Ok(Completion {
            content: self.content,
            usage: self.usage,
        })
    }
}
fn protocol(message: &str) -> AppError {
    AppError::new("provider_protocol", message)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn null_and_empty_tool_fields_do_not_reject_plain_text_streams() {
        for tool_calls in [Value::Null, json!([])] {
            let frame = json!({"choices":[{"delta":{"content":"plain text","tool_calls":tool_calls,"function_call":null},"finish_reason":"stop"}]});
            let stream = format!("data: {frame}\n\ndata: [DONE]\n\n");
            let mut decoder = StreamDecoder::default();
            let mut emitted = String::new();
            for byte in stream.as_bytes() {
                decoder
                    .feed(&[*byte], &mut |text| emitted.push_str(text))
                    .expect("null tool fields are not an invocation");
            }
            assert_eq!(emitted, "plain text");
            assert_eq!(decoder.finish().expect("completion").content, emitted);
        }
    }
    #[test]
    fn actual_or_malformed_tool_invocations_are_rejected_before_emitting_text() {
        for delta in [
            json!({"content":"untrusted","tool_calls":[{"function":{"name":"run_command","arguments":"{}"}}]}),
            json!({"content":"untrusted","function_call":{"name":"run_command","arguments":"{}"}}),
            json!({"content":"untrusted","tool_calls":{}}),
            json!({"content":"untrusted","function_call":"invalid"}),
        ] {
            let frame = json!({"choices":[{"delta":delta,"finish_reason":null}]});
            let stream = format!("data: {frame}\n\n");
            let mut decoder = StreamDecoder::default();
            let mut emitted = String::new();
            let error = decoder
                .feed(stream.as_bytes(), &mut |text| emitted.push_str(text))
                .expect_err("tool invocation is unsupported");
            assert_eq!(error.code, "provider_protocol");
            assert!(emitted.is_empty());
        }
    }
}
