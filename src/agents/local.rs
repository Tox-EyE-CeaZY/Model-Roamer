//! The in-process agent that shares this installation's llama-server.
mod queue;
mod stream;

pub(crate) use queue::{Permit, Queue};

use super::{AgentAdapter, AgentEvent, Info, Step, TurnSpec};
use crate::{
    collab::{self, Member},
    core::{Core, CoreRef},
    local_llm::{self, Endpoint},
    procs::Launch,
    tools,
};
use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::VecDeque, time::Duration};
use tokio::sync::watch;

const ID: &str = "local";
const SETTINGS: &str = "Settings → Local model";
const HISTORY_WINDOW: usize = 8;
const TOOL_ROUNDS: usize = 6;
pub(crate) const WRITE_MARKER: &str = "Local tool may have changed state";

pub struct Local;

impl AgentAdapter for Local {
    fn id(&self) -> String {
        ID.into()
    }

    fn info(&self) -> Info {
        Info {
            id: ID.into(),
            name: "Local Operator".into(),
            vendor: "Local".into(),
            hue: 159,
            strengths: ["plan", "orchestrate", "general"].map(String::from).into(),
            summary: "Runs on this machine for project coordination, planning, and chat.".into(),
            site: String::new(),
            models: local_llm::model_options(),
            mcp: true,
            can_lead: true,
        }
    }

    fn binaries(&self) -> Vec<String> {
        vec![]
    }
    fn launch(&self, _: &TurnSpec) -> Result<Launch> {
        bail!("Use the in-process local agent runner.")
    }
    fn parse(&self, _: &str, _: &mut Value) -> Vec<AgentEvent> {
        vec![]
    }
    fn install(&self) -> Vec<Step> {
        vec![]
    }
    fn login(&self, _: bool) -> Step {
        Step {
            label: format!("Open {SETTINGS}"),
            cmd: vec![],
            needs: vec![],
        }
    }
}

pub(crate) fn status(core: &Core) -> Value {
    let installation = local_llm::installation(core);
    let installed = installation.is_some();
    let setup = local_llm::setup_progress(core);
    let detail = if setup["running"] == true {
        setup_label(&setup)
    } else if installed {
        "Ready on this machine. No sign-in required.".into()
    } else {
        format!("Set up the local model in {SETTINGS}.")
    };
    json!({
        "id": ID, "info": Local.info(), "config": core.agent_cfg(ID), "installed": installed,
        "active_model": installation.as_ref().map(|(model, _)| model),
        "version": installation.map(|(_, tag)| tag),
        "auth": { "signed_in": if installed { "yes" } else { "no" }, "detail": detail },
        "checked_at": crate::util::now_ms(), "install": [], "login": null, "logout": null,
        "pty": false, "limit": core.limit_of(ID), "setup": setup
    })
}

fn setup_label(setup: &Value) -> String {
    let stage = setup["stage"].as_str().unwrap_or("");
    let stage = if stage.is_empty() {
        "Preparing local model"
    } else {
        stage
    };
    if setup["total"].as_u64().unwrap_or(0) > 0 {
        format!("{stage} ({}%)", setup["pct"].as_u64().unwrap_or(0))
    } else {
        stage.into()
    }
}

pub(crate) async fn acquire(
    core: &CoreRef,
    member: Option<&Member>,
    queued: impl FnOnce(),
) -> Result<Permit> {
    let config = core.agent_cfg(ID);
    ensure!(
        config["enabled"] != false,
        "The Local Operator is turned off in Agents."
    );
    let rate = member.map(|member| {
        (
            member.id.as_str(),
            config["member_turn_limit"]
                .as_u64()
                .unwrap_or(15)
                .min(10_000) as usize,
            Duration::from_secs(
                config["member_turn_window_secs"]
                    .as_u64()
                    .unwrap_or(600)
                    .clamp(1, 86_400),
            ),
        )
    });
    let ticket = local_llm::turn_queue(core).enter(member.map_or("", |m| &m.user_id), rate)?;
    if ticket.queued {
        queued();
    }
    Ok(ticket.acquire().await)
}

async fn engine(
    core: &CoreRef,
    model: &str,
    shared: bool,
    emit: &mut impl FnMut(AgentEvent),
) -> Result<Endpoint> {
    ensure!(
        core.agent_cfg(ID)["enabled"] != false,
        "The Local Operator is turned off in Agents."
    );
    let mut last_progress = String::new();
    loop {
        let progress = local_llm::setup_progress(core);
        if progress["running"] != true {
            break;
        }
        let label = setup_label(&progress);
        if label != last_progress {
            emit(AgentEvent::Progress(label.clone()));
            last_progress = label;
        }
        tokio::time::sleep(Duration::from_millis(350)).await;
    }
    let Some((active, _)) = local_llm::installation(core) else {
        bail!("The local model isn't installed. Download it in {SETTINGS}, then retry.");
    };
    if !model.is_empty() && model != active {
        bail!("This thread requests {model}, but {active} is selected. Select the requested model in {SETTINGS} or change the thread's model.");
    }
    emit(AgentEvent::Progress("Connecting to the local model".into()));
    let endpoint = local_llm::ensure_running(core).await.map_err(|error| {
        let help = format!("The local model could not start. Open {SETTINGS} to check its status.");
        if shared {
            tracing::warn!(%error, "Shared local model startup failed");
            anyhow!(help)
        } else {
            anyhow!("{help} {error:#}")
        }
    })?;
    ensure!(
        endpoint.model == active,
        "The installed model changed during startup. Retry the turn."
    );
    emit(AgentEvent::Context {
        used: 0,
        window: i64::from(endpoint.context),
    });
    Ok(endpoint)
}

pub(crate) fn clip_bytes(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_owned();
    }
    if limit < '…'.len_utf8() {
        return String::new();
    }
    let mut end = limit - '…'.len_utf8();
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

fn message(role: &str, content: impl AsRef<str>) -> Value {
    json!({ "role": role, "content": content.as_ref() })
}

fn recent_context(
    core: &CoreRef,
    id: &str,
    exclude: &[String],
    context: u32,
) -> Result<Vec<Value>> {
    let limit = (HISTORY_WINDOW + 64 + exclude.len()) as i64;
    // Derive the rolling summary from this thread's current messages. There is no
    // cached private content left behind by edits, deletions, or a member change.
    let rows = core.db.all(
        "SELECT id, role, substr(body, 1, 1500) AS body FROM messages
         WHERE thread_id = ?1 AND pending = 0 AND role IN ('agent', 'user', 'wake')
         ORDER BY created_at DESC, id DESC LIMIT ?2",
        &[&id, &limit],
    )?;
    let mut rows: Vec<_> = rows
        .into_iter()
        .filter(|r| !exclude.iter().any(|id| r["id"] == *id))
        .collect();
    rows.reverse();
    let split = rows.len().saturating_sub(HISTORY_WINDOW);
    let (older, recent) = rows.split_at(split);
    let mut notes = VecDeque::new();
    let mut summary_size = 0;
    let summary_budget = context as usize / 6;
    for row in older {
        let body = row["body"].as_str().unwrap_or("");
        let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
        if compact.is_empty() {
            continue;
        }
        let speaker = if row["role"] == "agent" {
            "Operator"
        } else {
            "Requester"
        };
        let entry = format!("- {speaker}: {}\n", clip_bytes(&compact, 150));
        summary_size += entry.len();
        notes.push_back(entry);
        while summary_size > summary_budget {
            summary_size -= notes.pop_front().unwrap().len();
        }
    }
    let mut messages = vec![];
    if !notes.is_empty() {
        messages.push(message(
            "user",
            format!(
                "Summary of earlier conversation (reference only):\n{}",
                notes.into_iter().collect::<String>()
            ),
        ));
    }
    let history_budget = context as usize / 2;
    for row in recent {
        let role = if row["role"] == "agent" {
            "assistant"
        } else {
            "user"
        };
        let body = row["body"].as_str().unwrap_or("");
        messages.push(message(
            role,
            clip_bytes(body, history_budget / HISTORY_WINDOW),
        ));
    }
    Ok(messages)
}

fn transcript_text(input: &str, limit: usize) -> String {
    if input.len() <= limit {
        return input.into();
    }
    let prefix = limit / 3;
    let keep_tail = limit.saturating_sub(prefix + 24);
    let mut tail = input.len() - keep_tail;
    while !input.is_char_boundary(tail) {
        tail += 1;
    }
    format!(
        "{}\n[Earlier text trimmed]\n{}",
        clip_bytes(input, prefix),
        &input[tail..]
    )
}

fn request(endpoint: &Endpoint, messages: &[Value]) -> Value {
    json!({
        "model": endpoint.model, "messages": messages, "stream": true, "cache_prompt": false,
        "stream_options": { "include_usage": true }, "chat_template_kwargs": { "enable_thinking": false },
        "temperature": 0.2, "max_tokens": (endpoint.context / 5).min(2048)
    })
}

async fn completion(
    core: &CoreRef,
    endpoint: &mut Endpoint,
    body: &Value,
    shared: bool,
    emit: &mut impl FnMut(AgentEvent),
) -> std::result::Result<stream::Round, stream::Error> {
    let mut round = stream::Round::default();
    match stream::receive(
        &endpoint.base_url,
        &endpoint.api_key,
        endpoint.context,
        body,
        &mut round,
        emit,
    )
    .await
    {
        Ok(()) => Ok(round),
        Err(stream::Error::Transport(_)) if endpoint.was_stopped() => Err(stream::Error::Protocol(
            anyhow!("The local model was stopped in Settings."),
        )),
        Err(stream::Error::Transport(_)) if !round.displayed => {
            emit(AgentEvent::Progress(
                "Reconnecting to the local model".into(),
            ));
            *endpoint = engine(core, &endpoint.model, shared, emit)
                .await
                .map_err(stream::Error::Protocol)?;
            let mut round = stream::Round::default();
            stream::receive(
                &endpoint.base_url,
                &endpoint.api_key,
                endpoint.context,
                body,
                &mut round,
                emit,
            )
            .await?;
            Ok(round)
        }
        Err(error) => Err(error),
    }
}

async fn conversation(
    core: &CoreRef,
    thread: &Value,
    member: Option<&Member>,
    mut endpoint: Endpoint,
    base: Vec<Value>,
    turn_id: Option<&str>,
    emit: &mut impl FnMut(AgentEvent),
) -> Result<String> {
    let mut rounds = VecDeque::<Vec<Value>>::new();
    let mut repair = None;
    let mut repaired = false;
    let mut executed = 0;
    loop {
        let member = member
            .map(|m| collab::refresh_member(core, m.clone()).map_err(|e| anyhow!(e.1)))
            .transpose()?;
        let tools = tools::local_list(core, thread, member.as_ref());
        let mut messages = base.clone();
        messages.extend(rounds.iter().flatten().cloned());
        if let Some(instruction) = repair.take() {
            messages.push(message("user", instruction));
        }
        if executed == TOOL_ROUNDS {
            messages.push(message("user", "The tool budget for this turn is used. Answer using the results already available."));
        }
        let mut body = request(&endpoint, &messages);
        // With --jinja, llama.cpp's native tool parser uses the supplied schemas
        // for its model-specific grammar when supported.
        if !tools.is_empty() && executed < TOOL_ROUNDS {
            body["tools"] = json!(tools);
            body["tool_choice"] = json!("auto");
            body["parallel_tool_calls"] = json!(false);
        }
        let reply = completion(core, &mut endpoint, &body, member.is_some(), emit).await;
        let calls = match reply {
            Ok(round) => match round.calls(if executed < TOOL_ROUNDS { &tools } else { &[] }) {
                Ok(calls) if calls.is_empty() => {
                    ensure!(
                        round.finish == "stop",
                        "The local model stopped before finishing its reply."
                    );
                    ensure!(
                        !round.text.trim().is_empty(),
                        "The local model returned an empty reply."
                    );
                    return Ok(round.text);
                }
                Ok(calls) => (round.text, calls),
                Err(error) => {
                    repair = Some(repair_request(&mut repaired, error, emit)?);
                    continue;
                }
            },
            Err(stream::Error::Tool(error)) => {
                repair = Some(repair_request(&mut repaired, error, emit)?);
                continue;
            }
            Err(stream::Error::Transport(error)) => bail!("{error}. Check {SETTINGS} and retry."),
            Err(stream::Error::Protocol(error)) => return Err(error),
        };
        let (text, calls) = calls;
        executed += 1;
        let mut exchange = vec![json!({
            "role": "assistant", "content": clip_bytes(&text, endpoint.context as usize / 8),
            "tool_calls": calls.iter().map(stream::Call::message).collect::<Vec<_>>(),
        })];
        for call in calls {
            if let Some(turn_id) = turn_id.filter(|_| tools::local_writes(&call.name)) {
                core.db.exec(
                    "UPDATE turns SET error = ?1 WHERE id = ?2",
                    &[&WRITE_MARKER, &turn_id],
                )?;
            }
            emit(AgentEvent::ToolUse {
                id: call.id.clone(),
                name: call.name.clone(),
                input: call.args.to_string(),
            });
            let caller = match &member {
                Some(identity) => tools::Caller::Member { thread, identity },
                None => tools::Caller::Thread(thread),
            };
            // A timeout or cancellation drops the current call. Never retry a
            // write automatically because it may have already taken effect.
            let result = tokio::time::timeout(
                Duration::from_secs(60),
                tools::call(core, caller, &call.name, &call.args),
            )
            .await;
            let (output, error) = match result {
                Ok(Ok(output)) => (output, false),
                Ok(Err(error)) => (format!("Error: {error}"), true),
                Err(_) => (
                    "The tool timed out. Check its state before trying again.".into(),
                    true,
                ),
            };
            let output = clip_bytes(&output, endpoint.context as usize / 5);
            emit(AgentEvent::ToolResult {
                id: call.id.clone(),
                output: output.clone(),
                error,
            });
            exchange.push(json!({ "role": "tool", "tool_call_id": call.id, "content": output }));
        }
        rounds.push_back(exchange);
        while rounds.len() > 1
            && rounds
                .iter()
                .flatten()
                .map(|m| m.to_string().len())
                .sum::<usize>()
                > endpoint.context as usize / 2
        {
            rounds.pop_front();
        }
    }
}

fn repair_request(
    repaired: &mut bool,
    error: anyhow::Error,
    emit: &mut impl FnMut(AgentEvent),
) -> Result<String> {
    let reason = clip_bytes(&error.to_string(), 160);
    ensure!(
        !*repaired,
        "The local model sent an invalid tool call again after one repair attempt: {reason}."
    );
    *repaired = true;
    emit(AgentEvent::Progress(format!(
        "Repairing an invalid tool call: {reason}"
    )));
    Ok(format!("The previous response contained an invalid tool call ({reason}). Retry using an available tool and its exact JSON arguments, or answer the original request directly. The invalid call was discarded."))
}

async fn run_turn(
    core: &CoreRef,
    thread: &Value,
    member: Option<&Member>,
    model: &str,
    input: &str,
    exclude: &[String],
    turn_id: Option<&str>,
    mut emit: impl FnMut(AgentEvent),
) -> Result<String> {
    let member = member
        .map(|m| collab::refresh_member(core, m.clone()).map_err(|e| anyhow!(e.1)))
        .transpose()?;
    let endpoint = engine(core, model, member.is_some(), &mut emit).await?;
    let id = thread["id"].as_str().context("Missing local thread ID")?;
    let thread = core.thread(id)?;
    if let Some(m) = &member {
        ensure!(
            thread["user_id"] == m.user_id
                && thread["project_id"] == m.project_id
                && crate::collab_chat::is_personal_chat(&thread),
            "The shared local model needs your own project chat."
        );
    } else {
        ensure!(
            collab::runs_here(&thread) && thread["agent"] == ID,
            "This thread is no longer assigned to the Local Operator."
        );
    }
    let project = thread["project_id"]
        .as_str()
        .map(|p| core.project(p))
        .transpose()?;
    let mut base = vec![message(
        "system",
        crate::prompts::local(&thread, project.as_ref(), endpoint.context),
    )];
    base.extend(recent_context(core, id, exclude, endpoint.context)?);
    base.push(message(
        "user",
        transcript_text(input, endpoint.context as usize * 3 / 4),
    ));
    let text = conversation(
        core,
        &thread,
        member.as_ref(),
        endpoint,
        base,
        turn_id,
        &mut emit,
    )
    .await?;
    if let Some(m) = member {
        collab::refresh_member(core, m).map_err(|e| anyhow!(e.1))?;
    }
    emit(AgentEvent::Final(text.clone()));
    Ok(text)
}

pub(crate) async fn run_owner_turn(
    core: &CoreRef,
    thread: &Value,
    model: &str,
    input: &str,
    exclude: &[String],
    turn_id: &str,
    emit: impl FnMut(AgentEvent),
) -> Result<String> {
    run_turn(
        core,
        thread,
        None,
        model,
        input,
        exclude,
        Some(turn_id),
        emit,
    )
    .await
}

async fn cancellable<T>(
    cancel: &mut watch::Receiver<bool>,
    task: impl std::future::Future<Output = Result<T>>,
    timeout: Duration,
) -> Result<T> {
    tokio::select! {
        biased;
        _ = cancel.wait_for(|cancelled| *cancelled) => bail!("The local model turn was cancelled."),
        result = tokio::time::timeout(timeout, task) => result.context("The local model turn timed out.")?,
    }
}

/// Stage 3 can stream into the requesting member's private project chat.
/// The caller supplies an authenticated member and only emits events to that member.
#[allow(dead_code)] // Called by the Stage 3 operator interface.
pub(crate) async fn run_member_turn(
    core: &CoreRef,
    member: &Member,
    thread_id: &str,
    input: &str,
    cancel: &mut watch::Receiver<bool>,
    mut emit: impl FnMut(AgentEvent),
) -> Result<String> {
    let member = collab::refresh_member(core, member.clone()).map_err(|e| anyhow!(e.1))?;
    let thread = collab::visible_thread(core, &member, thread_id).map_err(|e| anyhow!(e.1))?;
    ensure!(
        thread["user_id"] == member.user_id && crate::collab_chat::is_personal_chat(&thread),
        "The shared local model needs your own project chat."
    );
    cancellable(
        cancel,
        async {
            let _slot = acquire(core, Some(&member), || {
                emit(AgentEvent::Progress(
                    "Waiting for the shared local model".into(),
                ))
            })
            .await?;
            run_turn(core, &thread, Some(&member), "", input, &[], None, emit).await
        },
        Duration::from_secs(300),
    )
    .await
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThreadPlan {
    pub threads: Vec<PlannedThread>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlannedThread {
    pub title: String,
    pub prompt: String,
    pub agent: String,
    pub model: String,
}

fn plan_access(core: &CoreRef, project: &str, member: Option<&Member>) -> Result<()> {
    if let Some(member) = member {
        let member = collab::refresh_member(core, member.clone()).map_err(|e| anyhow!(e.1))?;
        ensure!(
            member.project_id == project,
            "That project isn't available to this member."
        );
        member.need("create_threads").map_err(|e| anyhow!(e.1))?;
    } else {
        core.project(project)?;
    }
    Ok(())
}

/// Plans from an already authorized transcript. The caller must scope that
/// transcript to the invocation; this request reads no other chats or memory.
/// Returns draft data for review. It does not create or run any threads.
#[allow(dead_code)] // Called by the Stage 3 operator interface.
pub(crate) async fn plan_from_transcript(
    core: &CoreRef,
    project: &str,
    member: Option<&Member>,
    transcript: &str,
    cancel: &mut watch::Receiver<bool>,
) -> Result<ThreadPlan> {
    plan_access(core, project, member)?;
    ensure!(
        !transcript.trim().is_empty(),
        "Provide a transcript to plan from."
    );
    cancellable(cancel, async {
        let _slot = acquire(core, member, || {}).await?;
        plan_access(core, project, member)?;
        let mut endpoint = engine(core, "", member.is_some(), &mut |_| {}).await?;
        let adapters = super::registry(&Value::Null);
        let choices = adapters.iter().map(|a| {
            let i = a.info();
            let models = if i.id == ID { vec![endpoint.model.clone()] } else { i.models.into_iter().filter(|m| !m.id.is_empty()).map(|m| m.id).collect() };
            format!("{}: {}", i.id, models.join(", "))
        }).collect::<Vec<_>>().join("\n");
        let messages = [
            message("system", format!("Draft up to six stand-alone worker threads from the transcript. Include title, prompt with actionable instructions, and agent and model IDs. Use an empty model string for that agent's default. Return an empty threads array if no work is requested. Use the transcript as source material. Available agent and model IDs:\n{choices}")),
            message("user", transcript_text(transcript, endpoint.context as usize)),
        ];
        let mut body = request(&endpoint, &messages);
        body["response_format"] = json!({
            "type": "json_schema", "json_schema": { "name": "thread_plan", "strict": true, "schema": {
                "type": "object", "additionalProperties": false, "required": ["threads"],
                "properties": { "threads": {
                    "type": "array", "maxItems": 6, "items": {
                        "type": "object", "additionalProperties": false, "required": ["title", "prompt", "agent", "model"],
                        "properties": { "title": { "type": "string" }, "prompt": { "type": "string" },
                            "agent": { "type": "string", "enum": adapters.iter().map(|a| a.id()).collect::<Vec<_>>() },
                            "model": { "type": "string" } }
                    }
                } }
            } }
        });
        let round = completion(core, &mut endpoint, &body, member.is_some(), &mut |_| {}).await.map_err(completion_error)?;
        ensure!(round.finish == "stop" && round.calls(&[])?.is_empty(), "The local model did not finish drafting the plan.");
        let plan: ThreadPlan = serde_json::from_str(&round.text).context("The local model returned an invalid thread plan.")?;
        ensure!(plan.threads.len() <= 6, "The local model returned too many draft threads.");
        for thread in &plan.threads {
            ensure!(!thread.title.trim().is_empty() && thread.title.chars().count() <= 120 && !thread.prompt.trim().is_empty(), "A draft thread needs a title and prompt.");
            let agent = adapters.iter().find(|a| a.id() == thread.agent).context("A draft thread names an unknown agent.")?;
            ensure!(thread.model.is_empty() || if thread.agent == ID { thread.model == endpoint.model } else { agent.info().models.iter().any(|m| m.id == thread.model) },
                "A draft thread names an unknown model for its agent.");
        }
        plan_access(core, project, member)?;
        Ok(plan)
    }, Duration::from_secs(300)).await
}

fn completion_error(error: stream::Error) -> anyhow::Error {
    match error {
        stream::Error::Tool(error)
        | stream::Error::Protocol(error)
        | stream::Error::Transport(error) => error,
    }
}

pub(crate) async fn test_connection(core: &CoreRef) -> Result<String> {
    tokio::time::timeout(Duration::from_secs(150), async {
        let _slot = acquire(core, None, || {}).await?;
        let mut endpoint = engine(core, "", false, &mut |_| {}).await?;
        let body = request(
            &endpoint,
            &[message("user", "Reply with exactly the words ROAMER READY")],
        );
        let round = completion(core, &mut endpoint, &body, false, &mut |_| {})
            .await
            .map_err(completion_error)?;
        ensure!(
            round.finish == "stop" && round.calls(&[])?.is_empty(),
            "The local model did not finish its test reply."
        );
        Ok(round.text)
    })
    .await
    .context("No local model answer within 150 seconds.")?
}

#[cfg(test)]
mod tests;
