//! A user-defined agent: any CLI that takes a prompt and prints an answer, set up in Settings › Agents without
//! writing code. Its output can be read as plain text or as Claude-, Codex- or Gemini-style JSON lines.

use super::*;

pub struct Custom {
    cfg: Value,
}

impl Custom {
    pub fn from_json(v: &Value) -> Option<Custom> {
        let id = v["id"].as_str()?.trim();
        if id.is_empty() || ["claude", "codex", "gemini", "antigravity"].contains(&id) || v["command"].as_str().unwrap_or("").trim().is_empty() {
            return None;
        }
        Some(Custom { cfg: v.clone() })
    }

    fn list(&self, key: &str) -> Vec<String> {
        self.cfg[key].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
    }

    fn output(&self) -> &str {
        self.cfg["output"].as_str().unwrap_or("text")
    }
}

fn fill(t: &str, spec: &TurnSpec, prompt: &str) -> String {
    t.replace("{prompt}", prompt)
        .replace("{model}", &spec.model)
        .replace("{session}", spec.session.as_deref().unwrap_or(""))
        .replace("{cwd}", &spec.cwd.display().to_string())
}

impl AgentAdapter for Custom {
    fn info(&self) -> Info {
        let c = &self.cfg;
        Info {
            id: s(&c["id"]),
            name: c["name"].as_str().unwrap_or("Custom agent").into(),
            vendor: c["vendor"].as_str().unwrap_or("Custom").into(),
            hue: c["hue"].as_i64().unwrap_or(95) as i32,
            strengths: self.list("strengths"),
            summary: c["summary"].as_str().unwrap_or("A command-line agent you added").into(),
            site: s(&c["site"]),
            models: vec![ModelOpt { id: "".into(), label: "CLI default".into(), note: String::new(), ..Default::default() }],
            mcp: false,
            can_lead: c["can_lead"].as_bool().unwrap_or(false),
        }
    }

    fn binaries(&self) -> Vec<String> {
        vec![s(&self.cfg["command"])]
    }

    fn needs_pty(&self) -> bool {
        self.cfg["pty"].as_bool().unwrap_or(false)
    }

    fn raw_is_text(&self) -> bool {
        self.output() == "text"
    }

    fn launch(&self, spec: &TurnSpec) -> anyhow::Result<Launch> {
        let full = with_system(spec);
        let via_stdin = self.cfg["prompt_via"].as_str().unwrap_or("stdin") == "stdin";
        let p = if via_stdin { String::new() } else { arg_prompt(spec, &full) };
        let mut args: Vec<String> = vec![];
        let tmpl = if spec.session.is_some() && self.cfg["resume_args"].is_array() { self.list("resume_args") } else { self.list("args") };
        for t in tmpl {
            if t.contains("{model}") && spec.model.is_empty() {
                continue;
            }
            if t.contains("{session}") && spec.session.is_none() {
                continue;
            }
            args.push(fill(&t, spec, &p));
        }
        let env = self.cfg["env"].as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), s(v))).collect()).unwrap_or_default();
        Ok(Launch { args, env, stdin: via_stdin.then_some(full), cwd: Some(spec.cwd.clone()), pty: self.needs_pty() })
    }

    fn parse(&self, line: &str, st: &mut Value) -> Vec<AgentEvent> {
        match self.output() {
            "claude" => super::claude::Claude.parse(line, st),
            "codex" => super::codex::Codex.parse(line, st),
            "gemini" => super::gemini::Gemini.parse(line, st),
            _ => {
                if line.trim().is_empty() && st["started"].is_null() {
                    return vec![];
                }
                st["started"] = json!(true);
                vec![AgentEvent::Raw(line.to_string())]
            }
        }
    }

    fn install(&self) -> Vec<Step> {
        let cmd = self.list("install");
        if cmd.is_empty() {
            return vec![];
        }
        vec![Step { label: format!("Install {}", self.info().name), cmd, needs: vec![] }]
    }

    fn login(&self, _headless: bool) -> Step {
        let cmd = self.list("login");
        Step { label: format!("Sign in to {}", self.info().name), cmd: if cmd.is_empty() { vec![s(&self.cfg["command"])] } else { cmd }, needs: vec![] }
    }
}
