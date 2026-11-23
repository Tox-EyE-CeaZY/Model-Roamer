//! The instructions every agent gets: what Model Roamer is, its role (lead, worker, reviewer, chat), its harness's own
//! tools, Model Roamer's tools, the project and its memory, and (for the lead) the fleet it can route work to.

use crate::{core::Core, memory, tools};
use serde_json::Value;

const BASE: &str = include_str!("../prompts/base.md");
const LEAD: &str = include_str!("../prompts/lead.md");
const WORKER: &str = include_str!("../prompts/worker.md");
const REVIEW: &str = include_str!("../prompts/review.md");
const CHAT: &str = include_str!("../prompts/chat.md");
const HARNESS: &str = include_str!("../prompts/harness.md");

/// The section of harness.md for one agent (custom agents share the generic one).
fn harness(agent: &str) -> String {
    let key = match agent {
        "claude" | "codex" | "antigravity" => agent,
        _ => "custom",
    };
    // a Windows checkout may have CRLF line endings; the section markers below assume LF
    let text = HARNESS.replace("\r\n", "\n");
    let marker = format!("## @{key}\n");
    let Some(start) = text.find(&marker) else { return String::new() };
    let rest = &text[start + marker.len()..];
    let end = rest.find("\n## @").map(|i| i + 1).unwrap_or(rest.len());
    rest[..end].trim().to_string()
}

/// What the lead knows about each agent: strengths, sign-in, headroom, what's running.
/// For a thread run by a buddy pair: who the other half is, what each of them is for, and how the turns pass.
/// Files other threads have claimed right now, so a thread knows before it starts what to leave alone.
fn claims_section(core: &Core, t: &Value, project: Option<&Value>) -> String {
    let (Some(pid), Some(tid)) = (project.and_then(|p| p["id"].as_str()), t["id"].as_str()) else { return String::new() };
    let rows = core
        .db
        .all("SELECT c.path, t.title, t.agent FROM claims c JOIN threads t ON t.id = c.thread_id WHERE c.project_id = ?1 AND c.thread_id != ?2 AND t.resolved = 0 AND t.status NOT IN ('done','cancelled','failed') ORDER BY c.created_at LIMIT 40", &[&pid, &tid])
        .unwrap_or_default();
    if rows.is_empty() {
        return String::new();
    }
    let list: Vec<String> = rows.iter().map(|r| format!("- `{}`: {} ({})", r["path"].as_str().unwrap_or(""), r["title"].as_str().unwrap_or(""), r["agent"].as_str().unwrap_or(""))).collect();
    format!("\n\n## Files other threads have claimed right now\n\nAnother agent is working in these. Don't edit them. If your task needs one, `message_lead` to ask for the work to be sequenced, and keep going on the rest. (`list_claims` shows the live picture.)\n\n{}\n", list.join("\n"))
}

fn buddy_section(core: &Core, t: &Value) -> String {
    let b = &t["buddy"];
    let Some(def) = b["id"].as_str().and_then(|id| core.buddy_def(id)) else { return String::new() };
    let side = if b["side"] == "b" { "b" } else { "a" };
    let other = if side == "a" { "b" } else { "a" };
    let name = |s: &str| def[s]["agent"].as_str().and_then(|a| core.adapter(a)).map(|a| a.info().name).unwrap_or_default();
    format!(
        "\n\n## You and your buddy\n\nThis thread is run by a pair of agents, **{}**, taking turns in the same folder. You are **{}**; your buddy is **{}**.\n\n{}\n\n**Your role:** {}\n\n**Your buddy's role:** {}\n\nHow it works: your reply at the end of your turn goes to your buddy as their next message, and theirs comes back to you. Make it a hand-over: what you did, the state of the files, what you want them to do or decide next. Don't repeat their work; build on it. When the work is complete and needs nothing more from your buddy, end your reply with `{}`: the pair stops there. The pair gets {} turns in all ({} used so far); the person may write to it at any time.\n",
        def["name"].as_str().unwrap_or("Buddies"),
        name(side),
        name(other),
        def["instructions"].as_str().unwrap_or(""),
        def[side]["role"].as_str().unwrap_or(""),
        def[other]["role"].as_str().unwrap_or(""),
        crate::runner::BUDDY_DONE,
        def["max_turns"].as_i64().unwrap_or(4).clamp(2, 20),
        b["turns"].as_i64().unwrap_or(0),
    )
}

pub fn fleet(core: &Core) -> String {
    let mut out = String::from("## The fleet you can route work to\n\n");
    let running = core.running.lock().map(|r| r.len()).unwrap_or(0);
    let routing = core.settings()["routing"].clone();
    for st in core.cached_status() {
        let id = st["id"].as_str().unwrap_or("");
        let cfg = &st["config"];
        if cfg["enabled"] == false {
            continue;
        }
        let info = &st["info"];
        let state = if st["installed"] == false {
            "not installed".to_string()
        } else if st["auth"]["signed_in"] == "no" {
            "not signed in".to_string()
        } else if st["limit"]["active"] == true {
            format!("out of usage for now ({})", st["limit"]["note"].as_str().unwrap_or("limit reached"))
        } else if st["pending"] == true {
            "not checked yet".to_string()
        } else {
            "ready".to_string()
        };
        let busy = core.db.scalar_i64("SELECT COUNT(*) FROM threads WHERE agent = ?1 AND status IN ('working','queued')", &[&id]).unwrap_or(0);
        let kinds: Vec<String> = routing.as_object().map(|o| o.iter().filter(|(_, v)| v.as_str() == Some(id)).map(|(k, _)| k.clone()).collect()).unwrap_or_default();
        out.push_str(&format!(
            "- **{}** (`{}`, {}): {} Strengths: {}. Routed here by default: {}. Status: {}; {} running now, up to {} at once.\n",
            info["name"].as_str().unwrap_or(id),
            id,
            info["vendor"].as_str().unwrap_or(""),
            info["summary"].as_str().unwrap_or(""),
            info["strengths"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
            if kinds.is_empty() { "nothing".into() } else { kinds.join(", ") },
            state,
            busy,
            cfg["max_parallel"].as_i64().unwrap_or(2),
        ));
    }
    out.push_str(&format!("\n{running} turn(s) are running across all projects right now. Prefer agents that are ready and have headroom; never route to one that is out of usage unless asked.\n"));
    out
}

pub struct Ctx<'a> {
    pub thread: &'a Value,
    pub project: Option<&'a Value>,
    pub workspace: String,
    pub mcp: bool,
    pub target: Option<&'a Value>,
}

pub fn system(core: &Core, c: &Ctx) -> String {
    let t = c.thread;
    let role = tools::role_of(t);
    let agent = t["agent"].as_str().unwrap_or("");
    let pname = c.project.and_then(|p| p["name"].as_str()).unwrap_or("");
    let role_line = match role {
        "lead" => "You are the **lead** of this project.",
        "worker" => "You are a **worker** on one task, reporting to the project's lead.",
        "review" => "You are a **reviewer** asked for a second opinion.",
        _ => "You are in a **direct chat** with the person.",
    };
    let ws_line = if c.workspace.is_empty() {
        String::new()
    } else {
        format!("Your workspace is `{}`{}.", c.workspace, if t["branch"].as_str().unwrap_or("").is_empty() { String::new() } else { format!(" on branch `{}` (a git worktree of its own, branched from `{}`)", t["branch"].as_str().unwrap_or(""), t["base_branch"].as_str().unwrap_or("")) })
    };
    let task = format!(
        "{}{}",
        t["instructions"].as_str().unwrap_or(""),
        t["criteria"].as_str().filter(|s| !s.trim().is_empty()).map(|s| format!("\n\n### Acceptance criteria\n\n{s}")).unwrap_or_default()
    );
    let mut s = BASE.replace("{{ROLE_LINE}}", role_line);
    s.push_str("\n\n");
    let body = match role {
        "lead" => LEAD.replace("{{FLEET}}", &fleet(core)),
        "worker" => WORKER.replace("{{TITLE}}", t["title"].as_str().unwrap_or("")).replace("{{WORKSPACE_LINE}}", &ws_line).replace("{{TASK}}", &task),
        "review" => REVIEW
            .replace("{{TARGET_TITLE}}", c.target.and_then(|x| x["title"].as_str()).unwrap_or(""))
            .replace("{{TARGET_AGENT}}", c.target.and_then(|x| x["agent"].as_str()).unwrap_or(""))
            .replace("{{TASK}}", &task),
        _ => CHAT.replace("{{WORKSPACE_LINE}}", &ws_line),
    };
    s.push_str(&body.replace("{{PROJECT}}", pname));
    s.push_str(&buddy_section(core, t));
    s.push_str(&claims_section(core, t, c.project));
    s.push_str("\n\n");
    s.push_str(&harness(agent));
    s.push_str("\n\n## Model Roamer tools\n\n");
    s.push_str(&tools::describe(role));
    if !c.mcp || agent == "antigravity" {
        s.push('\n');
        s.push_str(tools::TEXT_PROTOCOL);
    }
    if let Some(p) = c.project {
        s.push_str(&format!("\n\n## The project: {}\n\n", pname));
        if role == "lead" && !c.workspace.is_empty() {
            s.push_str(&format!("Project folder: `{}`.\n", c.workspace));
        }
        let repo = p["repo_url"].as_str().unwrap_or("");
        if !repo.is_empty() {
            s.push_str(&format!("Repository: {repo}. Base branch: `{}`.\n", p["base_branch"].as_str().unwrap_or("main")));
        }
        let instr = p["instructions"].as_str().unwrap_or("").trim();
        if !instr.is_empty() {
            s.push_str(&format!("\n### The person's instructions for this project (follow them)\n\n{instr}\n"));
        }
    }
    let pid = c.project.and_then(|p| p["id"].as_str());
    s.push_str(&format!("\n### Memory index\n\n{}\n", memory::index(core, pid)));
    // the design note travels with every agent so the suite stays one family
    if let Ok(Some(d)) = memory::get_by_name(core, pid, "design-language") {
        s.push_str(&format!("\n### Design language (from memory)\n\n{}\n", d["body"].as_str().unwrap_or("")));
    }
    s.push_str(&format!(
        "\n### About this session\n\nYou are `{}` in thread `{}` (\"{}\"). Today is {}. The person's machine runs {}.\n",
        agent,
        t["id"].as_str().unwrap_or(""),
        t["title"].as_str().unwrap_or(""),
        chrono::Local::now().format("%A %Y-%m-%d %H:%M %Z"),
        crate::os::os_name()
    ));
    if role != "lead" && t["title_by"].as_str().unwrap_or("auto") == "auto" {
        s.push_str("\n**Name this thread first.** Its title is still a placeholder. Your very first action this turn, before any other tool or any reply, is to call the Model Roamer tool `set_title` (3 to 6 words, verb first, see \"Naming your thread\"). The person sees the new name the moment you set it. Do it even for a quick question.\n");
    }
    s
}

/// The suite's design language, seeded into global memory the first time Model Roamer runs.
pub const DESIGN_NOTE: &str = "The Roamer suite look (Board Roamer, Web Roamer, Model Roamer): \"survey map meets ticket stub\".

- Color: oklch tokens. Themes night (default, deep teal-ink), paper (light, warm), violet. One accent hue (default acid lime oklch(.88 .2 128)); coral `--hot` for danger.
- Type: Bricolage Grotesque for display and body; JetBrains Mono for labels, buttons and chips (600 weight, 10-11.5px, uppercase, wide letter-spacing). Wordmarks like MODEL/ROAMER in mono 800 with an accent slash.
- Shapes: chamfered corners (clip-path polygons, 12px cut), skewed route chips (skewX(-14deg) with counter-skewed text), hard offset shadows (6px 6px 0, no blur), square-capped line icons on a 24px grid. No rounded cards, no decorative gradients, no chat bubbles.
- Atmosphere: a generative topographic contour map behind everything with gentle parallax, plus faint film grain.
- Motion: ease cubic-bezier(.2,.85,.25,1), spring cubic-bezier(.3,1.45,.5,1). Buttons press to .96; panels pop in over 180ms; a sliding hatch shows progress. Respect reduced motion.
- Interaction: custom frameless title bar, keyboard shortcuts for everything, right-click menus, toasts with Undo, empty states that teach, settings that save instantly.";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn harness_sections() {
        assert!(harness("claude").contains("Claude Code"));
        assert!(harness("codex").contains("apply_patch"));
        assert!(!harness("codex").contains("Gemini CLI"));
        assert!(harness("my-agent").contains("text protocol"));
    }
}
