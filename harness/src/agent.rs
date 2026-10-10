//! The model/tool loop for one attempt.
//!
//! History is append-only so the provider's prefix cache keeps hitting; every
//! request is also memoized on disk (keyed by its exact body) when a cache
//! directory is configured, which makes reruns and tests free.

use crate::responses::{Client, Completion, Item, Usage, parse_arguments};
use crate::tools::{Ctx, Tool, definition};
use anyhow::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub max_turns: usize,
    pub cache_dir: Option<PathBuf>,
    pub capture_requests: bool,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub submitted: bool,
    pub turns: usize,
    pub usage: Usage,
    pub last_message: Option<String>,
    pub error: Option<String>,
    pub termination: String,
}

pub async fn run(
    client: &Client,
    config: &AgentConfig,
    instructions: &str,
    prompt: &str,
    tools: &[Arc<dyn Tool>],
    ctx: &Ctx,
) -> Outcome {
    let mut outcome = Outcome::default();
    let result = run_loop(
        client,
        config,
        instructions,
        prompt,
        tools,
        ctx,
        &mut outcome,
    )
    .await;
    if let Err(error) = result {
        outcome.error = Some(format!("{error:#}"));
        outcome.termination = "error".into();
        ctx.trace.event(
            &ctx.id,
            "agent_error",
            json!({"turn": outcome.turns, "error": outcome.error, "usage": outcome.usage}),
        );
    }
    if outcome.termination.is_empty() {
        outcome.termination = "turn_limit".into();
    }
    outcome
}

async fn run_loop(
    client: &Client,
    config: &AgentConfig,
    instructions: &str,
    prompt: &str,
    tools: &[Arc<dyn Tool>],
    ctx: &Ctx,
    outcome: &mut Outcome,
) -> Result<()> {
    let specs: Vec<Value> = tools.iter().map(|t| definition(&t.spec())).collect();
    let mut items = vec![Item::user(prompt)];

    while outcome.turns < config.max_turns {
        if ctx.stop.is_cancelled() {
            outcome.termination = "cancelled".into();
            break;
        }
        ctx.trace.check()?;
        outcome.turns += 1;
        let body = client.body(instructions, &items, &specs, Some(&ctx.id));
        let started = Instant::now();
        let request_file = ctx
            .artifacts
            .join(format!("request-{:04}.json", outcome.turns));
        if config.capture_requests {
            crate::trace::artifact(&request_file, &serde_json::to_vec(&body)?)?;
        }
        ctx.trace.event(&ctx.id, "model_start", json!({"turn": outcome.turns, "request": config.capture_requests.then_some(&request_file)}));
        let (completion, cached) = match cached_completion(config, &body, client.endpoint()) {
            Some(c) => (c, true),
            None => {
                let c = tokio::select! {
                    biased;
                    _ = ctx.stop.cancelled() => { outcome.termination = "cancelled".into(); break; }
                    result = client.complete(&body, &ctx.trace, &ctx.id) => result?,
                };
                store_completion(config, &body, &c, client.endpoint());
                (c, false)
            }
        };
        outcome.usage.add(&completion.usage);
        if config.capture_requests {
            crate::trace::artifact(
                &ctx.artifacts
                    .join(format!("response-{:04}.json", outcome.turns)),
                &serde_json::to_vec(&completion)?,
            )?;
        }
        ctx.trace.event(
            &ctx.id,
            "model",
            json!({
                "turn": outcome.turns, "ms": started.elapsed().as_millis(), "cached_response": cached,
                "usage": completion.usage, "items": completion.items.len(), "incomplete": completion.incomplete,
                "text": completion.items.iter().filter_map(Item::assistant_text).collect::<Vec<_>>().join("\n"),
            }),
        );
        if let Some(text) = completion
            .items
            .iter()
            .filter_map(Item::assistant_text)
            .next_back()
        {
            outcome.last_message = Some(text);
        }

        let calls: Vec<(String, String, String)> = completion
            .items
            .iter()
            .filter_map(|item| match item {
                Item::FunctionCall {
                    call_id,
                    name,
                    arguments,
                    ..
                } => Some((call_id.clone(), name.clone(), arguments.clone())),
                _ => None,
            })
            .collect();
        let incomplete = completion.incomplete.clone();
        items.extend(completion.items);
        if calls.is_empty() {
            if let Some(reason) = incomplete {
                // The turn was cut off before any action; ask for a shorter one.
                items.push(Item::user(format!(
                    "Your previous turn stopped early ({reason}) before calling any tool. Continue from where you were, keep reasoning brief, and act with tools in small steps."
                )));
                continue;
            }
            outcome.termination = "no_tool_calls".into();
            break;
        }

        // Model call order is observable. Concurrency is explicit in lean_check
        // batches and independently owned subagents.
        for (call_id, name, arguments) in &calls {
            let output = tokio::select! {
                biased;
                _ = ctx.stop.cancelled() => { outcome.termination = "cancelled".into(); break; }
                output = invoke(tools, call_id, name, arguments, ctx) => output,
            };
            items.push(Item::output(call_id, output));
            if ctx.submitted.load(Ordering::SeqCst) {
                break;
            }
        }
        if ctx.submitted.load(Ordering::SeqCst) {
            outcome.submitted = true;
            outcome.termination = "submitted".into();
            break;
        }
    }
    Ok(())
}

async fn invoke(
    tools: &[Arc<dyn Tool>],
    call_id: &str,
    name: &str,
    arguments: &str,
    ctx: &Ctx,
) -> String {
    let started = Instant::now();
    ctx.trace.event(
        &ctx.id,
        "tool_start",
        json!({"call_id": call_id, "name": name, "arguments": arguments}),
    );
    let output = match (
        tools.iter().find(|t| t.spec().name == name),
        parse_arguments(arguments),
    ) {
        (Some(tool), Ok(args)) => {
            match tokio::time::timeout(std::time::Duration::from_secs(1830), tool.call(args, ctx))
                .await
            {
                Ok(result) => result.unwrap_or_else(|e| format!("ERROR: {e:#}")),
                Err(_) => {
                    ctx.stop.cancel();
                    "ERROR: tool watchdog expired; stopping attempt".into()
                }
            }
        }
        (None, _) => format!("ERROR: unknown tool `{name}`"),
        (_, Err(e)) => format!("ERROR: {e:#}"),
    };
    ctx.trace.event(
        &ctx.id,
        "tool",
        json!({"call_id": call_id, "name": name, "error": output.starts_with("ERROR:"), "arguments": arguments, "ms": started.elapsed().as_millis(), "output": output}),
    );
    output
}

fn cache_path(config: &AgentConfig, body: &Value, endpoint: &str) -> Option<PathBuf> {
    let dir = config.cache_dir.as_ref()?;
    let mut hash = Sha256::new();
    hash.update(endpoint.as_bytes());
    hash.update([0]);
    hash.update(serde_json::to_vec(body).ok()?);
    let digest = hash.finalize();
    Some(dir.join(format!("{:x}.json", digest)))
}

fn cached_completion(config: &AgentConfig, body: &Value, endpoint: &str) -> Option<Completion> {
    let text = std::fs::read(cache_path(config, body, endpoint)?).ok()?;
    serde_json::from_slice(&text).ok()
}

fn store_completion(config: &AgentConfig, body: &Value, completion: &Completion, endpoint: &str) {
    if let Some(path) = cache_path(config, body, endpoint) {
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let _ = std::fs::write(path, serde_json::to_vec(completion).unwrap_or_default());
    }
}
