use crate::error::{AppError, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderConfig {
    pub endpoint: String,
    pub model: String,
    pub api_key_env: String,
    #[serde(default)]
    pub embedding_model: String,
}
impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            model: String::new(),
            api_key_env: "ASTRAFORGE_API_KEY".into(),
            embedding_model: String::new(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}
pub struct Completion {
    pub content: String,
    pub usage: Usage,
}
pub trait ChatProvider: Send + Sync {
    fn stream_chat(
        &self,
        messages: &[ChatMessage],
        cancel: &AtomicBool,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<Completion>;
}
pub trait EmbeddingProvider: Send + Sync {
    fn identity(&self) -> String;
    fn embed(&self, inputs: &[String], cancel: &AtomicBool) -> Result<Vec<Vec<f32>>>;
}
pub struct CompatibleProvider {
    config: ProviderConfig,
    client: Client,
}
impl CompatibleProvider {
    pub fn new(config: ProviderConfig) -> Result<Self> {
        validate_endpoint(&config.endpoint)?;
        if config.model.trim().is_empty() {
            return Err(AppError::new(
                "provider_configuration",
                "Configure an AI model in Settings",
            ));
        }
        if !config
            .api_key_env
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(AppError::new(
                "provider_configuration",
                "Invalid API key environment variable name",
            ));
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| {
                AppError::new("provider_client", "Could not initialize the AI transport")
            })?;
        Ok(Self { config, client })
    }
    fn post(&self, resource: &str, body: Value) -> Result<reqwest::blocking::Response> {
        let endpoint = format!(
            "{}/{}",
            self.config.endpoint.trim_end_matches('/'),
            resource
        );
        let mut request = self.client.post(endpoint).json(&body);
        if let Ok(key) = std::env::var(&self.config.api_key_env) {
            if !key.is_empty() {
                request = request.bearer_auth(key);
            }
        }
        let response = request.send().map_err(|_| {
            AppError::new(
                "provider_network",
                "The configured AI provider could not be reached",
            )
        })?;
        if !response.status().is_success() {
            return Err(AppError::new(
                "provider_http",
                format!("AI provider returned HTTP {}", response.status().as_u16()),
            ));
        }
        Ok(response)
    }
}
pub fn validate_endpoint(endpoint: &str) -> Result<()> {
    let url = url::Url::parse(endpoint).map_err(|_| {
        AppError::new(
            "provider_configuration",
            "Configure an absolute AI provider endpoint",
        )
    })?;
    let loopback = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
    if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::new(
            "provider_configuration",
            "Use HTTPS, or HTTP on loopback, without embedded credentials or query parameters",
        ));
    }
    Ok(())
}
impl ChatProvider for CompatibleProvider {
    fn stream_chat(
        &self,
        messages: &[ChatMessage],
        cancel: &AtomicBool,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<Completion> {
        if cancel.load(Ordering::Relaxed) {
            return Err(AppError::new("cancelled", "AI request cancelled"));
        }
        let response = self.post(
            "chat/completions",
            json!({"model":self.config.model,"messages":messages,"stream":true,"max_tokens":4096}),
        )?;
        let mut reader = BufReader::new(response);
        let mut content = String::new();
        let mut usage = Usage::default();
        let mut bytes = Vec::new();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(AppError::new("cancelled", "AI request cancelled"));
            }
            bytes.clear();
            let read = reader
                .by_ref()
                .take(262_145)
                .read_until(b'\n', &mut bytes)?;
            if read == 0 {
                break;
            }
            if read > 262_144 {
                return Err(AppError::new(
                    "provider_limit",
                    "AI stream frame exceeds the size limit",
                ));
            }
            let line = std::str::from_utf8(&bytes)
                .map_err(|_| AppError::new("provider_protocol", "AI response is not UTF-8"))?;
            let Some(data) = line.trim().strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                break;
            }
            let frame: Value = serde_json::from_str(data).map_err(|_| {
                AppError::new(
                    "provider_protocol",
                    "AI provider returned a malformed stream frame",
                )
            })?;
            if let Some(text) = frame
                .pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
            {
                if content.len() + text.len() > 262_144 {
                    return Err(AppError::new(
                        "provider_limit",
                        "AI response exceeds the size limit",
                    ));
                }
                content.push_str(text);
                on_delta(text);
            }
            if let Some(tokens) = frame.get("usage") {
                usage.input_tokens = tokens.get("prompt_tokens").and_then(Value::as_u64);
                usage.output_tokens = tokens.get("completion_tokens").and_then(Value::as_u64);
            }
        }
        if content.is_empty() {
            return Err(AppError::new(
                "provider_empty",
                "AI provider returned no content",
            ));
        }
        Ok(Completion { content, usage })
    }
}
impl EmbeddingProvider for CompatibleProvider {
    fn identity(&self) -> String {
        format!("{}:{}", self.config.endpoint, self.config.embedding_model)
    }
    fn embed(&self, inputs: &[String], cancel: &AtomicBool) -> Result<Vec<Vec<f32>>> {
        if cancel.load(Ordering::Relaxed) {
            return Err(AppError::new("cancelled", "Embedding request cancelled"));
        }
        if self.config.embedding_model.is_empty() {
            return Err(AppError::new(
                "embeddings_unavailable",
                "No embedding model configured",
            ));
        }
        if inputs.len() > 32 || inputs.iter().map(String::len).sum::<usize>() > 131_072 {
            return Err(AppError::new(
                "embedding_limit",
                "Embedding batch exceeds the size limit",
            ));
        }
        let response = self.post(
            "embeddings",
            json!({"model":self.config.embedding_model,"input":inputs}),
        )?;
        let mut bytes = Vec::new();
        response.take(4_194_305).read_to_end(&mut bytes)?;
        if bytes.len() > 4_194_304 {
            return Err(AppError::new(
                "embedding_limit",
                "Embedding response exceeds the size limit",
            ));
        }
        #[derive(Deserialize)]
        struct Item {
            index: usize,
            embedding: Vec<f32>,
        }
        #[derive(Deserialize)]
        struct Response {
            data: Vec<Item>,
        }
        let mut result: Response = serde_json::from_slice(&bytes)?;
        result.data.sort_by_key(|item| item.index);
        if result.data.len() != inputs.len()
            || result.data.iter().enumerate().any(|(i, item)| {
                item.index != i
                    || item.embedding.is_empty()
                    || item.embedding.len() > 8192
                    || item.embedding.iter().any(|v| !v.is_finite())
            })
        {
            return Err(AppError::new(
                "provider_protocol",
                "Embedding provider returned invalid vectors",
            ));
        }
        Ok(result.data.into_iter().map(|item| item.embedding).collect())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoints_enforce_transport_and_credential_policy() {
        assert!(validate_endpoint("https://api.example.org/v1").is_ok());
        assert!(validate_endpoint("http://127.0.0.1:11434/v1").is_ok());
        for bad in [
            "http://example.org",
            "file:///tmp/a",
            "https://secret@example.org",
            "https://example.org?key=secret",
        ] {
            assert!(validate_endpoint(bad).is_err());
        }
    }
}
