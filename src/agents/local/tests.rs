use super::*;
use axum::{
    body::Body, extract::State, http::HeaderMap, response::Response, routing::post, Json, Router,
};
use std::{
    collections::VecDeque,
    convert::Infallible,
    path::PathBuf,
    sync::{Arc, Mutex},
};

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn project(core: &CoreRef) {
    core.db.exec("INSERT INTO projects(id, name, created_at, updated_at) VALUES ('project', 'Test project', 1000, 1000)", &[]).unwrap();
    core.db.exec("INSERT INTO threads(id, project_id, kind, title, agent, created_at, updated_at) VALUES ('local-lead', 'project', 'lead', 'Local test lead', 'local', 1000, 1000)", &[]).unwrap();
    core.db.exec("INSERT INTO threads(id, project_id, kind, title, agent, created_at, updated_at) VALUES ('sample-thread', 'project', 'worker', 'Prepare sample fixture', 'codex', 1000, 1000)", &[]).unwrap();
}

fn fixture() -> (TempDir, CoreRef) {
    let dir = TempDir(std::env::temp_dir().join(crate::util::id("local-agent-test")));
    let core = Core::open(dir.0.clone(), true).unwrap();
    project(&core);
    (dir, core)
}

fn member(core: &CoreRef, user: &str, role: &str) -> (Member, Value) {
    let id = format!("member-{user}");
    let token = crate::util::secret();
    let hash = crate::util::sha256(&token);
    core.db
        .exec(
            "INSERT INTO collab_users(id, name, created_at) VALUES (?1, ?1, 1000)",
            &[&user],
        )
        .unwrap();
    core.db.exec("INSERT INTO collab_members(id, user_id, project_id, role, token_hash, created_at) VALUES (?1, ?2, 'project', ?3, ?4, 1000)", &[&id, &user, &role, &hash]).unwrap();
    let member = collab::identify_member(core, &token, "127.0.0.1").unwrap();
    let id = collab::personal_chat(core, &member).unwrap();
    (member, core.thread(&id).unwrap())
}

enum ResponseBody {
    Events(Vec<String>),
    Stall,
}

#[derive(Default)]
struct FakeState {
    replies: Mutex<VecDeque<ResponseBody>>,
    requests: Mutex<Vec<Value>>,
    received: tokio::sync::Notify,
}

struct Fake {
    endpoint: Endpoint,
    state: Arc<FakeState>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fake {
    async fn new(replies: Vec<ResponseBody>) -> Self {
        async fn respond(
            State(state): State<Arc<FakeState>>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> Response {
            assert_eq!(headers["Authorization"], "Bearer fake-key");
            assert_eq!(body["stream"], true);
            assert_eq!(body["model"], "fake-model");
            assert_eq!(body["cache_prompt"], false);
            state.requests.lock().unwrap().push(body);
            state.received.notify_one();
            let reply = state
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request");
            let stream = match reply {
                ResponseBody::Events(events) => {
                    let wire = events
                        .into_iter()
                        .map(|event| format!("data: {event}\r\n\r\n"))
                        .collect::<String>();
                    // Split in the middle of lines and UTF-8 code points.
                    let parts: Vec<_> = wire
                        .as_bytes()
                        .chunks(17)
                        .map(|part| Ok::<_, Infallible>(part.to_vec()))
                        .collect();
                    Body::from_stream(futures_util::stream::iter(parts))
                }
                ResponseBody::Stall => Body::from_stream(futures_util::stream::pending::<
                    std::result::Result<&'static str, Infallible>,
                >()),
            };
            Response::builder()
                .header("Content-Type", "text/event-stream")
                .body(stream)
                .unwrap()
        }
        let state = Arc::new(FakeState {
            replies: Mutex::new(replies.into()),
            ..Default::default()
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = Endpoint::for_test(format!("http://{}", listener.local_addr().unwrap()));
        let app = Router::new()
            .route("/v1/chat/completions", post(respond))
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            endpoint,
            state,
            task,
        }
    }
}

fn packet(delta: Value, finish: Option<&str>) -> String {
    json!({ "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] }).to_string()
}

fn call_response(name: &str, args: &str) -> ResponseBody {
    let (first, second) = args.split_at(args.floor_char_boundary(args.len() / 2));
    ResponseBody::Events(vec![
        packet(
            json!({ "role": "assistant", "reasoning_content": "Checking the project." }),
            None,
        ),
        packet(
            json!({ "tool_calls": [{ "index": 0, "id": "call_fixture", "type": "function", "function": { "name": name, "arguments": first } }] }),
            None,
        ),
        packet(
            json!({ "tool_calls": [{ "index": 0, "function": { "arguments": second } }] }),
            None,
        ),
        packet(json!({}), Some("tool_calls")),
        "[DONE]".into(),
    ])
}

fn answer() -> ResponseBody {
    ResponseBody::Events(vec![
        packet(json!({ "content": "Found one " }), None),
        packet(json!({ "content": "thread 🧵." }), None),
        packet(json!({}), Some("stop")),
        json!({ "choices": [], "usage": { "prompt_tokens": 340, "completion_tokens": 22 } })
            .to_string(),
        "[DONE]".into(),
    ])
}

async fn scripted_turn(
    core: &CoreRef,
    thread: &Value,
    member: Option<&Member>,
    fake: &Fake,
) -> (Result<String>, Vec<AgentEvent>) {
    let mut events = Vec::new();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        conversation(
            core,
            thread,
            member,
            fake.endpoint.clone(),
            vec![message("user", "List the project threads.")],
            None,
            &mut |event| events.push(event),
        ),
    )
    .await
    .unwrap();
    (result, events)
}

#[tokio::test]
async fn streams_sse_and_executes_a_native_tool() {
    let (_dir, core) = fixture();
    let thread = core.thread("local-lead").unwrap();
    let fake = Fake::new(vec![
        call_response("list_threads", r#"{"status":"open"}"#),
        answer(),
    ])
    .await;
    let (result, events) = scripted_turn(&core, &thread, None, &fake).await;
    assert_eq!(result.unwrap(), "Found one thread 🧵.");
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::Thinking(text) if text == "Checking the project.")));
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolUse { name, .. } if name == "list_threads")));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolResult { output, error: false, .. } if output.contains("sample-thread"))));
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::Delta(text) if text.contains('🧵'))));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Context {
            used: 362,
            window: 4096
        }
    )));
    let requests = fake.state.requests.lock().unwrap();
    assert_eq!(
        requests[0]["tools"][0]["function"]["parameters"]["additionalProperties"],
        false
    );
    assert!(requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["role"] == "tool" && m["tool_call_id"] == "call_fixture"));
}

#[tokio::test]
async fn repairs_once_without_executing_invalid_arguments() {
    let (_dir, core) = fixture();
    let thread = core.thread("local-lead").unwrap();
    let fake = Fake::new(vec![
        call_response(
            "write_memory",
            r#"{"name":"wrong","body":"bad","body":"duplicate"}"#,
        ),
        call_response("list_threads", "{}"),
        answer(),
    ])
    .await;
    let (result, events) = scripted_turn(&core, &thread, None, &fake).await;
    assert!(result.is_ok());
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Progress(text) if text.contains("Repairing")))
            .count(),
        1
    );
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolUse { name, .. } if name == "write_memory")));
    let calls = fake.state.requests.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert!(calls[1]["messages"]
        .to_string()
        .contains("invalid tool call"));
    assert!(!calls[2]["messages"]
        .to_string()
        .contains("invalid tool call"));

    let fake = Fake::new(vec![
        call_response("list_threads", r#"{"extra":1}"#),
        call_response("list_threads", "{"),
    ])
    .await;
    let (result, events) = scripted_turn(&core, &thread, None, &fake).await;
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("after one repair attempt"));
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolUse { .. })));
    assert_eq!(fake.state.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn scopes_shared_tools_and_rechecks_live_permissions() {
    let (_dir, core) = fixture();
    let (viewer, chat) = member(&core, "violet", "viewer");
    let (other, other_chat) = member(&core, "indigo", "collaborator");
    let fake = Fake::new(vec![
        call_response(
            "spawn_thread",
            r#"{"title":"Unexpected","instructions":"Do it"}"#,
        ),
        answer(),
    ])
    .await;
    let (result, events) = scripted_turn(&core, &chat, Some(&viewer), &fake).await;
    assert!(result.is_ok());
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolUse { .. })));
    let schemas = fake.state.requests.lock().unwrap()[0]["tools"].to_string();
    for denied in ["spawn_thread", "write_memory", "send_message", "ask_user"] {
        assert!(!schemas.contains(denied), "{denied}");
    }
    let read = tools::local_list(&core, &chat, Some(&viewer))
        .into_iter()
        .find(|tool| tool["function"]["name"] == "read_memory")
        .unwrap();
    assert_eq!(
        read["function"]["parameters"]["properties"]["scope"]["enum"],
        json!(["collaborator", "project"])
    );

    let caller = || tools::Caller::Member {
        thread: &chat,
        identity: &viewer,
    };
    let other_id = other_chat["id"].as_str().unwrap();
    crate::memory::write(
        &core,
        Some("project"),
        Some(&other.user_id),
        "private-note",
        "",
        "other user secret",
        "test",
    )
    .unwrap();
    assert!(!tools::call(
        &core,
        caller(),
        "read_memory",
        &json!({ "scope": "collaborator", "name": "private-note" })
    )
    .await
    .unwrap()
    .contains("other user secret"));
    assert!(tools::call(
        &core,
        caller(),
        "read_memory",
        &json!({ "scope": "global" })
    )
    .await
    .is_err());
    let perms = json!({ "view_others_chats": true, "create_threads": true }).to_string();
    core.db
        .exec(
            "UPDATE collab_members SET perms = ?1 WHERE id = ?2",
            &[&perms, &viewer.id],
        )
        .unwrap();
    assert!(tools::call(
        &core,
        caller(),
        "read_thread",
        &json!({ "thread_id": other_id })
    )
    .await
    .is_err());
    let draft = tools::call(&core, caller(), "spawn_thread", &json!({ "title": "Draft for review", "instructions": "Prepare a fixture", "agent": "codex" })).await.unwrap();
    assert!(draft.contains("paused"));
    let draft = core
        .db
        .one(
            "SELECT id, status FROM threads WHERE title = 'Draft for review'",
            &[],
        )
        .unwrap()
        .unwrap();
    assert_eq!(draft["status"], "paused");
    assert_eq!(
        core.db
            .scalar_i64(
                "SELECT count(*) FROM messages WHERE thread_id = ?1 AND pending != 0",
                &[&draft["id"].as_str().unwrap()]
            )
            .unwrap(),
        0
    );
    assert!(tools::call(
        &core,
        caller(),
        "send_message",
        &json!({ "thread_id": "sample-thread", "text": "Run this" })
    )
    .await
    .is_err());
    core.db
        .exec(
            "UPDATE collab_members SET perms = '{}' WHERE id = ?1",
            &[&viewer.id],
        )
        .unwrap();
    assert!(tools::call(
        &core,
        caller(),
        "spawn_thread",
        &json!({ "title": "Denied", "instructions": "Denied" })
    )
    .await
    .is_err());
    core.db
        .exec(
            "UPDATE collab_members SET revoked_at = 1 WHERE id = ?1",
            &[&viewer.id],
        )
        .unwrap();
    assert!(tools::call(&core, caller(), "list_threads", &json!({}))
        .await
        .is_err());
}

#[tokio::test]
async fn fair_queue_skips_a_cancelled_ticket_and_preserves_each_users_order() {
    let queue = Arc::new(Queue::default());
    let held = queue.enter("a", None).unwrap().acquire().await;
    let a2 = queue.enter("a", None).unwrap();
    let a3 = queue.enter("a", None).unwrap();
    let b1 = queue.enter("b", None).unwrap();
    let b2 = queue.enter("b", None).unwrap();
    let dropped = queue.enter("cancelled", None).unwrap();
    let c1 = queue.enter("c", None).unwrap();
    drop(dropped);
    drop(held);
    tokio::time::timeout(Duration::from_secs(2), async {
        for ticket in [b1, c1, a2, b2, a3] {
            drop(ticket.acquire().await);
        }
    })
    .await
    .expect("expected per-user round-robin order");
}

#[tokio::test]
async fn configurable_member_rate_limit_is_shared_across_requests() {
    let (_dir, core) = fixture();
    let (a, _) = member(&core, "violet", "collaborator");
    let (b, _) = member(&core, "indigo", "collaborator");
    core.patch_settings(&json!({ "agents": { "local": { "member_turn_limit": 2, "member_turn_window_secs": 60 } } })).unwrap();
    for _ in 0..2 {
        drop(acquire(&core, Some(&a), || {}).await.unwrap());
    }
    assert!(acquire(&core, Some(&a), || {})
        .await
        .err()
        .unwrap()
        .to_string()
        .contains("rate limit"));
    drop(acquire(&core, Some(&b), || {}).await.unwrap());
    drop(acquire(&core, None, || {}).await.unwrap());
}

#[tokio::test]
async fn cancellation_drops_an_active_sse_stream_and_releases_the_slot() {
    let (_dir, core) = fixture();
    let fake = Fake::new(vec![ResponseBody::Stall]).await;
    let endpoint = fake.endpoint.clone();
    let state = fake.state.clone();
    let (tx, mut rx) = watch::channel(false);
    let cloned = core.clone();
    let work = tokio::spawn(async move {
        cancellable(
            &mut rx,
            async {
                let _permit = acquire(&cloned, None, || {}).await?;
                let body = request(&endpoint, &[message("user", "Wait for cancellation")]);
                stream::receive(
                    &endpoint.base_url,
                    &endpoint.api_key,
                    endpoint.context,
                    &body,
                    &mut stream::Round::default(),
                    &mut |_| {},
                )
                .await
                .map_err(completion_error)
            },
            Duration::from_secs(5),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), state.received.notified())
        .await
        .unwrap();
    tx.send(true).unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(1), work)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err()
        .to_string()
        .contains("cancelled"));
    drop(
        tokio::time::timeout(Duration::from_secs(1), acquire(&core, None, || {}))
            .await
            .unwrap()
            .unwrap(),
    );
}

#[test]
fn history_has_a_bounded_window_and_a_thread_local_summary() {
    let (_dir, core) = fixture();
    for i in 0..12 {
        core.db.exec(
            "INSERT INTO messages(id, thread_id, role, author, body, created_at) VALUES (?1, 'local-lead', 'user', 'Test', ?1, ?2)",
            &[&format!("history-{i}"), &(i as i64)],
        ).unwrap();
    }
    crate::runner::post(
        &core,
        "sample-thread",
        "user",
        "Test",
        "private-other-thread-text",
        json!({}),
        false,
    )
    .unwrap();
    let messages = recent_context(&core, "local-lead", &["history-11".into()], 4096).unwrap();
    assert_eq!(messages.len(), HISTORY_WINDOW + 1);
    assert!(messages[0]["content"]
        .as_str()
        .unwrap()
        .contains("history-2"));
    assert_eq!(messages.last().unwrap()["content"], "history-10");
    let transcript = json!(messages).to_string();
    assert!(!transcript.contains("history-11"));
    assert!(!transcript.contains("private-other-thread-text"));
}

#[tokio::test]
async fn missing_engine_points_to_settings() {
    let (_dir, core) = fixture();
    assert_eq!(status(&core)["installed"], false);
    assert!(test_connection(&core)
        .await
        .unwrap_err()
        .to_string()
        .contains(SETTINGS));
}

#[tokio::test]
#[ignore = "requires a verified Qwen installation in an isolated data directory"]
async fn windows_qwen_runs_a_real_project_tool_and_plans() {
    let data =
        PathBuf::from(std::env::var_os("MR_LOCAL_SMOKE_DATA").expect("isolated data directory"));
    assert!(
        !data.join("model-roamer.db").exists(),
        "use a fresh database"
    );
    let core = Core::open(data, true).unwrap();
    project(&core);
    let result = async {
        ensure!(local_llm::installation(&core).is_some_and(|(id, _)| id == "qwen3-4b-instruct-2507"), "Qwen installation required");
        crate::runner::post(&core, "local-lead", "user", "Test", "Use the list_threads tool to list the current project's threads and tell me their exact titles.", json!({}), true)?;
        tokio::time::timeout(Duration::from_secs(130), async {
            loop {
                if !core.running.lock().unwrap().contains_key("local-lead") { break; }
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        }).await?;
        let turn = core.db.one("SELECT id, status, error FROM turns WHERE thread_id = 'local-lead' ORDER BY started_at DESC LIMIT 1", &[])?.context("No turn recorded")?;
        let output = core.db.all("SELECT kind, title, body FROM activity WHERE turn_id = ?1 ORDER BY id", &[&turn["id"].as_str().unwrap()])?;
        ensure!(turn["status"] == "done", "Real Qwen turn failed: {turn}; {output:?}");
        ensure!(output.iter().any(|r| r["kind"] == "tool" && r["title"] == "list_threads"), "Qwen did not call list_threads: {output:?}");
        let reply = core.db.one("SELECT body FROM messages WHERE thread_id = 'local-lead' AND role = 'agent' ORDER BY created_at DESC LIMIT 1", &[])?.context("No agent reply")?;
        ensure!(reply["body"].as_str().unwrap_or("").contains("Prepare sample fixture"), "Unexpected Qwen reply: {reply}");
        let state = local_llm::status(&core).await;
        println!("Qwen engine: {}", state["runtime"]);
        println!("Qwen tool reply: {}", reply["body"]);
        let (_tx, mut cancel) = watch::channel(false);
        let plan = plan_from_transcript(&core, "project", None, "Create one worker thread to document the local tool call. Use the local agent and its default model.", &mut cancel).await?;
        ensure!(plan.threads.len() == 1, "Unexpected Qwen plan: {plan:?}");
        println!("Qwen structured plan: {plan:?}");
        Ok::<_, anyhow::Error>(())
    }.await;
    let stopped = local_llm::api_stop(State(core)).await;
    result.unwrap();
    let _ = stopped.map_err(|e| anyhow!(e.1)).unwrap();
}
