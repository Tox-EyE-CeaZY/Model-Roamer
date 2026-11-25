use super::{clip_bytes, AgentEvent};
use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::{
    de::{self, MapAccess, Visitor},
    Deserialize,
};
use serde_json::{json, Map, Value};
use std::{collections::BTreeMap, fmt, sync::OnceLock, time::Duration};

const MAX_EVENT_BYTES: usize = 256 * 1024;
const MAX_TOOL_BYTES: usize = 16 * 1024;
const MAX_CALLS: usize = 4;

#[derive(Debug)]
pub(super) enum Error {
    Transport(anyhow::Error),
    Protocol(anyhow::Error),
    Tool(anyhow::Error),
}

#[derive(Default)]
pub(super) struct Round {
    pub text: String,
    pub displayed: bool,
    pub finish: String,
    pending: BTreeMap<usize, Fragments>,
    thinking: String,
}

#[derive(Debug)]
pub(super) struct Call {
    pub id: String,
    pub name: String,
    pub args: Value,
}

impl Call {
    pub(super) fn message(&self) -> Value {
        json!({ "id": self.id, "type": "function", "function": { "name": self.name, "arguments": self.args.to_string() } })
    }
}

#[derive(Default)]
struct Fragments {
    id: String,
    name: String,
    args: String,
    started: bool,
}

#[derive(Deserialize)]
struct Packet {
    #[serde(default)]
    choices: Vec<Choice>,
    usage: Option<Value>,
    error: Option<Value>,
}

#[derive(Deserialize)]
struct Choice {
    index: usize,
    #[serde(default)]
    delta: Delta,
    finish_reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct Delta {
    content: Option<String>,
    reasoning_content: Option<String>,
    tool_calls: Option<Value>,
}

impl Round {
    fn flush_thinking(&mut self, emit: &mut impl FnMut(AgentEvent)) {
        if !self.thinking.is_empty() {
            self.displayed = true;
            emit(AgentEvent::Thinking(std::mem::take(&mut self.thinking)));
        }
    }

    fn push(
        &mut self,
        data: &str,
        window: u32,
        emit: &mut impl FnMut(AgentEvent),
    ) -> std::result::Result<(), Error> {
        let packet: Packet = serde_json::from_str(data)
            .context("Invalid local completion event")
            .map_err(Error::Protocol)?;
        if packet.error.is_some() {
            return Err(Error::Protocol(anyhow!(
                "The local engine reported an error during generation."
            )));
        }
        if let Some(usage) = packet.usage {
            let input = usage["prompt_tokens"].as_i64().unwrap_or(0).max(0);
            let output = usage["completion_tokens"].as_i64().unwrap_or(0).max(0);
            let cached = usage["prompt_tokens_details"]["cached_tokens"]
                .as_i64()
                .unwrap_or(0)
                .max(0);
            emit(AgentEvent::Usage {
                input,
                output,
                cached,
                cost: 0.0,
            });
            emit(AgentEvent::Context {
                used: input.saturating_add(output),
                window: i64::from(window),
            });
        }
        if packet.choices.is_empty() {
            return Ok(());
        }
        let mut choices = packet.choices.into_iter();
        let choice = choices.next().unwrap();
        if choices.next().is_some() || choice.index != 0 || !self.finish.is_empty() {
            return Err(Error::Protocol(anyhow!(
                "Unexpected choice in the local completion stream."
            )));
        }
        if let Some(thinking) = choice.delta.reasoning_content {
            self.thinking.push_str(&thinking);
            if self.thinking.len() >= 240 {
                self.flush_thinking(emit);
            }
        }
        if let Some(text) = choice.delta.content.filter(|s| !s.is_empty()) {
            self.flush_thinking(emit);
            self.displayed = true;
            self.text.push_str(&text);
            emit(AgentEvent::Delta(text));
        }
        if let Some(tool_calls) = choice.delta.tool_calls {
            self.push_tools(tool_calls).map_err(Error::Tool)?;
        }
        if let Some(finish) = choice.finish_reason {
            self.flush_thinking(emit);
            self.finish = finish;
        }
        Ok(())
    }

    fn push_tools(&mut self, tools: Value) -> Result<()> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Tool {
            index: usize,
            id: Option<String>,
            #[serde(rename = "type")]
            kind: Option<String>,
            function: Function,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Function {
            name: Option<String>,
            arguments: Option<String>,
        }
        for tool in
            serde_json::from_value::<Vec<Tool>>(tools).context("Invalid tool call fragment")?
        {
            ensure!(
                tool.index < MAX_CALLS,
                "Too many tool calls in one response"
            );
            ensure!(
                tool.kind.as_deref().is_none_or(|kind| kind == "function"),
                "Unexpected tool call type"
            );
            let call = self.pending.entry(tool.index).or_default();
            call.started |= tool.kind.is_some();
            for (field, fragment) in [
                (&mut call.id, tool.id),
                (&mut call.name, tool.function.name),
                (&mut call.args, tool.function.arguments),
            ] {
                field.push_str(fragment.as_deref().unwrap_or(""));
                ensure!(field.len() <= MAX_TOOL_BYTES, "Tool call is too long");
            }
        }
        Ok(())
    }

    pub(super) fn calls(&self, tools: &[Value]) -> Result<Vec<Call>> {
        if self.pending.is_empty() && self.finish != "tool_calls" {
            return Ok(vec![]);
        }
        ensure!(
            self.finish == "tool_calls" && !self.pending.is_empty(),
            "The tool call was incomplete"
        );
        let mut calls: Vec<Call> = Vec::new();
        for (index, (actual_index, call)) in self.pending.iter().enumerate() {
            ensure!(index == *actual_index, "Missing tool call index");
            ensure!(
                call.started
                    && !call.id.is_empty()
                    && call.id.len() <= 128
                    && call
                        .id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
                "Invalid tool call ID"
            );
            ensure!(
                !calls.iter().any(|c| c.id == call.id),
                "Duplicate tool call ID"
            );
            let tool = tools
                .iter()
                .find(|tool| tool["function"]["name"] == call.name)
                .context("Tool is unavailable in this conversation")?;
            let Object(args) = serde_json::from_str(&call.args)
                .context("Arguments must be a single JSON object with unique keys")?;
            let args = Value::Object(args);
            validate(&args, &tool["function"]["parameters"])?;
            calls.push(Call {
                id: call.id.clone(),
                name: call.name.clone(),
                args,
            });
        }
        Ok(calls)
    }
}

// Tool schemas contain scalar properties and arrays of scalars. Reject repeated keys
// instead of allowing serde_json's Value parser to silently keep the last value.
struct Object(Map<String, Value>);

impl<'de> Deserialize<'de> for Object {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct UniqueKeys;
        impl<'de> Visitor<'de> for UniqueKeys {
            type Value = Object;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a JSON object without duplicate keys")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Object, A::Error> {
                let mut values = Map::new();
                while let Some((key, value)) = map.next_entry::<String, Value>()? {
                    if values.insert(key, value).is_some() {
                        return Err(de::Error::custom("duplicate tool argument"));
                    }
                }
                Ok(Object(values))
            }
        }
        d.deserialize_map(UniqueKeys)
    }
}

fn validate(value: &Value, schema: &Value) -> Result<()> {
    let kind = schema["type"].as_str().unwrap_or("");
    match kind {
        "object" => {
            let object = value.as_object().context("Expected a JSON object")?;
            let properties = &schema["properties"];
            for required in schema["required"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                ensure!(
                    object.contains_key(required),
                    "Missing argument '{required}'"
                );
            }
            for (key, value) in object {
                let p = properties
                    .get(key)
                    .with_context(|| format!("Unexpected argument '{}'", clip_bytes(key, 60)))?;
                validate(value, p).with_context(|| {
                    format!(
                        "Argument '{key}': expected {}",
                        p["type"].as_str().unwrap_or("the declared value")
                    )
                })?;
            }
        }
        "array" => {
            let entries = value.as_array().context("Expected an array")?;
            ensure!(entries.len() <= 32, "Too many array entries");
            for entry in entries {
                validate(entry, &schema["items"])?;
            }
        }
        "string" if value.is_string() => {}
        "boolean" if value.is_boolean() => {}
        "integer" if value.as_i64().is_some() => {}
        _ => bail!("Expected {kind}"),
    }
    if let Some(values) = schema["enum"].as_array() {
        ensure!(
            values.contains(value),
            "Value is outside the allowed choices"
        );
    }
    Ok(())
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(4))
            .read_timeout(Duration::from_secs(90))
            .build()
            .expect("local HTTP client")
    })
}

#[derive(Default)]
struct Sse {
    pending: Vec<u8>,
    data: String,
}

impl Sse {
    fn line(&mut self, line: &[u8]) -> Result<Option<String>> {
        let line = std::str::from_utf8(line.strip_suffix(b"\r").unwrap_or(line))
            .context("Invalid UTF-8 in the local stream")?;
        if line.is_empty() && !self.data.is_empty() {
            return Ok(Some(std::mem::take(&mut self.data)));
        }
        if let Some(data) = line.strip_prefix("data:") {
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(data.strip_prefix(' ').unwrap_or(data));
        }
        ensure!(
            self.data.len() <= MAX_EVENT_BYTES,
            "Local completion event was too large"
        );
        Ok(None)
    }
}

pub(super) async fn receive(
    base_url: &str,
    key: &str,
    context: u32,
    body: &Value,
    round: &mut Round,
    emit: &mut impl FnMut(AgentEvent),
) -> std::result::Result<(), Error> {
    let mut response = client()
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(key)
        .json(body)
        .send()
        .await
        .context("Couldn't reach the local model")
        .map_err(Error::Transport)?;
    let status = response.status();
    if !status.is_success() {
        let reason = anyhow!("The local model returned HTTP {status}.");
        return Err(if status.is_server_error() {
            Error::Transport(reason)
        } else {
            Error::Protocol(reason)
        });
    }
    if !response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"))
    {
        return Err(Error::Protocol(anyhow!(
            "The local engine did not return a completion stream."
        )));
    }
    let mut parser = Sse::default();
    let mut total = 0;
    while let Some(chunk) = response
        .chunk()
        .await
        .context("The local model stream stopped")
        .map_err(Error::Transport)?
    {
        total += chunk.len();
        if total > 2 * 1024 * 1024 {
            return Err(Error::Protocol(anyhow!(
                "The local completion stream was too large."
            )));
        }
        for slice in chunk.split_inclusive(|b| *b == b'\n') {
            parser.pending.extend_from_slice(slice);
            if parser.pending.len() > MAX_EVENT_BYTES {
                return Err(Error::Protocol(anyhow!(
                    "Local completion event was too large."
                )));
            }
            if slice.ends_with(b"\n") {
                let line = std::mem::take(&mut parser.pending);
                let event = parser
                    .line(&line[..line.len() - 1])
                    .map_err(Error::Protocol)?;
                if let Some(event) = event {
                    if event == "[DONE]" {
                        return if round.finish.is_empty() {
                            Err(Error::Protocol(anyhow!(
                                "Local completion ended without a finish reason."
                            )))
                        } else {
                            Ok(())
                        };
                    }
                    round.push(&event, context, emit)?;
                }
            }
        }
    }
    Err(Error::Transport(anyhow!(
        "The local model stream ended before completion."
    )))
}
