use crate::error::{AppError, Result};
use crate::provider_stream::StreamDecoder;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::future::{poll_fn, Future};
use std::io;
use std::net::ToSocketAddrs;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::Poll;
use std::time::Duration;
type DnsError = Box<dyn std::error::Error + Send + Sync>;
#[derive(Default)]
struct BoundedResolver {
    active: Arc<AtomicUsize>,
}
struct DnsLease {
    active: Arc<AtomicUsize>,
}
impl Drop for DnsLease {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}
impl BoundedResolver {
    fn resolve_with(
        &self,
        lookup: impl FnOnce() -> io::Result<Addrs> + Send + 'static,
    ) -> Resolving {
        let lease = self
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                if active < 4 {
                    Some(active + 1)
                } else {
                    None
                }
            })
            .map(|_| DnsLease {
                active: self.active.clone(),
            });
        Box::pin(async move {
            let lease = lease.map_err(|_| {
                Box::new(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "AI DNS resolution capacity is exhausted",
                )) as DnsError
            })?;
            tokio::task::spawn_blocking(move || {
                let _lease = lease;
                lookup()
            })
            .await
            .map_err(|_| Box::new(io::Error::other("AI DNS worker failed")) as DnsError)?
            .map_err(|error| Box::new(error) as DnsError)
        })
    }
}
impl Resolve for BoundedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        self.resolve_with(move || {
            (host.as_str(), 0)
                .to_socket_addrs()
                .map(|addresses| Box::new(addresses) as Addrs)
        })
    }
}
fn dns_resolver() -> Arc<BoundedResolver> {
    static RESOLVER: OnceLock<Arc<BoundedResolver>> = OnceLock::new();
    RESOLVER
        .get_or_init(|| Arc::new(BoundedResolver::default()))
        .clone()
}
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
        Ok(Self { config })
    }
    async fn post(
        &self,
        resource: &str,
        body: Value,
        cancel: &AtomicBool,
    ) -> Result<reqwest::Response> {
        let client = Client::builder()
            .dns_resolver(dns_resolver())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| {
                AppError::new("provider_client", "Could not initialize the AI transport")
            })?;
        let endpoint = format!(
            "{}/{}",
            self.config.endpoint.trim_end_matches('/'),
            resource
        );
        let mut request = client.post(endpoint).json(&body);
        if let Ok(key) = std::env::var(&self.config.api_key_env) {
            if !key.is_empty() {
                request = request.bearer_auth(key);
            }
        }
        let response = cancellable(request.send(), cancel).await?;
        if !response.status().is_success() {
            return Err(AppError::new(
                "provider_http",
                format!("AI provider returned HTTP {}", response.status().as_u16()),
            ));
        }
        Ok(response)
    }
}
fn runtime() -> Result<&'static tokio::runtime::Runtime> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .max_blocking_threads(4)
                .thread_name("astraforge-provider")
                .enable_all()
                .build()
                .map_err(|_| {
                    AppError::new(
                        "provider_client",
                        "Could not initialize the AI transport runtime",
                    )
                })
        })
        .as_ref()
        .map_err(Clone::clone)
}
async fn cancellable<T>(
    future: impl Future<Output = std::result::Result<T, reqwest::Error>>,
    cancel: &AtomicBool,
) -> Result<T> {
    let mut future = Box::pin(future);
    let mut timer = tokio::time::interval(Duration::from_millis(20));
    poll_fn(|context| {
        if cancel.load(Ordering::SeqCst) {
            return Poll::Ready(Err(AppError::new("cancelled", "AI request cancelled")));
        }
        if let Poll::Ready(result) = future.as_mut().poll(context) {
            if cancel.load(Ordering::SeqCst) {
                return Poll::Ready(Err(AppError::new("cancelled", "AI request cancelled")));
            }
            return Poll::Ready(result.map_err(|_| {
                AppError::new(
                    "provider_network",
                    "The AI provider connection failed or timed out",
                )
            }));
        }
        while timer.poll_tick(context).is_ready() {}
        Poll::Pending
    })
    .await
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
        runtime()?.block_on(async {
            let mut response = self.post("chat/completions", json!({"model":self.config.model,"messages":messages,"stream":true,"max_tokens":4096}), cancel).await?;
            let mut decoder = StreamDecoder::default();
            while let Some(chunk) = cancellable(response.chunk(), cancel).await? {
                decoder.feed(&chunk, on_delta)?;
                if decoder.is_done() {
                    break;
                }
            }
            if cancel.load(Ordering::SeqCst) {
                return Err(AppError::new("cancelled", "AI request cancelled"));
            }
            decoder.finish()
        })
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
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let bytes = runtime()?.block_on(async {
            let mut response = self
                .post(
                    "embeddings",
                    json!({"model":self.config.embedding_model,"input":inputs}),
                    cancel,
                )
                .await?;
            let mut bytes = Vec::new();
            while let Some(chunk) = cancellable(response.chunk(), cancel).await? {
                if bytes.len() + chunk.len() > 4_194_304 {
                    return Err(AppError::new(
                        "embedding_limit",
                        "Embedding response exceeds the size limit",
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(bytes)
        })?;
        #[derive(Deserialize)]
        struct Item {
            index: usize,
            embedding: Vec<f32>,
        }
        #[derive(Deserialize)]
        struct Response {
            data: Vec<Item>,
        }
        let mut result: Response = serde_json::from_slice(&bytes).map_err(|_| {
            AppError::new(
                "provider_protocol",
                "Embedding provider returned malformed data",
            )
        })?;
        result.data.sort_by_key(|item| item.index);
        let dimensions = result
            .data
            .first()
            .map(|item| item.embedding.len())
            .unwrap_or(0);
        if result.data.len() != inputs.len()
            || result.data.iter().enumerate().any(|(i, item)| {
                item.index != i
                    || item.embedding.is_empty()
                    || item.embedding.len() != dimensions
                    || item.embedding.len() > 8192
                    || item.embedding.iter().any(|v| !v.is_finite())
            })
        {
            return Err(AppError::new(
                "provider_protocol",
                "Embedding provider returned invalid vectors",
            ));
        }
        if cancel.load(Ordering::SeqCst) {
            return Err(AppError::new("cancelled", "Embedding request cancelled"));
        }
        Ok(result.data.into_iter().map(|item| item.embedding).collect())
    }
}
#[cfg(test)]
#[path = "provider_dns_tests.rs"]
mod dns_tests;
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
