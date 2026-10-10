//! Attempts own their sandbox and child tasks until cleanup has completed.
use crate::agent::{self, AgentConfig, Outcome};
use crate::beam::{Beam, ExtraServer, ToolSpec};
use crate::pool::PoolConfig;
use crate::responses::{Client, Usage};
use crate::sandbox::{Sandbox, SandboxSpec, WORK};
use crate::tools::{
    BeamTool, Ctx, ExtraTool, Finish, LeanCheck, ReadFile, Role, Shell, Submit, Tool, WriteFile,
};
use crate::trace::{Trace, artifact};
use anyhow::{Context, Result, anyhow, bail};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use worktrees::{Error, Frozen, PublishOutcome, Published, Task, Verified};

pub struct Harness {
    pub task: Task,
    pub client: Client,
    pub sandbox: SandboxSpec,
    pub pool: PoolConfig,
    pub trace: Arc<Trace>,
    pub agent: AgentConfig,
    pub instructions: String,
    pub shell: bool,
    pub shell_timeout: Duration,
    pub stop: CancellationToken,
    pub usage: Mutex<Usage>,
    pub artifacts: PathBuf,
    pub next_id: AtomicU64,
    pub extra_servers: Vec<ExtraServer>,
}

pub struct AttemptResult {
    pub id: String,
    pub label: String,
    pub submitted: bool,
    pub turns: usize,
    pub usage: Usage,
    pub verdict: Verdict,
    pub termination: String,
    pub note: Option<String>,
    pub last_message: Option<String>,
    pub target_text: Option<String>,
    pub artifacts: PathBuf,
}

pub enum Verdict {
    NotSubmitted,
    Finished,
    Verified(Verified),
    Rejected(String),
    Failed(String),
}

impl AttemptResult {
    pub fn report(&self) -> Value {
        let (status, detail) = match &self.verdict {
            Verdict::NotSubmitted => ("not_submitted", ""),
            Verdict::Finished => ("finished", ""),
            Verdict::Verified(v) => ("verified", v.report()),
            Verdict::Rejected(s) => ("rejected", s.as_str()),
            Verdict::Failed(s) => ("failed", s.as_str()),
        };
        json!({"id": self.id, "label": self.label, "status": status, "detail": detail,
            "submitted": self.submitted, "turns": self.turns, "usage": self.usage,
            "termination": self.termination, "note": self.note, "last_message": self.last_message,
            "candidate": self.target_text.as_ref().map(|_| self.artifacts.join("candidate.lean"))})
    }
}

/// A lead owns one bounded set. Cancel cooperatively, then join; never abort
/// futures that may still own containers. Finished results remain on disk.
struct Children {
    running: JoinSet<Result<AttemptResult>>,
    stop: CancellationToken,
}
impl Children {
    async fn close(&mut self) -> Result<()> {
        self.stop.cancel();
        let mut error = None;
        while let Some(result) = self.running.join_next().await {
            if let Err(e) = result.map_err(anyhow::Error::from).and_then(|r| r) {
                error = Some(e);
            }
        }
        match error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl Harness {
    fn id(&self) -> String {
        format!(
            "attempt-{:06}",
            self.next_id.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn tools(
        self: &Arc<Self>,
        beam: &Beam,
        role: Role,
        children: Arc<tokio::sync::Mutex<Children>>,
    ) -> Vec<Arc<dyn Tool>> {
        let mut tools: Vec<Arc<dyn Tool>> = beam
            .tools()
            .iter()
            .map(|t| Arc::new(BeamTool(t.clone())) as Arc<dyn Tool>)
            .collect();
        tools.extend([
            Arc::new(LeanCheck) as Arc<dyn Tool>,
            Arc::new(ReadFile),
            Arc::new(WriteFile),
        ]);
        if self.shell {
            tools.push(Arc::new(Shell {
                timeout: self.shell_timeout,
            }));
        }
        if role == Role::Lead {
            tools.push(Arc::new(Subagents {
                harness: Arc::downgrade(self),
                children: children.clone(),
                wait: false,
            }));
            tools.push(Arc::new(Subagents {
                harness: Arc::downgrade(self),
                children,
                wait: true,
            }));
        }
        tools.push(match role {
            Role::Lead => Arc::new(Submit),
            Role::Subagent => Arc::new(Finish),
        });
        tools
    }

    pub fn run_attempt<'a>(
        self: &'a Arc<Self>,
        id: String,
        label: String,
        prompt: String,
        seed: Option<String>,
        role: Role,
        stop: CancellationToken,
    ) -> BoxFuture<'a, Result<AttemptResult>> {
        Box::pin(async move {
            let artifacts = self.artifacts.join(&id);
            std::fs::create_dir(&artifacts)?;
            self.trace.event(
                &id,
                "attempt_start",
                json!({"label": label, "role": format!("{role:?}"), "artifacts": artifacts}),
            );
            let task = self.task.clone();
            let attempt_id = id.clone();
            let attempt = tokio::task::spawn_blocking(move || -> Result<_> {
                let mut attempt = task.attempt(attempt_id)?;
                if let Some(seed) = seed {
                    attempt.write(seed.as_bytes())?;
                }
                Ok(attempt)
            })
            .await??;
            let root = attempt.root();
            let children = Arc::new(tokio::sync::Mutex::new(Children {
                running: JoinSet::new(),
                stop: stop.child_token(),
            }));
            let mut sandbox = None;
            let mut note = None;
            let mut outcome = match async {
                let s = Arc::new(Sandbox::create(&self.sandbox, &id, &root).await?);
                sandbox = Some(s.clone());
                self.pool.attach(&s, &root).await?;
                let beam = tokio::time::timeout(
                    Duration::from_secs(30),
                    Beam::spawn(self.pool.mcp(&s), WORK),
                )
                .await
                .context("Beam startup timeout")??;
                let beam = Arc::new(beam);
                let ctx = Ctx {
                    id: id.clone(),
                    role,
                    root: root.clone(),
                    target: self.task.target().clone(),
                    beam: beam.clone(),
                    sandbox: s.clone(),
                    trace: self.trace.clone(),
                    submitted: AtomicBool::new(false),
                    note: Mutex::new(None),
                    artifacts: artifacts.clone(),
                    stop: stop.clone(),
                };
                let mut tools = self.tools(&beam, role, children.clone());
                for config in &self.extra_servers {
                    let args: Vec<&str> = config.args.iter().map(String::as_str).collect();
                    let server = Arc::new(
                        tokio::time::timeout(
                            Duration::from_secs(30),
                            Beam::connect(s.command(&config.command, &args), None),
                        )
                        .await
                        .context("extra MCP startup timeout")??,
                    );
                    for spec in server.tools() {
                        let mut named = spec.clone();
                        named.name = format!("mcp_{}_{}", config.name, spec.name);
                        anyhow::ensure!(
                            !tools.iter().any(|t| t.spec().name == named.name),
                            "duplicate MCP tool {}",
                            named.name
                        );
                        tools.push(Arc::new(ExtraTool {
                            spec: named,
                            original_name: spec.name.clone(),
                            server: server.clone(),
                        }));
                    }
                }
                let outcome = agent::run(
                    &self.client,
                    &self.agent,
                    &self.instructions,
                    &prompt,
                    &tools,
                    &ctx,
                )
                .await;
                note = ctx.note.lock().unwrap().clone();
                Ok::<_, anyhow::Error>(outcome)
            }
            .await
            {
                Ok(outcome) => outcome,
                Err(error) => Outcome {
                    error: Some(format!("startup: {error:#}")),
                    termination: "startup_error".into(),
                    ..Default::default()
                },
            };
            if let Err(error) = children.lock().await.close().await {
                outcome.error = Some(format!("child cleanup: {error:#}"));
            }
            if let Some(sandbox) = sandbox
                && let Err(error) = sandbox.close().await
            {
                // Never remove a bind mount while a writer may still be alive.
                // Retain the workspace for explicit recovery if Docker is unavailable.
                std::mem::forget(attempt);
                artifact(
                    &artifacts.join("result.json"),
                    &serde_json::to_vec_pretty(
                        &json!({"status": "cleanup_failed", "root": root, "error": format!("{error:#}")}),
                    )?,
                )?;
                bail!(
                    "{id}: cannot stop sandbox; retained {}: {error:#}",
                    root.display()
                );
            }
            self.usage.lock().unwrap().add(&outcome.usage);
            let frozen = tokio::task::spawn_blocking(move || attempt.freeze()).await?;
            let mut result = AttemptResult {
                id: id.clone(),
                label,
                submitted: outcome.submitted,
                turns: outcome.turns,
                usage: outcome.usage,
                verdict: Verdict::NotSubmitted,
                termination: outcome.termination,
                note,
                last_message: outcome.last_message,
                target_text: None,
                artifacts,
            };
            let frozen = match frozen {
                Ok(frozen) => {
                    artifact(&result.artifacts.join("candidate.lean"), frozen.bytes())?;
                    result.target_text = Some(String::from_utf8(frozen.bytes().to_vec())?);
                    Some(frozen)
                }
                Err(error) => {
                    outcome.error = Some(format!("capture: {error}"));
                    None
                }
            };
            // Persist progress before any verifier process can fail.
            let mut pending = result.report();
            pending["status"] = json!(if outcome.submitted && role == Role::Lead {
                "verifying"
            } else {
                "captured"
            });
            artifact(
                &result.artifacts.join("result.json"),
                &serde_json::to_vec_pretty(&pending)?,
            )?;
            result.verdict = match (outcome.error, frozen) {
                (Some(error), _) => Verdict::Failed(error),
                (None, Some(frozen)) if result.submitted && role == Role::Lead => {
                    self.trace.event(
                        &id,
                        "verification_start",
                        json!({"candidate": result.artifacts.join("candidate.lean")}),
                    );
                    match verify_candidate(self.task.clone(), frozen, &result.artifacts).await {
                        Ok(v) => Verdict::Verified(v),
                        Err(Error::Rejected(report)) => Verdict::Rejected(report),
                        Err(error) => Verdict::Failed(format!("verify: {error}")),
                    }
                }
                (None, _) if result.submitted => Verdict::Finished,
                _ => Verdict::NotSubmitted,
            };
            let report = result.report();
            artifact(
                &result.artifacts.join("result.json"),
                &serde_json::to_vec_pretty(&report)?,
            )?;
            self.trace.event(&id, "attempt_end", report);
            if let Verdict::Failed(error) = &result.verdict {
                eprintln!("{id}: {error}");
            }
            self.trace.check()?;
            Ok(result)
        })
    }

    pub async fn publish(self: &Arc<Self>, verified: Verified) -> Result<Published> {
        let task = self.task.clone();
        let outcome = tokio::task::spawn_blocking(move || task.publish(verified)).await?;
        let published = match outcome {
            Ok(PublishOutcome::Committed(p)) | Ok(PublishOutcome::AlreadyWon(p)) => p,
            Err(Error::CommitUncertain { published, source }) => {
                self.trace.event(
                    "harness",
                    "commit_uncertain",
                    json!({"error": source.to_string()}),
                );
                published
            }
            Err(e) => return Err(e.into()),
        };
        self.stop.cancel();
        Ok(published)
    }

    pub async fn independent(
        self: &Arc<Self>,
        n: usize,
        prompt: &str,
        seed: Option<&str>,
    ) -> Result<Option<Published>> {
        let mut running = JoinSet::new();
        for i in 1..=n {
            let h = self.clone();
            let id = self.id();
            let prompt = prompt.to_owned();
            let seed = seed.map(str::to_owned);
            running.spawn(async move {
                h.run_attempt(
                    id,
                    format!("agent-{i}"),
                    prompt,
                    seed,
                    Role::Lead,
                    h.stop.child_token(),
                )
                .await
            });
        }
        let mut winner = None;
        let mut error = None;
        while let Some(joined) = running.join_next().await {
            match joined.map_err(anyhow::Error::from).and_then(|r| r) {
                Ok(result) => {
                    eprintln!(
                        "{} ({}): {} after {} turns ({} in / {} cached / {} out tokens)",
                        result.label,
                        result.id,
                        result.report()["status"],
                        result.turns,
                        result.usage.input_tokens,
                        result.usage.cached_input_tokens,
                        result.usage.output_tokens
                    );
                    if let Verdict::Verified(v) = result.verdict
                        && winner.is_none()
                    {
                        match self.publish(v).await {
                            Ok(p) => winner = Some(p),
                            Err(e) => {
                                error = Some(e);
                                self.stop.cancel();
                            }
                        }
                    }
                }
                Err(e) => {
                    error = Some(e);
                    self.stop.cancel();
                }
            }
        }
        // Also catches containers left by a panicking task, after every task joins.
        crate::sandbox::prune(&self.sandbox.run_id).await?;
        self.trace.event(
            "harness",
            "run_end",
            json!({"usage": self.usage.lock().unwrap().clone()}),
        );
        if let Some(error) = error {
            return Err(error);
        }
        Ok(winner)
    }
}

/// Retry only operational verifier failures, once, using the identical saved bytes
/// in another fresh trusted tree. Both reports survive regardless of the verdict.
pub async fn verify_candidate(task: Task, frozen: Frozen, dir: &Path) -> Result<Verified, Error> {
    let mut frozen = frozen;
    let mut reports = String::new();
    for round in 1..=2 {
        let bytes = frozen.bytes().to_vec();
        let result = tokio::task::spawn_blocking(move || frozen.verify())
            .await
            .map_err(|e| Error::VerifierFailed(std::io::Error::other(e.to_string())))?;
        let report = match &result {
            Ok(v) => v.report().to_owned(),
            Err(e) => e.to_string(),
        };
        reports.push_str(&format!("Verification {round}\n{report}\n"));
        artifact(&dir.join("verification.txt"), reports.as_bytes())
            .map_err(|e| Error::VerifierFailed(std::io::Error::other(e.to_string())))?;
        if round == 2 || !matches!(result, Err(Error::VerifierFailed(_))) {
            return result;
        }
        let mut attempt = task.attempt("verification-retry")?;
        attempt.write(&bytes)?;
        frozen = attempt.freeze()?;
    }
    unreachable!()
}

struct Subagents {
    harness: std::sync::Weak<Harness>,
    children: Arc<tokio::sync::Mutex<Children>>,
    wait: bool,
}
impl Tool for Subagents {
    fn spec(&self) -> ToolSpec {
        if self.wait {
            return ToolSpec { name: "wait_subagents".into(), description: "Wait up to 30 seconds for a child to finish; return newly completed results (including notes and target text), plus the remaining count. You may continue your own work and call again. Each result is delivered once and also saved as an artifact.".into(), input_schema: json!({"type": "object", "properties": {}, "additionalProperties": false}), read_only: false };
        }
        ToolSpec { name: "spawn_subagents".into(), description: "Start subagents in private projects seeded with your current target. First checks that the seed elaborates (sorries allowed). Returns IDs immediately; use wait_subagents to collect results while continuing your own work. At most 8 uncollected children. Submit/termination cancels and joins remaining children; collect their work first.".into(), input_schema: json!({"type": "object", "properties": {"tasks": {"type": "array", "minItems": 1, "maxItems": 8, "items": {"type": "object", "properties": {"label": {"type": "string"}, "instructions": {"type": "string"}}, "required": ["label", "instructions"], "additionalProperties": false}}}, "required": ["tasks"], "additionalProperties": false}), read_only: false }
    }
    fn call<'a>(&'a self, args: Value, ctx: &'a Ctx) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let mut children = self.children.lock().await;
            if self.wait {
                let mut completed = Vec::new();
                if let Ok(Some(joined)) =
                    tokio::time::timeout(Duration::from_secs(30), children.running.join_next())
                        .await
                {
                    completed.push(child_report(joined));
                }
                while let Some(joined) = children.running.try_join_next() {
                    completed.push(child_report(joined));
                }
                return Ok(serde_json::to_string(
                    &json!({"completed": completed, "remaining": children.running.len()}),
                )?);
            }
            let harness = self
                .harness
                .upgrade()
                .ok_or_else(|| anyhow!("harness is gone"))?;
            let tasks = args
                .get("tasks")
                .and_then(Value::as_array)
                .context("missing tasks")?;
            if tasks.is_empty() || tasks.len() + children.running.len() > 8 {
                bail!("1..8 tasks; collect previous children before exceeding 8");
            }
            let tasks: Vec<(String, String)> = tasks
                .iter()
                .map(|t| {
                    Ok((
                        t["label"].as_str().context("missing label")?.to_owned(),
                        t["instructions"]
                            .as_str()
                            .context("missing instructions")?
                            .to_owned(),
                    ))
                })
                .collect::<Result<_>>()?;
            let sync = ctx.beam.call("lean_sync", &json!({"path": ctx.target.as_str(), "diagnostic_scope": "all", "diagnostics_in_result": true})).await?;
            let diagnostics: Value =
                serde_json::from_str(&sync).with_context(|| format!("seed sync failed: {sync}"))?;
            if diagnostics
                .pointer("/diagnostics/counts/error")
                .and_then(Value::as_u64)
                != Some(0)
                || diagnostics.pointer("/readiness/save_ready") != Some(&json!(true))
            {
                bail!(
                    "repair the seed before spawning; sorries are allowed, elaboration errors are not: {sync}"
                );
            }
            let seed = String::from_utf8(worktrees::read_beneath(
                &ctx.root,
                &ctx.target,
                4 * 1024 * 1024,
            )?)?;
            let mut ids = Vec::new();
            for (label, prompt) in tasks {
                let id = harness.id();
                ids.push(json!({"id": id, "label": label}));
                ctx.trace
                    .event(&ctx.id, "child_spawn", json!({"child": id, "label": label}));
                let h = harness.clone();
                let seed = seed.clone();
                let stop = children.stop.child_token();
                children.running.spawn(async move {
                    h.run_attempt(id, label, prompt, Some(seed), Role::Subagent, stop)
                        .await
                });
            }
            Ok(serde_json::to_string(&json!({"started": ids}))?)
        })
    }
}

fn child_report(joined: Result<Result<AttemptResult>, tokio::task::JoinError>) -> Value {
    match joined.map_err(anyhow::Error::from).and_then(|r| r) {
        Ok(r) => {
            let mut report = r.report();
            report["target_text"] = json!(r.target_text);
            report
        }
        Err(e) => json!({"status": "failed", "detail": format!("{e:#}")}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use worktrees::{Options, Pool, RelPath, Verification, Verifier, VerifyError};

    struct CrashOnce(AtomicU64);
    impl Verifier for CrashOnce {
        fn policy_id(&self) -> &str {
            "crash-once-test"
        }
        fn verify(&self, input: &Verification<'_>) -> Result<String, VerifyError> {
            assert_eq!(
                std::fs::read(input.root().join("Solution.lean")).unwrap(),
                b"saved candidate"
            );
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(VerifyError::Operational(std::io::Error::other(
                    "deliberate crash",
                )))
            } else {
                Ok("checked on retry".into())
            }
        }
    }

    #[tokio::test]
    async fn operational_retry_reuses_saved_bytes_and_retains_both_reports() {
        let dir = std::env::temp_dir().join(format!("harness-verify-retry-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let source = dir.join("source");
        let store = dir.join("store");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&store).unwrap();
        std::fs::write(source.join("Solution.lean"), "original").unwrap();
        let pool = Pool::create(
            &source,
            &store,
            Options::default(),
            Arc::new(CrashOnce(AtomicU64::new(0))),
        )
        .unwrap();
        let task = pool.task(RelPath::new("Solution.lean").unwrap()).unwrap();
        let mut attempt = task.attempt("test").unwrap();
        attempt.write(b"saved candidate").unwrap();
        let frozen = attempt.freeze().unwrap();
        artifact(&dir.join("candidate.lean"), frozen.bytes()).unwrap();
        let verified = verify_candidate(task, frozen, &dir).await.unwrap();
        assert_eq!(verified.report(), "checked on retry");
        let report = std::fs::read_to_string(dir.join("verification.txt")).unwrap();
        assert!(report.contains("deliberate crash") && report.contains("checked on retry"));
        assert_eq!(
            std::fs::read(dir.join("candidate.lean")).unwrap(),
            b"saved candidate"
        );
        drop(verified);
        drop(pool);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
