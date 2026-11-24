//! The member-token tool allowlist, local thread bindings, and revocable artifact grants.
use crate::{
    collab::{self, Member},
    core::{Core, CoreRef},
    memory::{self, Scope, View},
    server::{bad, ApiErr, Who, R},
    tools, util,
};
use anyhow::{anyhow, Result};
use axum::{
    body::{to_bytes, Bytes},
    extract::{Path, State},
    http::{Method, StatusCode},
    Extension, Json,
};
use serde_json::{json, Value};
use std::collections::HashMap;

fn field<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("").trim()
}

pub(crate) fn scope(s: &str) -> Result<Option<Scope>, ApiErr> {
    Scope::parse(s).map_err(|e| bad(e.to_string()))
}

pub(crate) fn member_memory(
    core: &CoreRef,
    m: &Member,
    scope: Option<Scope>,
    name: &str,
    query: &str,
) -> Result<Vec<Value>, ApiErr> {
    if scope == Some(Scope::Global) {
        m.need("read_global_memory")?;
    }
    Ok(collab::memory_scope(m)
        .only(scope)
        .rows(core, name, query)?)
}

pub(crate) fn save_member_memory(core: &CoreRef, m: &Member, args: &Value) -> RValue {
    let user = match scope(field(args, "scope"))? {
        Some(Scope::Global) => {
            return Err(denied("Only the owner can write the host's global memory."))
        }
        Some(Scope::Collaborator) => Some(m.user_id.as_str()),
        _ => {
            m.need("edit_memory")?;
            None
        }
    };
    let mut row = memory::write(
        core,
        Some(&m.project_id),
        user,
        field(args, "name"),
        field(args, "description"),
        args["body"].as_str().unwrap_or(""),
        &m.author(),
    )?;
    if let Some(row) = row.as_object_mut() {
        row.remove("body");
        row.remove("user_id");
        row.remove("deleted_at");
    }
    Ok(row)
}

type RValue = Result<Value, ApiErr>;

fn denied(message: &str) -> ApiErr {
    ApiErr(StatusCode::FORBIDDEN, message.into())
}

pub(crate) fn bind_row(core: &Core, thread: &Value) -> Result<Option<Value>> {
    Ok(core.db.one(
        "SELECT * FROM collab_agent_threads WHERE thread_id = ?1",
        &[&field(thread, "id")],
    )?)
}

pub(crate) async fn bind(
    State(core): State<CoreRef>,
    Extension(_who): Extension<Who>,
    Path((link, local)): Path<(String, String)>,
    Json(b): Json<Value>,
) -> R {
    let t = core.thread(&local)?;
    if !collab::runs_here(&t) {
        return Err(denied("Bind a thread that runs on this Model Roamer."));
    }
    let remote = b["host_thread_id"].as_str().filter(|id| !id.is_empty());
    forward(&core, &link, "check_binding", remote, &json!({})).await?;
    core.db.exec(
        "INSERT INTO collab_agent_threads(thread_id, link_id, host_thread_id) VALUES (?1, ?2, ?3)
         ON CONFLICT(thread_id) DO UPDATE SET link_id = excluded.link_id, host_thread_id = excluded.host_thread_id",
        &[&local, &link, &remote],
    )?;
    Ok(Json(
        json!({ "thread_id": local, "link_id": link, "host_thread_id": remote }),
    ))
}

pub(crate) async fn unbind(
    State(core): State<CoreRef>,
    Extension(_who): Extension<Who>,
    Path((link, local)): Path<(String, String)>,
) -> R {
    core.db.exec(
        "DELETE FROM collab_agent_threads WHERE thread_id = ?1 AND link_id = ?2",
        &[&local, &link],
    )?;
    Ok(Json(json!({ "ok": true })))
}

fn readonly(tool: &str) -> bool {
    matches!(
        tool,
        "check_binding"
            | "read_memory"
            | "list_threads"
            | "read_thread"
            | "list_artifacts"
            | "list_claims"
            | "check_status"
    )
}

pub(crate) fn shared_tool(tool: &str) -> bool {
    matches!(
        tool,
        "list_threads"
            | "read_thread"
            | "list_artifacts"
            | "publish_artifact"
            | "claim_paths"
            | "release_paths"
            | "list_claims"
            | "set_status"
            | "check_status"
    )
}

async fn forward(
    core: &CoreRef,
    link: &str,
    tool: &str,
    thread: Option<&str>,
    args: &Value,
) -> RValue {
    let body =
        Bytes::from(json!({ "tool": tool, "thread_id": thread, "arguments": args }).to_string());
    let response = collab::proxy_host(
        core,
        link,
        "mcp",
        Method::POST,
        &HashMap::new(),
        body,
        readonly(tool),
    )
    .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .map_err(|_| {
            ApiErr(
                StatusCode::BAD_GATEWAY,
                "The host tool response was too large.".into(),
            )
        })?;
    let result: Value = serde_json::from_slice(&bytes).map_err(|_| {
        ApiErr(
            StatusCode::BAD_GATEWAY,
            format!("The host returned an invalid tool response ({status})."),
        )
    })?;
    if !status.is_success() {
        return Err(ApiErr(
            status,
            result["error"]
                .as_str()
                .unwrap_or("The host refused that tool call.")
                .into(),
        ));
    }
    Ok(result)
}

pub(crate) async fn guest_tool(
    core: &CoreRef,
    binding: &Value,
    tool: &str,
    args: &Value,
) -> Result<String> {
    let link = field(binding, "link_id");
    let thread = binding["host_thread_id"].as_str();
    let request = |tool| forward(core, link, tool, thread, args);
    if tool == "read_memory" {
        let name = field(args, "name");
        let query = field(args, "query");
        let requested = Scope::parse(field(args, "scope"))?;
        let mut local = View::local(None).only(requested).rows(core, name, query)?;
        if matches!(requested, Some(Scope::Collaborator | Scope::Global))
            || (!name.is_empty() && local.first().is_some_and(|m| m["scope"] == "collaborator"))
        {
            return Ok(memory::render(&local, name, query));
        }
        let mut remote_args = args.clone();
        remote_args["scope"] = json!("project");
        match forward(core, link, tool, thread, &remote_args).await {
            Ok(rows) => {
                let shared = rows
                    .as_array()
                    .ok_or_else(|| anyhow!("The host returned invalid memory rows."))?;
                // Preserve personal > project > global precedence and local global entries during an outage.
                local.sort_by_key(|m| m["scope"] == "global");
                let index = local
                    .iter()
                    .position(|m| m["scope"] == "global")
                    .unwrap_or(local.len());
                local.splice(index..index, shared.iter().cloned());
                return Ok(memory::render(&local, name, query));
            }
            Err(e) if requested.is_none() && !local.is_empty() => {
                return Ok(format!(
                    "{}\n\nShared project memory unavailable. {}",
                    memory::render(&local, name, query),
                    e.1
                ));
            }
            Err(e) => return Err(anyhow!(e.1)),
        }
    }
    let mut result = request(tool).await.map_err(|e| anyhow!(e.1))?;
    if tool == "publish_artifact" || tool == "list_artifacts" {
        let row = core
            .db
            .one("SELECT host_url FROM collab_links WHERE id = ?1", &[&link])?
            .ok_or_else(|| anyhow!("No such project link."))?;
        let base = field(&row, "host_url");
        let add_base = |artifact: &mut Value| {
            if let Some(url) = artifact["url"]
                .as_str()
                .filter(|url| url.starts_with("/a/"))
            {
                artifact["url"] = json!(format!("{base}{url}"));
            }
        };
        if let Some(rows) = result.as_array_mut() {
            rows.iter_mut().for_each(add_base);
        } else {
            add_base(&mut result);
        }
    }
    Ok(result
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| result.to_string()))
}

fn owned_thread(core: &CoreRef, m: &Member, id: &str) -> RValue {
    if id.is_empty() {
        return Err(bad(
            "Bind this local thread to one of your threads on the host first.",
        ));
    }
    let thread = collab::visible_thread(core, m, id)?;
    if thread["user_id"].as_str() != Some(m.user_id.as_str()) {
        return Err(denied("You can change only your own threads."));
    }
    Ok(thread)
}

pub(crate) async fn host(
    State(core): State<CoreRef>,
    Extension(m): Extension<Member>,
    Json(b): Json<Value>,
) -> R {
    let tool = field(&b, "tool");
    let tid = field(&b, "thread_id");
    let args = &b["arguments"];
    if !args.is_object() && !args.is_null() {
        return Err(bad("Tool arguments must be an object."));
    }
    let result = match tool {
        "check_binding" => {
            if !tid.is_empty() {
                owned_thread(&core, &m, tid)?;
            }
            json!("Connected.")
        }
        "read_memory" => json!(member_memory(
            &core,
            &m,
            scope(field(args, "scope"))?,
            field(args, "name"),
            field(args, "query")
        )?),
        "write_memory" => {
            let saved = save_member_memory(&core, &m, args)?;
            json!(format!(
                "Saved memory '{}' ({}).",
                field(&saved, "name"),
                field(&saved, "scope")
            ))
        }
        "list_threads" | "check_status" => {
            let filter = field(args, "status");
            let rows = collab::threads_of(&core, &m, 0)?;
            let lines: Vec<_> = rows
                .iter()
                .filter(|r| match filter {
                    "" => true,
                    "open" => r["resolved"] == 0,
                    status => r["status"] == status,
                })
                .map(tools::thread_line)
                .collect();
            json!(if lines.is_empty() {
                "No visible threads.".into()
            } else {
                lines.join("\n")
            })
        }
        "read_thread" => {
            let before = args["before"].as_i64().unwrap_or(i64::MAX);
            let limit = args["limit"].as_i64().unwrap_or(12).clamp(1, 60);
            let history =
                collab::thread_history(&core, &m, field(args, "thread_id"), before, limit)?;
            json!(tools::format_history(
                &history["thread"],
                history["messages"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
            ))
        }
        "list_artifacts" => json!(collab::member_artifacts(&core, &m)?),
        "publish_artifact" => {
            if !tid.is_empty() {
                owned_thread(&core, &m, tid)?;
            }
            collab::publish_member_artifact(&core, &m, Some(tid).filter(|id| !id.is_empty()), args)?
        }
        "list_claims" => {
            let rows = tools::claim_rows(&core, &m.project_id)?;
            json!(tools::format_claims(rows.iter().filter(
                |r| collab::can_see(&m, r) && relative_path(field(r, "path"))
            )))
        }
        "claim_paths" | "release_paths" => {
            m.need("manage_claims")?;
            owned_thread(&core, &m, tid)?;
            if !args["paths"].is_null()
                && !args["paths"]
                    .as_array()
                    .is_some_and(|paths| paths.iter().all(Value::is_string))
            {
                return Err(bad("Paths must be a list of strings."));
            }
            let paths = tools::claim_paths(args);
            if paths.iter().any(|path| !relative_path(path))
                || (tool == "claim_paths" && paths.is_empty())
            {
                return Err(bad("Give paths relative to the shared repository."));
            }
            if tool == "release_paths" {
                tools::release_claims(&core, tid, &paths)?;
                json!("Released.")
            } else {
                let others = tools::claim_rows(&core, &m.project_id)?;
                let visible = others
                    .iter()
                    .filter(|r| collab::can_see(&m, r) && relative_path(field(r, "path")));
                json!(tools::record_claims(
                    &core,
                    &m.project_id,
                    tid,
                    &paths,
                    field(args, "note"),
                    visible
                )?)
            }
        }
        "set_status" => {
            m.need("update_thread_status")?;
            owned_thread(&core, &m, tid)?;
            let status = field(args, "status");
            tools::validate_status(status).map_err(|e| bad(e.to_string()))?;
            if status == "done" {
                tools::release_claims(&core, tid, &[])?;
            }
            core.set_status(tid, status, &util::clip(field(args, "note"), 200))?;
            json!(format!("Host thread status is '{status}'."))
        }
        _ => {
            return Err(denied(
                "That tool isn't available through the collaboration bridge.",
            ))
        }
    };
    Ok(Json(result))
}

pub(crate) fn relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with(['/', '\\'])
        && !path.contains(':')
        && !path.split(['/', '\\']).any(|part| part.trim() == "..")
}

fn grant_sig(core: &CoreRef, artifact: &str, membership: &str) -> String {
    util::sha256(&format!(
        "{}:member-artifact:{artifact}:{membership}",
        core.local_token
    ))[..24]
        .to_string()
}

pub(crate) fn artifact_link(core: &CoreRef, member: &Member, artifact: &str) -> String {
    format!(
        "/a/{artifact}/{}.{}/",
        member.id,
        grant_sig(core, artifact, &member.id)
    )
}

pub(crate) fn valid_artifact_grant(core: &CoreRef, artifact: &str, signed: &str) -> bool {
    let Some((membership, signature)) = signed.split_once('.') else {
        return false;
    };
    if !util::same(signature, &grant_sig(core, artifact, membership)) {
        return false;
    }
    core.db
        .one(
            "SELECT 1 FROM collab_members m JOIN collab_users u ON u.id = m.user_id
         JOIN projects p ON p.id = m.project_id JOIN artifacts a ON a.project_id = p.id
         WHERE m.id = ?1 AND a.id = ?2 AND m.revoked_at IS NULL AND u.revoked_at IS NULL
             AND p.deleted_at IS NULL AND a.deleted_at IS NULL",
            &[&membership, &artifact],
        )
        .ok()
        .flatten()
        .is_some()
}
