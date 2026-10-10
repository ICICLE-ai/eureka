//! Streaming client for the OpenAI Responses API (`POST /responses`).
//!
//! The event handling is derived from openai/codex `codex-rs/codex-api/src/sse/responses.rs`
//! and `codex-client/src/retry.rs` (Apache-2.0, commit 402f5b6), reduced to what a
//! single-model prover needs: complete output items, usage, errors, idle timeout, retries.
//! Items we do not interpret (reasoning) are kept verbatim so they can be sent back.

use anyhow::{Context, Result, anyhow, bail};
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::time::Duration;

/// One item of the conversation, in the Responses API wire format.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Item {
    Message {
        role: String,
        content: Vec<Content>,
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        /// JSON text, as the API sends it.
        arguments: String,
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
    /// Provider reasoning; passed back untouched on later turns.
    Reasoning {
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    InputText { text: String },
    OutputText { text: String },
}

impl Item {
    pub fn user(text: impl Into<String>) -> Self {
        Item::Message {
            role: "user".into(),
            content: vec![Content::InputText { text: text.into() }],
            rest: Map::new(),
        }
    }

    pub fn output(call_id: impl Into<String>, output: impl Into<String>) -> Self {
        Item::FunctionCallOutput {
            call_id: call_id.into(),
            output: output.into(),
        }
    }

    /// Visible text of an assistant message, if this is one.
    pub fn assistant_text(&self) -> Option<String> {
        match self {
            Item::Message { role, content, .. } if role == "assistant" => Some(
                content
                    .iter()
                    .map(|c| match c {
                        Content::InputText { text } | Content::OutputText { text } => text.as_str(),
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            ),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Completion {
    pub items: Vec<Item>,
    pub usage: Usage,
    /// Set when the response stopped early (e.g. `max_output_tokens`); the
    /// items received so far are still valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Request<'a> {
    pub model: &'a str,
    pub stream: bool,
    pub instructions: &'a str,
    pub input: &'a [Item],
    pub tools: &'a [Value],
    pub tool_choice: &'static str,
    pub parallel_tool_calls: bool,
    pub reasoning: Reasoning<'a>,
    pub store: bool,
    pub include: &'static [&'static str],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<&'a str>,
    pub max_output_tokens: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct Reasoning<'a> {
    pub effort: &'a str,
}

#[derive(Clone, Debug)]
pub struct ModelConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub effort: String,
    pub max_output_tokens: u32,
    pub idle_timeout: Duration,
    pub max_attempts: u32,
}

pub struct Client {
    http: reqwest::Client,
    pub config: ModelConfig,
}

#[derive(Deserialize)]
struct StreamEvent {
    #[serde(rename = "type")]
    kind: String,
    item: Option<Value>,
    response: Option<Value>,
    error: Option<Value>,
}

enum Attempt {
    Done(Completion),
    Retry(String),
}

impl Client {
    pub fn new(config: ModelConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { http, config })
    }

    /// Build the request body for `input` with the given tools. Public so the
    /// agent can key its response cache on the exact bytes sent.
    pub fn body(
        &self,
        instructions: &str,
        input: &[Item],
        tools: &[Value],
        cache_key: Option<&str>,
    ) -> Value {
        serde_json::to_value(Request {
            model: &self.config.model,
            stream: true,
            instructions,
            input,
            tools,
            tool_choice: "auto",
            parallel_tool_calls: true,
            reasoning: Reasoning {
                effort: &self.config.effort,
            },
            store: false,
            include: &["reasoning.encrypted_content"],
            prompt_cache_key: cache_key,
            max_output_tokens: self.config.max_output_tokens,
        })
        .expect("request is serializable")
    }

    pub fn endpoint(&self) -> &str {
        &self.config.base_url
    }

    /// One completion, with retries on 429/5xx/transport errors.
    pub async fn complete(
        &self,
        body: &Value,
        trace: &crate::trace::Trace,
        id: &str,
    ) -> Result<Completion> {
        let mut last = String::new();
        for attempt in 0..self.config.max_attempts {
            if attempt > 0 {
                tokio::time::sleep(backoff(attempt)).await;
            }
            trace.event(id, "request_start", serde_json::json!({"try": attempt + 1}));
            let response =
                match tokio::time::timeout(Duration::from_secs(1800), self.once(body)).await {
                    Ok(result) => result,
                    Err(_) => Ok(Attempt::Retry("request deadline exceeded".into())),
                };
            trace.event(id, "request_end", serde_json::json!({"try": attempt + 1, "error": response.as_ref().err().map(|e| format!("{e:#}"))}));
            match response? {
                Attempt::Done(c) => return Ok(c),
                Attempt::Retry(reason) => {
                    trace.event(
                        id,
                        "request_retry",
                        serde_json::json!({"try": attempt + 1, "reason": reason}),
                    );
                    last = reason;
                }
            }
        }
        bail!(
            "model request failed after {} attempts: {last}",
            self.config.max_attempts
        )
    }

    async fn once(&self, body: &Value) -> Result<Attempt> {
        let url = format!("{}/responses", self.config.base_url.trim_end_matches('/'));
        let response = match self
            .http
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .json(body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return Ok(Attempt::Retry(format!("transport: {e}"))),
        };
        let status = response.status();
        if status.as_u16() == 429 || status.is_server_error() {
            let text = response.text().await.unwrap_or_default();
            return Ok(Attempt::Retry(format!("HTTP {status}: {text}")));
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("HTTP {status}: {text}");
        }

        let mut events = response.bytes_stream().eventsource();
        let mut items = Vec::new();
        let mut failure: Option<String> = None;
        loop {
            let next = tokio::time::timeout(self.config.idle_timeout, events.next()).await;
            let event = match next {
                Err(_) => return Ok(Attempt::Retry("idle timeout waiting for SSE".into())),
                Ok(None) => {
                    return Ok(Attempt::Retry(failure.unwrap_or_else(|| {
                        "stream closed before response.completed".into()
                    })));
                }
                Ok(Some(Err(e))) => return Ok(Attempt::Retry(format!("stream: {e}"))),
                Ok(Some(Ok(event))) => event,
            };
            let event: StreamEvent = match serde_json::from_str(&event.data) {
                Ok(e) => e,
                Err(_) => continue,
            };
            match event.kind.as_str() {
                "response.output_item.done" => {
                    // Unknown item kinds are skipped rather than failing the turn.
                    if let Some(item) = event
                        .item
                        .and_then(|v| serde_json::from_value::<Item>(v).ok())
                    {
                        items.push(item);
                    }
                }
                "response.completed" => {
                    let usage = event
                        .response
                        .as_ref()
                        .and_then(|r| r.get("usage"))
                        .map(parse_usage)
                        .unwrap_or_default();
                    return Ok(Attempt::Done(Completion {
                        items,
                        usage,
                        incomplete: None,
                    }));
                }
                "response.incomplete" => {
                    let reason = event
                        .response
                        .as_ref()
                        .and_then(|r| r.pointer("/incomplete_details/reason"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned();
                    if reason == "content_filter" {
                        bail!("incomplete response: {reason}");
                    }
                    let usage = event
                        .response
                        .as_ref()
                        .and_then(|r| r.get("usage"))
                        .map(parse_usage)
                        .unwrap_or_default();
                    return Ok(Attempt::Done(Completion {
                        items,
                        usage,
                        incomplete: Some(reason),
                    }));
                }
                "response.failed" => {
                    let message = event
                        .response
                        .as_ref()
                        .and_then(|r| r.pointer("/error/message"))
                        .and_then(Value::as_str)
                        .unwrap_or("response.failed");
                    return Ok(Attempt::Retry(message.to_owned()));
                }
                "error" => {
                    failure = Some(
                        event
                            .error
                            .as_ref()
                            .and_then(|e| e.get("message"))
                            .and_then(Value::as_str)
                            .unwrap_or("error event")
                            .to_owned(),
                    );
                }
                _ => {}
            }
        }
    }
}

fn parse_usage(usage: &Value) -> Usage {
    let get = |p: &str| usage.pointer(p).and_then(Value::as_u64).unwrap_or(0);
    Usage {
        input_tokens: get("/input_tokens"),
        cached_input_tokens: get("/input_tokens_details/cached_tokens"),
        output_tokens: get("/output_tokens"),
        reasoning_tokens: get("/output_tokens_details/reasoning_tokens"),
    }
}

/// Exponential backoff with jitter: 1s, 2s, 4s, ... capped at 30s.
fn backoff(attempt: u32) -> Duration {
    let base = 1000u64.saturating_mul(1u64 << attempt.min(5));
    let jitter = (std::process::id() as u64 ^ attempt as u64) % 200;
    Duration::from_millis(base.min(30_000) + jitter)
}

/// Convenience for callers that need a hard error from a tool-argument string.
pub fn parse_arguments(arguments: &str) -> Result<Value> {
    if arguments.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(arguments)
        .with_context(|| format!("tool arguments are not valid JSON: {arguments}"))
        .and_then(|v: Value| {
            if v.is_object() {
                Ok(v)
            } else {
                Err(anyhow!("tool arguments must be a JSON object"))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_round_trip_and_keep_unknown_fields() {
        let json = serde_json::json!({
            "type": "reasoning", "id": "r1", "status": "completed",
            "content": [{"type": "reasoning_text", "text": "think"}],
            "encrypted_content": "abc"
        });
        let item: Item = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&item).unwrap(), json);

        let call = serde_json::json!({
            "type": "function_call", "id": "f1", "status": "completed",
            "call_id": "call_1", "name": "lean_goals", "arguments": "{\"line\": 3}"
        });
        let item: Item = serde_json::from_value(call.clone()).unwrap();
        assert_eq!(serde_json::to_value(&item).unwrap(), call);
        assert!(matches!(item, Item::FunctionCall { ref name, .. } if name == "lean_goals"));
    }

    #[test]
    fn usage_parses_cache_fields() {
        let u = parse_usage(&serde_json::json!({
            "input_tokens": 343, "input_tokens_details": {"cached_tokens": 128},
            "output_tokens": 112, "output_tokens_details": {"reasoning_tokens": 22}
        }));
        assert_eq!(u.cached_input_tokens, 128);
        assert_eq!(u.reasoning_tokens, 22);
    }

    #[test]
    fn arguments_must_be_objects() {
        assert!(parse_arguments("").unwrap().is_object());
        assert!(parse_arguments("[1]").is_err());
        assert!(parse_arguments("{\"a\":1}").is_ok());
    }
}
