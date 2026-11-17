//! The AI assistant panel: a chat docked above the status line that can
//! explain Cano and act on the buffer through a small set of tools.
//!
//! It talks to Anthropic's Messages API, or to OpenAI's Chat Completions API
//! when only an OpenAI key is set, and is simply unavailable without a key.
//! Requests go through the `curl` executable on a worker thread rather than an
//! HTTP crate: Cano is built for size, and a TLS stack would dwarf the editor.
//! The key reaches curl on its standard input, never on its command line,
//! where any other user could read it out of the process table.
//!
//! The panel owns the conversation and the tool loop.  Tools that touch the
//! editor run on the main thread between frames; a shell command runs on a
//! worker so a slow build does not freeze the screen, and it -- like writing a
//! file -- waits for the user to allow it first.

use std::collections::VecDeque;
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Value, json};

use crate::app::{App, AppEffect};
use crate::buffer::Buffer;
use crate::history::UndoRecord;
use crate::io::GENERAL_HELP;
use crate::terminal::{Input, Mouse, MouseKind};

/// Share of the screen the panel takes when it first opens.
pub const DEFAULT_RATIO: f32 = 0.2;
/// The panel's own rows: the title bar, at least one transcript row, and the
/// input line.
pub const MIN_ROWS: u16 = 3;
/// Model round trips one request may take before the loop is stopped, so a
/// model that keeps calling tools cannot run up a bill unattended.
const MAX_ROUNDS: usize = 40;
/// The most a single tool result may hand back to the model.
const MAX_TOOL_OUTPUT: usize = 30_000;
/// Lines `read_buffer` and `read_file` return per call.
const MAX_LINES: usize = 2_000;
/// Transcript entries kept on screen; older ones scroll away for good.
const MAX_ENTRIES: usize = 400;
/// How long one HTTP request may take before curl gives up.
const REQUEST_TIMEOUT_SECONDS: u32 = 600;

const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const OPENAI_URL: &str = "https://api.openai.com/v1/chat/completions";
const ANTHROPIC_MODEL: &str = "claude-opus-5-5";
const OPENAI_MODEL: &str = "gpt-5";

const HELP_KEYS: &str = include_str!("../docs/help/keys");
const HELP_COMMANDS: &str = include_str!("../docs/help/cmds");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Provider {
    Anthropic,
    OpenAi,
}

/// Which API to call and how.  The key is kept out of `Debug` output.
#[derive(Clone)]
pub struct Backend {
    pub provider: Provider,
    pub model: String,
    key: String,
    effort: String,
}

impl fmt::Debug for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Backend")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl Backend {
    /// The backend the environment asks for, or `None` when no key is set.
    ///
    /// Anthropic wins when both keys are present unless `CANO_AI_PROVIDER`
    /// says otherwise; `CANO_AI_MODEL` and `CANO_AI_EFFORT` override the
    /// defaults.
    pub fn from_env() -> Option<Self> {
        Self::detect(|name| std::env::var(name).ok())
    }

    fn detect(var: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let set = |name: &str| var(name).filter(|value| !value.trim().is_empty());
        let anthropic = set("ANTHROPIC_API_KEY");
        let openai = set("OPENAI_API_KEY");
        let wanted = set("CANO_AI_PROVIDER").map(|value| value.to_ascii_lowercase());
        let (provider, key) = match (wanted.as_deref(), anthropic, openai) {
            (Some("openai"), _, Some(key)) => (Provider::OpenAi, key),
            (_, Some(key), _) => (Provider::Anthropic, key),
            (_, None, Some(key)) => (Provider::OpenAi, key),
            (_, None, None) => return None,
        };
        let model = set("CANO_AI_MODEL").unwrap_or_else(|| {
            match provider {
                Provider::Anthropic => ANTHROPIC_MODEL,
                Provider::OpenAi => OPENAI_MODEL,
            }
            .to_owned()
        });
        Some(Self {
            provider,
            model,
            key: key.trim().to_owned(),
            effort: set("CANO_AI_EFFORT").unwrap_or_else(|| "medium".to_owned()),
        })
    }

    fn url(&self) -> &'static str {
        match self.provider {
            Provider::Anthropic => ANTHROPIC_URL,
            Provider::OpenAi => OPENAI_URL,
        }
    }

    fn headers(&self) -> Vec<String> {
        let mut headers = vec!["content-type: application/json".to_owned()];
        match self.provider {
            Provider::Anthropic => {
                headers.push(format!("x-api-key: {}", self.key));
                headers.push("anthropic-version: 2023-06-01".to_owned());
            }
            Provider::OpenAi => headers.push(format!("authorization: Bearer {}", self.key)),
        }
        headers
    }

    /// The request body for the conversation so far.
    fn body(&self, messages: &[Value]) -> Value {
        match self.provider {
            Provider::Anthropic => json!({
                    "model": self.model,
                    "max_tokens": 16000,
                    "system": system_prompt(),
                    "tools": tool_specs()
                        .into_iter()
                        .map(|(name, description, schema)| json!({
                            "name": name,
                            "description": description,
                            "input_schema": schema,
                        }))
                        .collect::<Vec<_>>(),
                    "messages": messages,
                    "output_config": {"effort": self.effort},
                    // The system prompt and tools are identical on every
                    // request, so the whole prefix is worth caching.
                    "cache_control": {"type": "ephemeral"},
            }),
            Provider::OpenAi => {
                let mut all = vec![json!({"role": "system", "content": system_prompt()})];
                all.extend(messages.iter().cloned());
                json!({
                    "model": self.model,
                    "max_completion_tokens": 16000,
                    "messages": all,
                    "tools": tool_specs()
                        .into_iter()
                        .map(|(name, description, schema)| json!({
                            "type": "function",
                            "function": {
                                "name": name,
                                "description": description,
                                "parameters": schema,
                            },
                        }))
                        .collect::<Vec<_>>(),
                })
            }
        }
    }

    /// Reads one response into what the loop needs: the text to show, the
    /// tools to run, and the message to append to the conversation verbatim.
    fn parse(&self, response: &Value) -> Turn {
        match self.provider {
            Provider::Anthropic => {
                let content = response["content"].as_array().cloned().unwrap_or_default();
                let mut turn = Turn {
                    // Appended exactly as received: thinking blocks have to
                    // go back unchanged on the next request.
                    message: json!({"role": "assistant", "content": content}),
                    ..Turn::default()
                };
                for block in &content {
                    match block["type"].as_str() {
                        Some("text") => {
                            if let Some(text) = block["text"].as_str() {
                                turn.text.push(text.to_owned());
                            }
                        }
                        Some("tool_use") => turn.calls.push(Call {
                            id: block["id"].as_str().unwrap_or_default().to_owned(),
                            name: block["name"].as_str().unwrap_or_default().to_owned(),
                            input: block["input"].clone(),
                        }),
                        _ => {}
                    }
                }
                turn.stop = response["stop_reason"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                if turn.stop == "refusal" {
                    // Nothing the model said in a declined turn is safe to
                    // build on, and its tool calls must not run.
                    turn.calls.clear();
                    turn.message = Value::Null;
                }
                turn
            }
            Provider::OpenAi => {
                let choice = &response["choices"][0];
                let message = &choice["message"];
                let mut turn = Turn::default();
                if let Some(text) = message["content"].as_str().filter(|t| !t.is_empty()) {
                    turn.text.push(text.to_owned());
                }
                if let Some(refusal) = message["refusal"].as_str() {
                    turn.text.push(refusal.to_owned());
                }
                let calls = message["tool_calls"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                for call in &calls {
                    let arguments = call["function"]["arguments"].as_str().unwrap_or("{}");
                    turn.calls.push(Call {
                        id: call["id"].as_str().unwrap_or_default().to_owned(),
                        name: call["function"]["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                        input: serde_json::from_str(arguments).unwrap_or(Value::Null),
                    });
                }
                let mut kept = json!({"role": "assistant", "content": message["content"]});
                if !calls.is_empty() {
                    kept["tool_calls"] = Value::Array(calls);
                }
                turn.message = kept;
                turn.stop = choice["finish_reason"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                turn
            }
        }
    }

    /// The conversation messages that carry a round of tool results.
    fn result_messages(&self, results: &[ToolResult]) -> Vec<Value> {
        match self.provider {
            Provider::Anthropic => vec![json!({
                "role": "user",
                "content": results
                    .iter()
                    .map(|result| json!({
                        "type": "tool_result",
                        "tool_use_id": result.id,
                        "content": result.content,
                        "is_error": result.is_error,
                    }))
                    .collect::<Vec<_>>(),
            })],
            Provider::OpenAi => results
                .iter()
                .map(|result| {
                    json!({
                        "role": "tool",
                        "tool_call_id": result.id,
                        "content": result.content,
                    })
                })
                .collect(),
        }
    }
}

/// One model response, reduced to what the loop acts on.
#[derive(Clone, Debug, Default)]
struct Turn {
    text: Vec<String>,
    calls: Vec<Call>,
    stop: String,
    /// The assistant message to append, or `Null` for one to drop.
    message: Value,
}

#[derive(Clone, Debug)]
struct Call {
    id: String,
    name: String,
    input: Value,
}

#[derive(Clone, Debug)]
struct ToolResult {
    id: String,
    content: String,
    is_error: bool,
}

/// What a line of the transcript is, which decides its label and color.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    User,
    Assistant,
    Tool,
    Error,
    Info,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub role: Role,
    pub text: String,
}

/// A result a worker thread hands back.
type Slot<T> = Arc<Mutex<Option<T>>>;

#[derive(Clone, Debug)]
enum Phase {
    Idle,
    /// A model request is in flight.
    Requesting {
        slot: Slot<Result<Value, String>>,
    },
    /// The call at the front of the queue needs the user's permission.
    Approving,
    /// An approved shell command is running.
    Shell {
        slot: Slot<String>,
        id: String,
        /// The command's process id once it has started, so that stopping
        /// the task can stop the command too.
        pid: Slot<u32>,
    },
}

#[derive(Clone, Debug)]
pub struct Assistant {
    /// `None` when no API key is configured, which switches the feature off.
    pub backend: Option<Backend>,
    pub open: bool,
    /// Whether keys go to the panel rather than the buffer.
    pub focused: bool,
    /// The panel's share of the screen, kept as a ratio so it survives a
    /// resize the way it looked.
    pub ratio: f32,
    /// True while a drag that started on the title bar is resizing the panel.
    pub resizing: bool,
    /// True while a drag that started on the transcript's scrollbar is still
    /// scrolling it, even if the pointer strays off the column.
    pub bar_drag: bool,
    pub input: Vec<u8>,
    pub input_cursor: usize,
    pub transcript: Vec<Entry>,
    /// Transcript rows scrolled back from the newest.
    pub scroll: usize,
    messages: Vec<Value>,
    phase: Phase,
    queue: VecDeque<Call>,
    results: Vec<ToolResult>,
    rounds: usize,
    /// When the current request began, for the elapsed time in the title.
    started: Option<Instant>,
    /// Tools the user said to allow for the rest of the session.
    always: Vec<String>,
    /// Where the conversation is kept between runs, when there is somewhere
    /// to keep it.
    pub history_path: Option<PathBuf>,
    /// The conversation changed since it was last written.
    dirty: bool,
}

impl Assistant {
    pub fn new(backend: Option<Backend>) -> Self {
        Self {
            backend,
            open: false,
            focused: false,
            ratio: DEFAULT_RATIO,
            resizing: false,
            bar_drag: false,
            input: Vec::new(),
            input_cursor: 0,
            transcript: Vec::new(),
            scroll: 0,
            messages: Vec::new(),
            phase: Phase::Idle,
            queue: VecDeque::new(),
            results: Vec::new(),
            rounds: 0,
            started: None,
            always: Vec::new(),
            history_path: None,
            dirty: false,
        }
    }

    pub fn available(&self) -> bool {
        self.backend.is_some()
    }

    /// Whether something is happening that the main loop must keep polling.
    pub fn busy(&self) -> bool {
        matches!(self.phase, Phase::Requesting { .. } | Phase::Shell { .. })
    }

    pub fn approving(&self) -> Option<String> {
        if !matches!(self.phase, Phase::Approving) {
            return None;
        }
        self.queue.front().map(describe_call)
    }

    /// The title bar's activity note.
    pub fn status(&self) -> String {
        let elapsed = self
            .started
            .map(|start| start.elapsed().as_secs())
            .unwrap_or_default();
        match self.phase {
            Phase::Idle => String::new(),
            Phase::Requesting { .. } => format!("thinking… {elapsed}s"),
            Phase::Approving => "waiting for you".to_owned(),
            Phase::Shell { .. } => format!("running… {elapsed}s"),
        }
    }

    /// Rows the panel takes on a screen `height` rows tall, leaving the
    /// editor at least one row and the status and prompt lines theirs.
    pub fn rows(&self, height: u16) -> u16 {
        if !self.open {
            return 0;
        }
        let most = height.saturating_sub(3);
        if most < MIN_ROWS {
            return 0;
        }
        let wanted = (f32::from(height) * self.ratio).round() as u16;
        wanted.clamp(MIN_ROWS, most)
    }

    /// Scrolls the transcript back (positive) or forward, stopping at its
    /// oldest line: `most` is how far back the last frame could show.
    pub fn scroll_by(&mut self, delta: isize, most: usize) {
        self.scroll = self.scroll.saturating_add_signed(delta).min(most);
    }

    /// Sets the panel height from the row its title bar was dragged to.
    pub fn resize_to(&mut self, row: u16, panel_bottom: u16, height: u16) {
        if height == 0 {
            return;
        }
        let rows = panel_bottom.saturating_sub(row).max(MIN_ROWS);
        self.ratio = (f32::from(rows) / f32::from(height)).clamp(0.05, 0.95);
    }

    pub(crate) fn push(&mut self, role: Role, text: impl Into<String>) {
        self.dirty = true;
        let text = text.into();
        let text = text.trim_end();
        if text.is_empty() {
            return;
        }
        self.transcript.push(Entry {
            role,
            text: text.to_owned(),
        });
        if self.transcript.len() > MAX_ENTRIES {
            let excess = self.transcript.len() - MAX_ENTRIES;
            self.transcript.drain(..excess);
        }
        self.scroll = 0;
    }

    /// Starts the conversation over.
    fn clear(&mut self) {
        self.cancel();
        self.messages.clear();
        self.transcript.clear();
        self.scroll = 0;
        // Clearing is the one way a chat is forgotten, so the saved copy
        // goes with it.
        self.dirty = false;
        if let Some(path) = &self.history_path {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Picks the conversation up where the last run left it.
    ///
    /// The transcript always comes back.  The API history only comes back
    /// for the same provider and model: each provider's messages are in its
    /// own format, and a model is only shown its own thinking blocks.  A
    /// file that cannot be read is left alone rather than treated as empty,
    /// so a damaged chat is not overwritten before anyone looks at it.
    pub fn restore(&mut self, path: PathBuf) {
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(saved) = serde_json::from_str::<Value>(&text)
        {
            for entry in saved["transcript"].as_array().into_iter().flatten() {
                let role = match entry["role"].as_str() {
                    Some("user") => Role::User,
                    Some("assistant") => Role::Assistant,
                    Some("tool") => Role::Tool,
                    Some("error") => Role::Error,
                    _ => Role::Info,
                };
                if let Some(text) = entry["text"].as_str() {
                    self.transcript.push(Entry {
                        role,
                        text: text.to_owned(),
                    });
                }
            }
            let same = self.backend.as_ref().is_some_and(|backend| {
                saved["provider"] == provider_name(backend.provider)
                    && saved["model"] == backend.model.as_str()
            });
            if same {
                self.messages = saved["messages"].as_array().cloned().unwrap_or_default();
            } else if !self.transcript.is_empty() {
                self.push(
                    Role::Info,
                    "(The model changed since this chat, so it starts without the earlier \
                     context. Ctrl-L clears.)",
                );
            }
        }
        self.history_path = Some(path);
    }

    /// Writes the conversation out once it has settled.
    ///
    /// Mid-round the history holds tool calls that are not answered yet,
    /// which the API would refuse to continue from, so nothing is written
    /// until the round is over.  The file is private to the user: a chat
    /// carries whatever the files and commands it touched printed.
    pub fn persist(&mut self) {
        if !self.dirty || !matches!(self.phase, Phase::Idle) || !self.queue.is_empty() {
            return;
        }
        let (Some(path), Some(backend)) = (&self.history_path, &self.backend) else {
            return;
        };
        let saved = json!({
            "version": 1,
            "provider": provider_name(backend.provider),
            "model": backend.model,
            "transcript": self
                .transcript
                .iter()
                .map(|entry| json!({"role": role_name(entry.role), "text": entry.text}))
                .collect::<Vec<_>>(),
            "messages": self.messages,
        });
        if write_private(path, saved.to_string().as_bytes()).is_ok() {
            self.dirty = false;
        }
    }

    /// Abandons whatever is in flight, leaving a conversation the API will
    /// still accept: every tool call that was asked for gets an answer.
    pub(crate) fn cancel(&mut self) {
        let was_busy = !matches!(self.phase, Phase::Idle);
        let mut pending = std::mem::take(&mut self.results);
        if let Phase::Shell { id, pid, .. } = &self.phase {
            pending.push(cancelled(id));
            // Esc cancels the task (FR-AI-6), so the command it started
            // stops too, rather than running on unseen in the background.
            if let Some(pid) = *pid.lock().unwrap_or_else(|e| e.into_inner()) {
                stop_process_group(pid);
            }
        }
        pending.extend(self.queue.drain(..).map(|call| cancelled(&call.id)));
        if !pending.is_empty()
            && let Some(backend) = &self.backend
        {
            self.messages.extend(backend.result_messages(&pending));
        }
        self.phase = Phase::Idle;
        self.started = None;
        if was_busy {
            self.push(Role::Info, "Stopped.");
        }
    }

    /// Adds what the user typed, merged into a trailing user message so the
    /// roles keep alternating after a cancelled round.
    fn add_user_text(&mut self, text: &str) {
        let provider = self.backend.as_ref().map(|b| b.provider);
        if provider == Some(Provider::Anthropic)
            && let Some(last) = self.messages.last_mut()
            && last["role"] == "user"
            && let Some(content) = last["content"].as_array_mut()
        {
            content.push(json!({"type": "text", "text": text}));
            return;
        }
        self.messages.push(json!({"role": "user", "content": text}));
    }

    /// Sends the conversation as it stands.
    fn request(&mut self) {
        let Some(backend) = &self.backend else {
            return;
        };
        // Tests drive the loop by hand and must never reach the network.
        if cfg!(test) {
            self.started.get_or_insert_with(Instant::now);
            self.phase = Phase::Requesting {
                slot: Arc::default(),
            };
            return;
        }
        let body = backend.body(&self.messages).to_string();
        let slot: Slot<Result<Value, String>> = Arc::default();
        let writer = Arc::clone(&slot);
        let (url, headers) = (backend.url(), backend.headers());
        std::thread::spawn(move || {
            let result = post(url, &headers, &body);
            *writer.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        });
        self.started.get_or_insert_with(Instant::now);
        self.phase = Phase::Requesting { slot };
    }
}

fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "anthropic",
        Provider::OpenAi => "openai",
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
        Role::Error => "error",
        Role::Info => "info",
    }
}

/// Replaces `path` with `bytes` in one step, readable only by the owner.
///
/// The bytes go to a sibling first and are renamed over the old file, so a
/// crash mid-write leaves the previous chat rather than half of a new one.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary, path)
}

fn cancelled(id: &str) -> ToolResult {
    ToolResult {
        id: id.to_owned(),
        content: "Cancelled by the user.".to_owned(),
        is_error: true,
    }
}

fn take<T>(slot: &Slot<T>) -> Option<T> {
    slot.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// One line saying what a call will do, for the transcript and the
/// permission question.
fn describe_call(call: &Call) -> String {
    let field = |name: &str| call.input[name].as_str().unwrap_or_default().to_owned();
    match call.name.as_str() {
        "run_shell" => format!("run shell: {}", field("command")),
        "write_file" => format!("write file: {}", field("path")),
        "run_editor_command" => format!(":{}", field("command")),
        "read_file" => format!("read {}", field("path")),
        "list_directory" => format!("list {}", field("path")),
        "edit_buffer" => format!(
            "edit lines {}-{}",
            call.input["start_line"], call.input["end_line"]
        ),
        "read_buffer" => "read buffer".to_owned(),
        other => other.to_owned(),
    }
}

/// Where the character before byte `at` starts, skipping UTF-8 continuation
/// bytes; 0 at the start.
fn prev_boundary(bytes: &[u8], at: usize) -> usize {
    (0..at)
        .rev()
        .find(|&index| bytes[index] & 0xC0 != 0x80)
        .unwrap_or(0)
}

/// Where the character after the one at byte `at` starts; the length at the
/// end.
fn next_boundary(bytes: &[u8], at: usize) -> usize {
    (at + 1..bytes.len())
        .find(|&index| bytes[index] & 0xC0 != 0x80)
        .unwrap_or(bytes.len())
        .max(at)
}

fn needs_approval(name: &str) -> bool {
    matches!(name, "run_shell" | "write_file")
}

// ---- HTTP ------------------------------------------------------------------

/// POSTs `body` through curl, which retries twice on a rate limit or
/// overload by itself.
///
/// Everything sensitive goes to curl as a config file on its stdin.  A
/// quoted config value takes backslash escapes, so the body's backslashes
/// and quotes are escaped; compact JSON carries no raw newlines to worry
/// about.
fn post(url: &str, headers: &[String], body: &str) -> Result<Value, String> {
    let (status, text) = curl(url, headers, body)?;
    let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if (200..300).contains(&status) {
        return Ok(value);
    }
    let message = value["error"]["message"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| text.chars().take(300).collect());
    Err(format!("HTTP {status}: {message}"))
}

fn curl(url: &str, headers: &[String], body: &str) -> Result<(u16, String), String> {
    let quote = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
    let mut config = format!(
        "url = \"{}\"\nrequest = \"POST\"\nsilent\nshow-error\nretry = 2\nmax-time = {REQUEST_TIMEOUT_SECONDS}\n\
         write-out = \"\\n%{{http_code}}\"\n",
        quote(url)
    );
    for header in headers {
        config.push_str(&format!("header = \"{}\"\n", quote(header)));
    }
    config.push_str(&format!("data-binary = \"{}\"\n", quote(body)));

    let mut child = Command::new("curl")
        .args(["--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not run curl ({error}); the assistant needs it"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(config.as_bytes())
            .map_err(|error| error.to_string())?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let (text, code) = stdout.rsplit_once('\n').unwrap_or(("", stdout.as_str()));
    let status = code.trim().parse::<u16>().unwrap_or(0);
    if status == 0 {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(format!("request failed: {}", error.trim()));
    }
    Ok((status, text.to_owned()))
}

// ---- prompt and tools ------------------------------------------------------

fn system_prompt() -> String {
    format!(
        "You are the assistant built into Cano, a small modal (vim-like) terminal text editor. \
You appear in a panel at the bottom of the editor. You do two jobs:\n\
1. Teach people Cano: answer questions about its modes, keys and `:` commands from the \
reference below. Be concrete and give the exact keys. If the reference does not cover \
something, say so rather than guessing.\n\
2. Do tasks: read and edit the open buffer, run editor commands, read files, and -- with \
the user's permission -- run shell commands or write files. For a larger task, look \
before you change anything (get_editor_state, read_buffer), make the edits, then check \
the result. Edits to the buffer can be undone with `u`; tell the user when you change it.\n\n\
The panel is a few lines tall and plain text, so keep answers short and do not use \
markdown tables or headings. Line numbers in tools are 1-based.\n\n\
Panel keys: Enter sends, Esc returns to the editor (or stops a running request), Ctrl-K \
opens and closes the panel, Ctrl-L starts a new conversation, Up/Down/PageUp/PageDown \
scroll. The mouse focuses the panel, scrolls it, and drags its top bar to resize it.\n\n\
=== Cano reference: overview ===\n{}\n\
=== Cano reference: keys ===\n{HELP_KEYS}\n\
=== Cano reference: commands ===\n{HELP_COMMANDS}",
        String::from_utf8_lossy(GENERAL_HELP)
    )
}

fn tool_specs() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "get_editor_state",
            "The open file's name and directory, mode, cursor line and column, line count, \
             and whether it has unsaved changes.",
            json!({"type": "object", "properties": {}}),
        ),
        (
            "read_buffer",
            "Read lines of the open buffer, numbered. Without a range, reads from the top.",
            json!({
                "type": "object",
                "properties": {
                    "start_line": {"type": "integer", "description": "First line, 1-based"},
                    "end_line": {"type": "integer", "description": "Last line, inclusive"},
                },
            }),
        ),
        (
            "edit_buffer",
            "Replace lines start_line..end_line (inclusive, 1-based) of the open buffer with \
             new_text. To insert without replacing, set end_line = start_line - 1; to append, \
             set start_line to the line count + 1. An empty new_text deletes the lines. One \
             call is one undo step.",
            json!({
                "type": "object",
                "properties": {
                    "start_line": {"type": "integer"},
                    "end_line": {"type": "integer"},
                    "new_text": {"type": "string"},
                },
                "required": ["start_line", "end_line", "new_text"],
            }),
        ),
        (
            "run_editor_command",
            "Run a Cano `:` command line, such as `w` to save, `s/old/new/g`, or `set ...`. \
             Do not include the leading colon. Returns the editor's status message.",
            json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"],
            }),
        ),
        (
            "read_file",
            "Read a text file from disk, numbered. Paths are relative to the working directory.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "start_line": {"type": "integer"},
                    "end_line": {"type": "integer"},
                },
                "required": ["path"],
            }),
        ),
        (
            "list_directory",
            "List a directory; subdirectories end in /.",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string", "description": "Defaults to ."}},
            }),
        ),
        (
            "run_shell",
            "Run a shell command and get its exit status and output. The user is asked to \
             allow each command first.",
            json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"],
            }),
        ),
        (
            "write_file",
            "Create or overwrite a file other than the one open in the editor (use \
             edit_buffer for that). The user is asked to allow it first.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"},
                },
                "required": ["path", "content"],
            }),
        ),
    ]
}

fn clip(mut text: String) -> String {
    if text.len() > MAX_TOOL_OUTPUT {
        text.truncate(text.floor_char_boundary(MAX_TOOL_OUTPUT));
        text.push_str("\n[output truncated]");
    }
    text
}

/// Numbers lines `start..=end` (1-based) of `text`.
fn numbered(text: &str, start: Option<u64>, end: Option<u64>) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let first = start.map_or(1, |s| s.max(1) as usize);
    let last = end.map_or(total, |e| (e as usize).min(total));
    let last = last.min(first.saturating_add(MAX_LINES - 1));
    let mut out = String::new();
    for (index, line) in lines
        .iter()
        .enumerate()
        .take(last)
        .skip(first.saturating_sub(1))
    {
        out.push_str(&format!("{:>5}  {line}\n", index + 1));
    }
    if last < total {
        out.push_str(&format!("[{} more lines; total {total}]\n", total - last));
    }
    if out.is_empty() {
        out = format!("[no lines in that range; total {total}]");
    }
    clip(out)
}

/// Byte offset where 1-based `line` starts, or the end of the data for the
/// line just past the last.
fn line_start(buffer: &Buffer, line: usize) -> usize {
    buffer
        .rows
        .get(line - 1)
        .map_or(buffer.data.len(), |row| row.start)
}

/// Lines as a person counts them: a final newline ends the last line rather
/// than starting the empty row the buffer keeps after it.
fn line_count(buffer: &Buffer) -> usize {
    buffer.rows.len() - usize::from(buffer.data.ends_with(b"\n"))
}

/// Computes the byte range lines `start..=end` occupy and the bytes to put
/// there, so the replacement keeps the line structure around it intact.
fn line_edit(
    buffer: &Buffer,
    start: usize,
    end: usize,
    text: &str,
) -> Result<(usize, usize, Vec<u8>), String> {
    let data = &buffer.data;
    let total = line_count(buffer);
    if start == 0 || start > total + 1 || end + 1 < start || end > total {
        return Err(format!(
            "line range {start}-{end} is outside the buffer, which has {total} lines"
        ));
    }
    let from = line_start(buffer, start);
    let to = if end < start {
        from
    } else {
        line_start(buffer, end + 1)
    };
    let mut bytes = text.as_bytes().to_vec();
    if !bytes.is_empty()
        && !bytes.ends_with(b"\n")
        && (to < data.len() || data[..to].ends_with(b"\n"))
    {
        bytes.push(b'\n');
    }
    // Appending after a last line that has no newline needs one first.
    if from == data.len() && !data.is_empty() && !data.ends_with(b"\n") && !bytes.is_empty() {
        bytes.insert(0, b'\n');
    }
    Ok((from, to, bytes))
}

fn shell(command: &str, started: &Slot<u32>) -> String {
    let mut builder = Command::new("sh");
    builder
        .args(["-c", command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, so stopping it also stops what it started.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut builder, 0);
    let output = builder.spawn().and_then(|child| {
        *started.lock().unwrap_or_else(|e| e.into_inner()) = Some(child.id());
        child.wait_with_output()
    });
    match output {
        Ok(output) => {
            let mut text = format!(
                "exit status: {}\n",
                output
                    .status
                    .code()
                    .map_or("signal".to_owned(), |c| c.to_string())
            );
            text.push_str(&String::from_utf8_lossy(&output.stdout));
            if !output.stderr.is_empty() {
                text.push_str("\n[stderr]\n");
                text.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            clip(text)
        }
        Err(error) => format!("could not start the shell: {error}"),
    }
}

/// Asks a shell command's process group to stop.
fn stop_process_group(pid: u32) {
    #[cfg(unix)]
    if let Ok(pid) = libc::pid_t::try_from(pid) {
        // SAFETY: `kill` takes plain integers; a group that has already
        // exited makes it fail with ESRCH, which is ignored.
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

// ---- the editor side -------------------------------------------------------

impl App {
    /// Ctrl-K and the status-bar button.
    pub fn toggle_assistant(&mut self) {
        if !self.assistant.available() {
            self.set_message(
                "AI assistant is off: set ANTHROPIC_API_KEY or OPENAI_API_KEY and restart",
            );
            return;
        }
        let assistant = &mut self.assistant;
        assistant.open = !assistant.open;
        assistant.focused = assistant.open;
        assistant.resizing = false;
        if assistant.open && assistant.transcript.is_empty() {
            let model = assistant
                .backend
                .as_ref()
                .map(|b| b.model.clone())
                .unwrap_or_default();
            assistant.push(
                Role::Info,
                format!(
                    "Ask about Cano or give me a task ({model}). Enter sends, Esc goes back \
                     to the editor, Ctrl-K closes."
                ),
            );
        }
    }

    /// Whether this key belongs to the panel rather than the editor.
    pub fn assistant_owns(&self, input: Input) -> bool {
        input == Input::Control(11)
            || (self.assistant.open
                && self.assistant.focused
                && !matches!(input, Input::Mouse(_) | Input::Resize))
    }

    /// A key typed while the panel has focus.
    pub fn assistant_key(&mut self, input: Input) -> Vec<AppEffect> {
        if input == Input::Control(11) {
            self.toggle_assistant();
            return Vec::new();
        }
        if self.assistant.approving().is_some() {
            return match input {
                Input::Byte(b'y' | b'Y') | Input::Enter => self.answer_approval(true, false),
                Input::Byte(b'a' | b'A') => self.answer_approval(true, true),
                Input::Byte(b'n' | b'N') => self.answer_approval(false, false),
                // Esc stops the whole task; `n` refuses this one step and
                // lets the model carry on without it.
                Input::Escape | Input::Control(3) => {
                    self.assistant.cancel();
                    Vec::new()
                }
                _ => Vec::new(),
            };
        }
        let assistant = &mut self.assistant;
        match input {
            Input::Escape | Input::Control(3) => {
                if assistant.busy() {
                    assistant.cancel();
                } else {
                    assistant.focused = false;
                }
            }
            Input::Enter => return self.send_prompt(),
            Input::Control(12) => assistant.clear(),
            Input::Backspace | Input::Control(8) => {
                let start = prev_boundary(&assistant.input, assistant.input_cursor);
                assistant.input.drain(start..assistant.input_cursor);
                assistant.input_cursor = start;
            }
            Input::Delete => {
                let end = next_boundary(&assistant.input, assistant.input_cursor);
                assistant.input.drain(assistant.input_cursor..end);
            }
            Input::Left => {
                assistant.input_cursor = prev_boundary(&assistant.input, assistant.input_cursor);
            }
            Input::Right => {
                assistant.input_cursor = next_boundary(&assistant.input, assistant.input_cursor);
            }
            Input::Home | Input::Control(1) => assistant.input_cursor = 0,
            Input::End | Input::Control(5) => assistant.input_cursor = assistant.input.len(),
            Input::Control(21) => {
                assistant.input.drain(..assistant.input_cursor);
                assistant.input_cursor = 0;
            }
            Input::Up | Input::Down | Input::PageUp | Input::PageDown => {
                let page = isize::try_from(
                    self.viewport
                        .panel_track
                        .map_or(1, |(_, _, height)| height.saturating_sub(1).max(1)),
                )
                .unwrap_or(1);
                let delta = match input {
                    Input::Up => 1,
                    Input::Down => -1,
                    Input::PageUp => page,
                    _ => -page,
                };
                assistant.scroll_by(delta, self.viewport.panel_scroll_max);
            }
            Input::Byte(byte) => {
                let byte = if byte == b'\t' { b' ' } else { byte };
                assistant.input.insert(assistant.input_cursor, byte);
                assistant.input_cursor += 1;
            }
            _ => {}
        }
        Vec::new()
    }

    /// A mouse gesture, if it was on the panel or its button.  Returns true
    /// when the panel used it.
    pub fn assistant_mouse(&mut self, mouse: Mouse) -> bool {
        let viewport = self.viewport;
        if mouse.kind == MouseKind::Press
            && let Some((x0, x1, y)) = viewport.ai_button
            && mouse.row == y
            && (x0..x1).contains(&mouse.column)
        {
            self.toggle_assistant();
            return true;
        }
        let assistant = &mut self.assistant;
        // The scrollbar: a press on its column, and every drag that began
        // there, puts the thumb under the pointer.
        if mouse.kind == MouseKind::Press {
            assistant.bar_drag = viewport.panel_track.is_some_and(|(x, top, height)| {
                mouse.column == x && (top..top + height).contains(&mouse.row)
            });
        }
        if assistant.bar_drag && matches!(mouse.kind, MouseKind::Press | MouseKind::Drag) {
            if let Some(back) = viewport.panel_scroll_from_track(mouse.row) {
                assistant.scroll = back;
            }
            if mouse.kind == MouseKind::Press {
                assistant.focused = true;
                assistant.resizing = false;
            }
            return true;
        }
        if mouse.kind == MouseKind::Drag && assistant.resizing {
            if let Some((top, rows)) = viewport.panel {
                assistant.resize_to(mouse.row, top + rows, viewport.screen_height);
            }
            return true;
        }
        let Some((top, rows)) = viewport.panel else {
            return false;
        };
        let inside = (top..top + rows).contains(&mouse.row);
        match mouse.kind {
            MouseKind::Press => {
                assistant.resizing = inside && mouse.row == top;
                assistant.focused = inside;
                inside
            }
            MouseKind::ScrollUp if inside => {
                assistant.scroll_by(3, viewport.panel_scroll_max);
                true
            }
            MouseKind::ScrollDown if inside => {
                assistant.scroll_by(-3, viewport.panel_scroll_max);
                true
            }
            _ => inside,
        }
    }

    fn send_prompt(&mut self) -> Vec<AppEffect> {
        let assistant = &mut self.assistant;
        let text = String::from_utf8_lossy(&assistant.input).trim().to_owned();
        if text.is_empty() {
            return Vec::new();
        }
        // `/clear` is Ctrl-L typed out, for anyone who reaches for the chat
        // command every other assistant has.  It is answered here and never
        // sent: it goes through the same `clear` as the key, so it stops
        // whatever is in flight and forgets the saved chat just the same --
        // which is also why it is honoured while the assistant is busy.
        if text == "/clear" {
            assistant.input.clear();
            assistant.input_cursor = 0;
            assistant.clear();
            return Vec::new();
        }
        if assistant.busy() {
            self.set_message("The assistant is still working; Esc stops it");
            return Vec::new();
        }
        assistant.input.clear();
        assistant.input_cursor = 0;
        assistant.push(Role::User, text.clone());
        assistant.add_user_text(&text);
        assistant.rounds = 0;
        assistant.started = None;
        assistant.request();
        Vec::new()
    }

    fn answer_approval(&mut self, allow: bool, always: bool) -> Vec<AppEffect> {
        let assistant = &mut self.assistant;
        let Some(call) = assistant.queue.pop_front() else {
            assistant.phase = Phase::Idle;
            return Vec::new();
        };
        if always {
            assistant.always.push(call.name.clone());
        }
        assistant.phase = Phase::Idle;
        if allow {
            self.run_tool(call)
        } else {
            self.assistant
                .push(Role::Tool, format!("✗ denied: {}", describe_call(&call)));
            self.assistant.results.push(ToolResult {
                id: call.id,
                content: "The user did not allow this.".to_owned(),
                is_error: true,
            });
            self.advance_tools()
        }
    }

    /// Checks on in-flight work; called by the main loop between frames.
    pub fn assistant_poll(&mut self) -> Vec<AppEffect> {
        match self.assistant.phase.clone() {
            Phase::Requesting { slot } => match take(&slot) {
                Some(Ok(response)) => self.receive(&response),
                Some(Err(error)) => {
                    let assistant = &mut self.assistant;
                    assistant.phase = Phase::Idle;
                    assistant.started = None;
                    assistant.push(Role::Error, error);
                    Vec::new()
                }
                None => Vec::new(),
            },
            Phase::Shell { slot, id, .. } => match take(&slot) {
                Some(output) => {
                    self.assistant.phase = Phase::Idle;
                    let first = output.lines().next().unwrap_or_default().to_owned();
                    self.assistant.push(Role::Tool, format!("  {first}"));
                    self.assistant.results.push(ToolResult {
                        id,
                        content: output,
                        is_error: false,
                    });
                    self.advance_tools()
                }
                None => Vec::new(),
            },
            Phase::Idle | Phase::Approving => Vec::new(),
        }
    }

    fn receive(&mut self, response: &Value) -> Vec<AppEffect> {
        let assistant = &mut self.assistant;
        assistant.phase = Phase::Idle;
        let Some(backend) = assistant.backend.clone() else {
            return Vec::new();
        };
        assistant.dirty = true;
        let turn = backend.parse(response);
        for text in &turn.text {
            assistant.push(Role::Assistant, text.clone());
        }
        if turn.message.is_null() {
            assistant.started = None;
            if turn.stop == "refusal" {
                assistant.push(Role::Error, "The model declined this request.");
            }
            return Vec::new();
        }
        assistant.messages.push(turn.message);
        if matches!(turn.stop.as_str(), "max_tokens" | "length") {
            assistant.push(Role::Info, "(reply cut off at the length limit)");
        }
        if turn.calls.is_empty() {
            assistant.started = None;
            return Vec::new();
        }
        assistant.rounds += 1;
        if assistant.rounds > MAX_ROUNDS {
            assistant.queue.extend(turn.calls);
            assistant.cancel();
            assistant.push(
                Role::Error,
                format!("Stopped after {MAX_ROUNDS} tool rounds; say \"continue\" to go on."),
            );
            return Vec::new();
        }
        assistant.queue.extend(turn.calls);
        self.advance_tools()
    }

    /// Runs queued calls until one has to wait, then sends the results once
    /// the round is complete.
    fn advance_tools(&mut self) -> Vec<AppEffect> {
        let mut effects = Vec::new();
        loop {
            if !matches!(self.assistant.phase, Phase::Idle) {
                return effects;
            }
            let Some(call) = self.assistant.queue.front() else {
                break;
            };
            if needs_approval(&call.name) && !self.assistant.always.contains(&call.name) {
                self.assistant.phase = Phase::Approving;
                self.assistant.focused = true;
                return effects;
            }
            let call = self.assistant.queue.pop_front().expect("checked above");
            effects.extend(self.run_tool(call));
        }
        let assistant = &mut self.assistant;
        if !assistant.results.is_empty() {
            let results = std::mem::take(&mut assistant.results);
            if let Some(backend) = &assistant.backend {
                assistant.messages.extend(backend.result_messages(&results));
            }
            assistant.request();
        }
        effects
    }

    /// Runs one call.  A shell command goes to a worker and finishes later;
    /// everything else finishes here.
    fn run_tool(&mut self, call: Call) -> Vec<AppEffect> {
        self.assistant
            .push(Role::Tool, format!("⚙ {}", describe_call(&call)));
        if call.name == "run_shell" {
            let command = call.input["command"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let slot: Slot<String> = Arc::default();
            let pid: Slot<u32> = Arc::default();
            let (writer, started) = (Arc::clone(&slot), Arc::clone(&pid));
            std::thread::spawn(move || {
                let output = shell(&command, &started);
                *writer.lock().unwrap_or_else(|e| e.into_inner()) = Some(output);
            });
            self.assistant.phase = Phase::Shell {
                slot,
                id: call.id,
                pid,
            };
            return Vec::new();
        }
        let mut effects = Vec::new();
        // A tool that changes the buffer can arrive while the user is typing:
        // keep the user's insertion and the tool's edit as separate steps.
        let edits = matches!(call.name.as_str(), "edit_buffer" | "run_editor_command");
        if edits {
            self.settle_for_outside_edit();
        }
        let outcome = self.tool(&call, &mut effects);
        if edits {
            self.editor.resume_insert_step();
        }
        let (content, is_error) = match outcome {
            Ok(content) => (content, false),
            Err(error) => {
                self.assistant.push(Role::Error, format!("  {error}"));
                (error, true)
            }
        };
        self.assistant.results.push(ToolResult {
            id: call.id,
            content: clip(content),
            is_error,
        });
        effects.extend(self.advance_tools());
        effects
    }

    fn tool(&mut self, call: &Call, effects: &mut Vec<AppEffect>) -> Result<String, String> {
        let input = &call.input;
        let number = |name: &str| input[name].as_u64();
        let text = |name: &str| input[name].as_str().map(str::to_owned);
        match call.name.as_str() {
            "get_editor_state" => {
                let cwd = std::env::current_dir().unwrap_or_default();
                Ok(json!({
                    "file": self.filename.display().to_string(),
                    "working_directory": cwd.display().to_string(),
                    "mode": format!("{:?}", self.editor.mode),
                    "cursor_line": self.editor.buffer.cursor_row().unwrap_or(0) + 1,
                    "cursor_column": self.editor.buffer.cursor_column().unwrap_or(0) + 1,
                    "line_count": line_count(&self.editor.buffer),
                    "unsaved_changes": !self.saved(),
                    "read_only": self.readonly,
                })
                .to_string())
            }
            "read_buffer" => Ok(numbered(
                &String::from_utf8_lossy(&self.editor.buffer.data),
                number("start_line"),
                number("end_line"),
            )),
            "edit_buffer" => {
                if self.readonly {
                    return Err("this buffer is read-only".to_owned());
                }
                let (Some(start), Some(end), Some(new_text)) = (
                    number("start_line"),
                    input["end_line"].as_i64(),
                    text("new_text"),
                ) else {
                    return Err("start_line, end_line and new_text are required".to_owned());
                };
                let end = usize::try_from(end.max(0)).unwrap_or(0);
                let (from, to, bytes) =
                    line_edit(&self.editor.buffer, start as usize, end, &new_text)?;
                let original = self
                    .editor
                    .buffer
                    .replace_region(from, to, &bytes)
                    .ok_or("the edit did not apply")?;
                let new_end = from + bytes.len();
                self.editor
                    .history
                    .push_undo(UndoRecord::replace_region(from, new_end, original));
                self.editor.buffer.cursor = from.min(self.editor.buffer.data.len());
                Ok(format!(
                    "Done. The buffer now has {} lines.",
                    line_count(&self.editor.buffer)
                ))
            }
            "run_editor_command" => {
                let command = text("command").ok_or("command is required")?;
                let previous = self.commands.message.take();
                let produced = self.run_command(command.trim().as_bytes());
                let mut notes = Vec::new();
                for effect in produced {
                    match effect {
                        AppEffect::Save(path) => {
                            notes.push(format!("saving {}", path.display()));
                            effects.push(AppEffect::Save(path));
                        }
                        AppEffect::Quit => notes.push("quitting is left to the user".to_owned()),
                        AppEffect::Shell(_) => {
                            notes.push("use run_shell for shell commands".to_owned());
                        }
                        AppEffect::Suspend | AppEffect::Redraw => {}
                    }
                }
                // A command that quits sets the flag the save path reads;
                // the assistant never ends the session.
                self.commands.quit = false;
                let message = self.commands.message.clone();
                if message.is_none() {
                    self.commands.message = previous;
                }
                let mut report = message.unwrap_or_else(|| "OK".to_owned());
                if !notes.is_empty() {
                    report.push_str(&format!(" ({})", notes.join("; ")));
                }
                Ok(report)
            }
            "read_file" => {
                let path = text("path").ok_or("path is required")?;
                let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
                Ok(numbered(
                    &String::from_utf8_lossy(&bytes),
                    number("start_line"),
                    number("end_line"),
                ))
            }
            "list_directory" => {
                let path = text("path").unwrap_or_else(|| ".".to_owned());
                let mut names = std::fs::read_dir(&path)
                    .map_err(|e| format!("{path}: {e}"))?
                    .filter_map(Result::ok)
                    .map(|entry| {
                        let mut name = entry.file_name().to_string_lossy().into_owned();
                        if entry.path().is_dir() {
                            name.push('/');
                        }
                        name
                    })
                    .collect::<Vec<_>>();
                names.sort();
                names.truncate(1000);
                Ok(names.join("\n"))
            }
            "write_file" => {
                let path = PathBuf::from(text("path").ok_or("path is required")?);
                let content = text("content").ok_or("content is required")?;
                if same_file(&path, &self.filename) {
                    return Err("that is the open file; use edit_buffer instead".to_owned());
                }
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::write(&path, content.as_bytes())
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                Ok(format!(
                    "Wrote {} bytes to {}",
                    content.len(),
                    path.display()
                ))
            }
            other => Err(format!("unknown tool {other}")),
        }
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (resolve(a), resolve(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

/// The canonical form of `path`.  A file that does not exist yet (an open
/// buffer never saved) is resolved through its directory, so `./new.txt`
/// and `new.txt` still name the same file.
fn resolve(path: &Path) -> Option<PathBuf> {
    if let Ok(full) = path.canonicalize() {
        return Some(full);
    }
    let name = path.file_name()?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Some(parent.canonicalize().ok()?.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn no_key_means_no_assistant() {
        assert!(Backend::detect(env(&[])).is_none());
        assert!(Backend::detect(env(&[("ANTHROPIC_API_KEY", "  ")])).is_none());
    }

    #[test]
    fn anthropic_is_preferred_unless_asked_otherwise() {
        let both = [("ANTHROPIC_API_KEY", "a"), ("OPENAI_API_KEY", "o")];
        let backend = Backend::detect(env(&both)).unwrap();
        assert_eq!(backend.provider, Provider::Anthropic);
        assert_eq!(backend.model, ANTHROPIC_MODEL);

        let mut forced = both.to_vec();
        forced.push(("CANO_AI_PROVIDER", "openai"));
        assert_eq!(
            Backend::detect(env(&forced)).unwrap().provider,
            Provider::OpenAi
        );

        let only = Backend::detect(env(&[("OPENAI_API_KEY", "o")])).unwrap();
        assert_eq!(only.provider, Provider::OpenAi);
        let secret = Backend::detect(env(&[("ANTHROPIC_API_KEY", "sk-secret")])).unwrap();
        assert!(!format!("{secret:?}").contains("sk-secret"));
    }

    #[test]
    fn line_edits_keep_the_surrounding_lines() {
        let data = b"one\ntwo\nthree\n";
        let buffer = Buffer::new(data.to_vec());
        let apply = |start, end, text: &str| {
            let (from, to, bytes) = line_edit(&buffer, start, end, text).unwrap();
            let mut out = data.to_vec();
            out.splice(from..to, bytes);
            String::from_utf8(out).unwrap()
        };
        assert_eq!(apply(2, 2, "TWO"), "one\nTWO\nthree\n");
        assert_eq!(apply(2, 1, "new"), "one\nnew\ntwo\nthree\n");
        assert_eq!(apply(4, 3, "four"), "one\ntwo\nthree\nfour\n");
        assert_eq!(apply(1, 2, ""), "three\n");
        assert!(line_edit(&buffer, 0, 1, "x").is_err());
        assert!(line_edit(&buffer, 2, 9, "x").is_err());

        let open = b"a\nb";
        let (from, to, bytes) = line_edit(&Buffer::new(open.to_vec()), 3, 2, "c").unwrap();
        let mut out = open.to_vec();
        out.splice(from..to, bytes);
        assert_eq!(out, b"a\nb\nc");
    }

    #[test]
    fn cursor_steps_over_whole_characters() {
        let text = "aé€".as_bytes();
        assert_eq!(next_boundary(text, 0), 1);
        assert_eq!(next_boundary(text, 1), 3);
        assert_eq!(next_boundary(text, 3), 6);
        assert_eq!(next_boundary(text, 6), 6);
        assert_eq!(prev_boundary(text, 6), 3);
        assert_eq!(prev_boundary(text, 3), 1);
        assert_eq!(prev_boundary(text, 0), 0);
    }

    #[test]
    fn panel_height_follows_the_ratio_within_bounds() {
        let mut assistant = Assistant::new(None);
        assert_eq!(assistant.rows(50), 0);
        assistant.open = true;
        assert_eq!(assistant.rows(50), 10);
        assert_eq!(assistant.rows(5), 0);
        assistant.resize_to(20, 47, 50);
        assert_eq!(assistant.rows(50), 27);
    }

    fn app_with_assistant(text: &[u8]) -> App {
        let mut app = App::new(text.to_vec(), PathBuf::from("file.txt"));
        app.assistant.backend = Backend::detect(env(&[("ANTHROPIC_API_KEY", "k")]));
        app
    }

    fn type_text(app: &mut App, text: &str) {
        for byte in text.bytes() {
            app.handle(Input::Byte(byte));
        }
    }

    /// Hands the loop a model response as if it had come off the wire.
    fn reply(app: &mut App, content: Value, stop: &str) -> Vec<AppEffect> {
        let Phase::Requesting { slot, .. } = app.assistant.phase.clone() else {
            panic!("no request in flight: {:?}", app.assistant.phase);
        };
        *slot.lock().unwrap() = Some(Ok(json!({"content": content, "stop_reason": stop})));
        app.assistant_poll()
    }

    #[test]
    fn ctrl_k_without_a_key_explains_how_to_turn_it_on() {
        let mut app = App::new(b"x".to_vec(), PathBuf::from("file.txt"));
        app.handle(Input::Control(11));
        assert!(!app.assistant.open);
        assert!(
            app.commands
                .message
                .as_deref()
                .unwrap()
                .contains("ANTHROPIC_API_KEY")
        );
    }

    #[test]
    fn the_panel_takes_the_keyboard_until_escape() {
        let mut app = app_with_assistant(b"hello\n");
        app.handle(Input::Control(11));
        assert!(app.assistant.open && app.assistant.focused);
        type_text(&mut app, "dd");
        assert_eq!(app.assistant.input, b"dd");
        assert_eq!(app.editor.buffer.data, b"hello\n");

        app.handle(Input::Escape);
        assert!(app.assistant.open && !app.assistant.focused);
        app.handle(Input::Byte(b'x'));
        assert_eq!(app.editor.buffer.data, b"ello\n");

        app.handle(Input::Control(11));
        assert!(!app.assistant.open);
    }

    #[test]
    fn the_status_bar_button_and_title_bar_answer_the_mouse() {
        let mut app = app_with_assistant(b"text\n");
        app.viewport.ai_button = Some((70, 74, 23));
        app.viewport.screen_height = 24;
        let press = |column, row| Mouse {
            kind: MouseKind::Press,
            column,
            row,
        };
        app.handle(Input::Mouse(press(71, 23)));
        assert!(app.assistant.open);

        app.viewport.panel = Some((18, 5));
        app.handle(Input::Mouse(press(3, 2)));
        assert!(
            !app.assistant.focused,
            "a click in the text gives the editor the keys"
        );
        app.handle(Input::Mouse(press(3, 18)));
        assert!(app.assistant.focused && app.assistant.resizing);
        app.handle(Input::Mouse(Mouse {
            kind: MouseKind::Drag,
            column: 3,
            row: 11,
        }));
        assert_eq!(app.assistant.rows(24), 12);

        app.handle(Input::Mouse(press(71, 23)));
        assert!(!app.assistant.open);
    }

    #[test]
    fn scrolling_stops_at_the_oldest_line_and_comes_straight_back() {
        let mut app = app_with_assistant(b"");
        app.handle(Input::Control(11));
        app.viewport.panel = Some((10, 8));
        app.viewport.panel_track = Some((79, 11, 6));
        app.viewport.panel_scroll_max = 4;
        let wheel = |kind| {
            Input::Mouse(Mouse {
                kind,
                column: 5,
                row: 12,
            })
        };
        for _ in 0..20 {
            app.handle(wheel(MouseKind::ScrollUp));
        }
        assert_eq!(app.assistant.scroll, 4, "the wheel stops at the top");
        app.handle(wheel(MouseKind::ScrollDown));
        assert_eq!(app.assistant.scroll, 1, "one notch down moves right away");
        for _ in 0..5 {
            app.handle(Input::PageUp);
        }
        assert_eq!(app.assistant.scroll, 4);
        app.handle(Input::PageDown);
        assert_eq!(app.assistant.scroll, 0);

        // Grabbing the bar at its top shows the oldest lines; dragging to
        // its bottom comes back to the newest, even off the column.
        let mouse = |kind, column, row| Input::Mouse(Mouse { kind, column, row });
        app.handle(mouse(MouseKind::Press, 79, 11));
        assert_eq!(app.assistant.scroll, 4);
        app.handle(mouse(MouseKind::Drag, 40, 16));
        assert_eq!(app.assistant.scroll, 0);
        assert!(!app.assistant.resizing);
    }

    #[test]
    fn the_chat_survives_a_restart_until_it_is_cleared() {
        let fixture = crate::test_support::Fixture::new("ai-history");
        let path = fixture.root.join("ai-history.json");

        let mut app = app_with_assistant(b"");
        app.assistant.restore(path.clone());
        app.handle(Input::Control(11));
        type_text(&mut app, "hello");
        app.handle(Input::Enter);
        // Nothing is written while the request is still out.
        app.assistant.persist();
        assert!(!path.exists());
        reply(
            &mut app,
            json!([{"type": "text", "text": "Hi there."}]),
            "end_turn",
        );
        app.assistant.persist();
        assert!(path.exists());

        let mut next = app_with_assistant(b"");
        next.assistant.restore(path.clone());
        assert_eq!(next.assistant.transcript.last().unwrap().text, "Hi there.");
        assert_eq!(
            next.assistant.messages.len(),
            2,
            "the model keeps its context"
        );

        // Another model gets the transcript to read but not the context.
        let mut other = App::new(Vec::new(), PathBuf::from("file.txt"));
        other.assistant.backend = Backend::detect(env(&[("OPENAI_API_KEY", "k")]));
        other.assistant.restore(path.clone());
        assert!(other.assistant.messages.is_empty());
        assert!(
            other
                .assistant
                .transcript
                .iter()
                .any(|e| e.text == "Hi there.")
        );

        next.handle(Input::Control(11));
        next.handle(Input::Control(12));
        assert!(!path.exists(), "Ctrl-L forgets the saved chat too");
        assert!(next.assistant.transcript.is_empty());
    }

    #[test]
    fn typing_slash_clear_starts_a_new_chat_without_sending_anything() {
        let fixture = crate::test_support::Fixture::new("ai-clear");
        let path = fixture.root.join("ai-history.json");

        let mut app = app_with_assistant(b"");
        app.assistant.restore(path.clone());
        app.handle(Input::Control(11));
        type_text(&mut app, "hello");
        app.handle(Input::Enter);
        reply(
            &mut app,
            json!([{"type": "text", "text": "Hi there."}]),
            "end_turn",
        );
        app.assistant.persist();
        assert!(path.exists());

        // Surrounding blanks are trimmed the way any prompt's are.
        type_text(&mut app, "  /clear ");
        app.handle(Input::Enter);
        assert!(!app.assistant.busy(), "nothing went to the API");
        assert!(app.assistant.transcript.is_empty());
        assert!(app.assistant.messages.is_empty());
        assert!(app.assistant.input.is_empty());
        assert_eq!(app.assistant.input_cursor, 0);
        app.assistant.persist();
        assert!(
            !path.exists(),
            "/clear forgets the saved chat as Ctrl-L does"
        );

        // It also stops a request in flight, as Ctrl-L would.
        type_text(&mut app, "again");
        app.handle(Input::Enter);
        assert!(app.assistant.busy());
        type_text(&mut app, "/clear");
        app.handle(Input::Enter);
        assert!(!app.assistant.busy());
        assert!(app.assistant.messages.is_empty());

        // Only the whole prompt counts: a question that mentions it is sent.
        type_text(&mut app, "what does /clear do?");
        app.handle(Input::Enter);
        assert!(app.assistant.busy());
    }

    #[test]
    fn a_task_edits_the_buffer_through_tools_and_can_be_undone() {
        let mut app = app_with_assistant(b"one\ntwo\nthree\n");
        app.handle(Input::Control(11));
        type_text(&mut app, "capitalise line 2");
        app.handle(Input::Enter);
        assert!(app.assistant.busy());
        assert_eq!(app.assistant.messages[0]["content"], "capitalise line 2");

        reply(
            &mut app,
            json!([
                {"type": "tool_use", "id": "a", "name": "read_buffer", "input": {}},
                {"type": "tool_use", "id": "b", "name": "edit_buffer",
                 "input": {"start_line": 2, "end_line": 2, "new_text": "TWO"}},
            ]),
            "tool_use",
        );
        assert_eq!(app.editor.buffer.data, b"one\nTWO\nthree\n");
        // Both results went back together, in one user message.
        let results = &app.assistant.messages[2];
        assert_eq!(results["role"], "user");
        assert_eq!(results["content"].as_array().unwrap().len(), 2);
        assert!(
            results["content"][0]["content"]
                .as_str()
                .unwrap()
                .contains("2  two")
        );

        reply(
            &mut app,
            json!([{"type": "text", "text": "Done."}]),
            "end_turn",
        );
        assert!(!app.assistant.busy());
        assert_eq!(app.assistant.transcript.last().unwrap().text, "Done.");

        app.handle(Input::Escape);
        app.handle(Input::Byte(b'u'));
        assert_eq!(app.editor.buffer.data, b"one\ntwo\nthree\n");
    }

    /// Real keys only: the user is typing on line 3, opens the panel with
    /// Ctrl-K (which leaves the editor in Insert mode), asks for a change,
    /// and the reply edits line `line`.  Escape then goes back to the editor.
    fn edit_while_typing(line: u64) -> App {
        let mut app = app_with_assistant(b"one\ntwo\nthree\n");
        app.handle(Input::Byte(b'j'));
        app.handle(Input::Byte(b'j'));
        app.handle(Input::Byte(b'i'));
        type_text(&mut app, "XY");
        assert_eq!(app.editor.buffer.data, b"one\ntwo\nXYthree\n");
        app.handle(Input::Control(11));
        assert!(app.assistant.focused);
        type_text(&mut app, "change a line");
        app.handle(Input::Enter);
        assert_eq!(app.editor.mode, crate::editor::Mode::Insert);
        reply(
            &mut app,
            json!([{"type": "tool_use", "id": "e", "name": "edit_buffer",
                    "input": {"start_line": line, "end_line": line, "new_text": "ONE!!"}}]),
            "tool_use",
        );
        reply(
            &mut app,
            json!([{"type": "text", "text": "Done."}]),
            "end_turn",
        );
        app.handle(Input::Escape); // the panel lets go of the keyboard
        assert!(!app.assistant.focused);
        app
    }

    #[test]
    fn an_assistant_edit_while_typing_is_its_own_undo_step() {
        // The edit lands above the text being typed, which moves it.
        let mut app = edit_while_typing(1);
        assert_eq!(app.editor.buffer.data, b"ONE!!\ntwo\nXYthree\n");
        assert_eq!(app.editor.mode, crate::editor::Mode::Insert);
        type_text(&mut app, "Z");
        app.handle(Input::Escape);
        // One `u` per step, newest first: the typing after the edit, the
        // edit, then the typing before it.
        app.handle(Input::Byte(b'u'));
        assert_eq!(app.editor.buffer.data, b"ONE!!\ntwo\nXYthree\n");
        app.handle(Input::Byte(b'u'));
        assert_eq!(app.editor.buffer.data, b"one\ntwo\nXYthree\n");
        app.handle(Input::Byte(b'u'));
        assert_eq!(app.editor.buffer.data, b"one\ntwo\nthree\n");
    }

    #[test]
    fn an_assistant_edit_below_the_typing_is_its_own_undo_step_too() {
        let mut app = edit_while_typing(3);
        assert_eq!(app.editor.buffer.data, b"one\ntwo\nONE!!\n");
        app.handle(Input::Escape);
        app.handle(Input::Byte(b'u'));
        assert_eq!(app.editor.buffer.data, b"one\ntwo\nXYthree\n");
        app.handle(Input::Byte(b'u'));
        assert_eq!(app.editor.buffer.data, b"one\ntwo\nthree\n");
    }

    /// Every editor state that can hold buffer offsets across keys, hit by
    /// an assistant edit that empties the buffer or grows it.  Each row
    /// first checks that its state was really entered.  After the edit, a
    /// fixed run of keys must leave the buffer valid and must not panic.
    /// The oracle is structural (`validate`), so it cannot see an edit that
    /// is lost; the separate tests above cover those.
    #[test]
    fn an_assistant_edit_in_any_editor_state_leaves_a_valid_buffer() {
        use crate::editor::Mode;
        use Input::{Byte, Control, Enter, Escape};
        let k = |s: &str| s.bytes().map(Byte).collect::<Vec<_>>();
        let cat = |parts: Vec<Vec<Input>>| parts.concat();
        type Entered = fn(&App) -> bool;
        let states: Vec<(&str, Vec<Input>, Entered)> = vec![
            ("normal", vec![], |a| a.editor.mode == Mode::Normal),
            ("insert", k("Axy"), |a| a.editor.mode == Mode::Insert),
            (
                "insert after Ctrl-N",
                cat(vec![k("Ao"), vec![Control(14)]]),
                |a| a.editor.mode == Mode::Insert,
            ),
            ("visual", k("vll"), |a| a.editor.mode == Mode::Visual),
            ("visual line", k("Vj"), |a| a.editor.mode == Mode::Visual),
            ("visual block", cat(vec![vec![Control(22)], k("jl")]), |a| {
                a.editor.mode == Mode::Visual
            }),
            ("pending operator", k("d"), |a| a.editor.pending_operator()),
            ("counted operator", k("2d"), |a| a.editor.pending_operator()),
            ("jump waiting for a character", k("s"), |a| {
                a.pending_hint() == "s-"
            }),
            ("jump labels shown", k("se"), |a| a.pending_hint() == "jump"),
            ("command prompt", k(":s/o"), |a| {
                a.editor.mode == Mode::Command
            }),
            ("search prompt", k("/t"), |a| a.editor.mode == Mode::Search),
            (
                "confirm substitute",
                cat(vec![k(":%s/o/0/gc"), vec![Enter]]),
                |a| a.confirming.is_some(),
            ),
            ("file explorer", vec![Control(14)], |a| a.explorer.is_some()),
            ("save prompt", cat(vec![k("x"), vec![Control(14)]]), |a| {
                a.save_prompt.is_some()
            }),
        ];
        let edits = [("empty", 1, 3, ""), ("grow", 1, 1, "a\nb\nc\nd\ne")];
        let after = cat(vec![
            vec![Control(14)],
            k("yn"),
            vec![Enter, Escape],
            k("lxu"),
            vec![Control(18)],
            k("pvdu"),
        ]);
        let mut failures = Vec::new();
        for (name, keys, entered) in &states {
            for (edit, start, end, text) in edits {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut app = app_with_assistant(b"one\ntwo\nthree\n");
                    app.viewport.rows = 3;
                    app.viewport.content_x = 5;
                    app.viewport.content_width = 40;
                    app.mark_rendered();
                    app.handle(Control(11));
                    type_text(&mut app, "edit");
                    app.handle(Enter);
                    app.handle(Control(11)); // Ctrl-K closes the panel; the request runs on
                    for key in keys {
                        app.handle(*key);
                    }
                    if !entered(&app) {
                        return Err("the state was not entered".to_owned());
                    }
                    reply(
                        &mut app,
                        json!([{"type": "tool_use", "id": "e", "name": "edit_buffer",
                                "input": {"start_line": start, "end_line": end, "new_text": text}}]),
                        "tool_use",
                    );
                    reply(
                        &mut app,
                        json!([{"type": "text", "text": "ok"}]),
                        "end_turn",
                    );
                    for (i, key) in after.iter().enumerate() {
                        app.handle(*key);
                        if let Err(error) = app.editor.buffer.validate() {
                            return Err(format!("after key {i} ({key:?}): {error:?}"));
                        }
                    }
                    Ok(())
                }));
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(why)) => failures.push(format!("{name} / {edit}: {why}")),
                    Err(panic) => failures.push(format!(
                        "{name} / {edit}: panic {}",
                        panic
                            .downcast_ref::<String>()
                            .cloned()
                            .or(panic.downcast_ref::<&str>().map(|s| s.to_string()))
                            .unwrap_or_default()
                    )),
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn an_assistant_edit_ends_a_substitute_waiting_for_confirmation() {
        let mut app = app_with_assistant(b"one\ntwo\nthree\n");
        app.handle(Input::Control(11));
        type_text(&mut app, "edit");
        app.handle(Input::Enter);
        app.handle(Input::Control(11)); // Ctrl-K closes the panel; the request runs on
        type_text(&mut app, ":%s/o/0/gc");
        app.handle(Input::Enter);
        app.handle(Input::Byte(b'y'));
        assert_eq!(app.editor.buffer.data, b"0ne\ntwo\nthree\n");
        assert!(app.confirming.is_some());
        reply(
            &mut app,
            json!([{"type": "tool_use", "id": "e", "name": "edit_buffer",
                    "input": {"start_line": 3, "end_line": 3, "new_text": ""}}]),
            "tool_use",
        );
        assert!(app.confirming.is_none());
        reply(
            &mut app,
            json!([{"type": "text", "text": "ok"}]),
            "end_turn",
        );
        app.handle(Input::Byte(b'u'));
        assert_eq!(app.editor.buffer.data, b"0ne\ntwo\nthree\n");
        app.handle(Input::Byte(b'u'));
        assert_eq!(app.editor.buffer.data, b"one\ntwo\nthree\n");
    }

    #[cfg(unix)]
    #[test]
    fn escape_stops_a_running_shell_command() {
        let mut app = app_with_assistant(b"");
        app.handle(Input::Control(11));
        type_text(&mut app, "wait");
        app.handle(Input::Enter);
        reply(
            &mut app,
            json!([{"type": "tool_use", "id": "s", "name": "run_shell",
                    "input": {"command": "sleep 30"}}]),
            "tool_use",
        );
        app.handle(Input::Byte(b'y'));
        let Phase::Shell { pid, .. } = app.assistant.phase.clone() else {
            panic!("not running: {:?}", app.assistant.phase);
        };
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let pid = loop {
            if let Some(pid) = *pid.lock().unwrap() {
                break pid as libc::pid_t;
            }
            assert!(Instant::now() < deadline, "the command never started");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        app.handle(Input::Escape);
        assert!(!app.assistant.busy());
        // The group is gone once the worker has reaped `sh`.
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while unsafe { libc::kill(-pid, 0) } == 0 {
            assert!(Instant::now() < deadline, "the command is still running");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// The model `CanoAssistantProtocol`'s view of the assistant
    /// (rust-verifier/case_studies/cano/specs/assistant_protocol.json).
    fn abstract_state(a: &Assistant) -> (u8, u8, u8, usize, usize) {
        let (req, appr, shell) = match a.phase {
            Phase::Idle => (0, 0, 0),
            Phase::Requesting { .. } => (1, 0, 0),
            Phase::Approving => (0, 1, 0),
            Phase::Shell { .. } => (0, 0, 1),
        };
        (req, appr, shell, a.queue.len(), a.results.len())
    }

    /// The API's rule, read off the real conversation: every `tool_use` the
    /// model sent has a `tool_result`.
    fn unanswered_calls(messages: &[Value]) -> Vec<String> {
        let mut asked = Vec::new();
        let mut answered = Vec::new();
        for m in messages {
            for block in m["content"].as_array().into_iter().flatten() {
                match block["type"].as_str() {
                    Some("tool_use") => asked.push(block["id"].as_str().unwrap_or("").to_owned()),
                    Some("tool_result") => {
                        answered.push(block["tool_use_id"].as_str().unwrap_or("").to_owned())
                    }
                    _ => {}
                }
            }
        }
        asked.retain(|id| !answered.contains(id));
        asked
    }

    #[derive(Clone, Copy, Debug)]
    enum Event {
        Send,
        Text,
        Fail,
        Calls(&'static [&'static str]),
        Key(u8),
        ShellDone,
        Escape,
        Clear,
    }

    fn apply_event(app: &mut App, event: Event, dir: &Path, n: &mut usize) {
        match event {
            Event::Send => {
                app.assistant.focused = true;
                type_text(app, "go");
                app.handle(Input::Enter);
            }
            Event::Text => {
                reply(app, json!([{"type": "text", "text": "ok"}]), "end_turn");
            }
            Event::Fail => {
                let Phase::Requesting { slot } = app.assistant.phase.clone() else {
                    unreachable!()
                };
                *slot.lock().unwrap() = Some(Err("network".to_owned()));
                app.assistant_poll();
            }
            Event::Calls(kinds) => {
                let calls: Vec<Value> = kinds
                    .iter()
                    .map(|kind| {
                        *n += 1;
                        let id = format!("c{n}");
                        match *kind {
                            "shell" => json!({"type": "tool_use", "id": id, "name": "run_shell", "input": {"command": "true"}}),
                            "write" => json!({"type": "tool_use", "id": id, "name": "write_file",
                                "input": {"path": dir.join(format!("w{n}")).display().to_string(), "content": "x"}}),
                            _ => json!({"type": "tool_use", "id": id, "name": "read_buffer", "input": {}}),
                        }
                    })
                    .collect();
                reply(app, Value::Array(calls), "tool_use");
            }
            Event::Key(byte) => {
                app.assistant.focused = true;
                app.handle(Input::Byte(byte));
            }
            Event::ShellDone => {
                let deadline = Instant::now() + std::time::Duration::from_secs(10);
                while matches!(app.assistant.phase, Phase::Shell { .. }) {
                    app.assistant_poll();
                    assert!(Instant::now() < deadline, "the shell never finished");
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            }
            Event::Escape => {
                app.assistant.focused = true;
                app.handle(Input::Escape);
            }
            Event::Clear => {
                app.assistant.focused = true;
                app.handle(Input::Control(12));
            }
        }
    }

    /// Explores the real assistant breadth-first over these events, and
    /// checks the model's invariant and the API's rule after every one.
    #[test]
    fn the_assistant_follows_the_protocol_model_in_every_reachable_state() {
        use std::collections::{BTreeSet, VecDeque};
        let dir = std::env::temp_dir().join(format!("cano-protocol-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let events = [
            Event::Send,
            Event::Text,
            Event::Fail,
            Event::Calls(&["read"]),
            Event::Calls(&["read", "read"]),
            Event::Calls(&["shell"]),
            Event::Calls(&["write"]),
            Event::Calls(&["read", "shell"]),
            Event::Calls(&["shell", "read"]),
            Event::Calls(&["write", "read"]),
            Event::Calls(&["shell", "write"]),
            Event::Key(b'y'),
            Event::Key(b'n'),
            Event::Key(b'a'),
            Event::ShellDone,
            Event::Escape,
            Event::Clear,
        ];
        // Keys go in every state, including ones the model never takes them
        // in; only a reply needs a request in flight, and only a finished
        // shell needs one running.
        let enabled = |a: &Assistant, e: Event| {
            let (req, _, shell, ..) = abstract_state(a);
            match e {
                Event::Text | Event::Fail | Event::Calls(_) => req == 1,
                Event::ShellDone => shell == 1,
                Event::Send | Event::Key(_) | Event::Escape | Event::Clear => true,
            }
        };
        let mut seen = BTreeSet::new();
        let mut queue: VecDeque<Vec<Event>> = VecDeque::from([vec![]]);
        let mut steps = 0;
        while let Some(path) = queue.pop_front() {
            for &event in &events {
                let mut app = app_with_assistant(b"one\ntwo\n");
                app.handle(Input::Control(11));
                let mut n = 0;
                for &e in &path {
                    apply_event(&mut app, e, &dir, &mut n);
                }
                if !enabled(&app.assistant, event) {
                    continue;
                }
                apply_event(&mut app, event, &dir, &mut n);
                steps += 1;
                let a = &app.assistant;
                let (req, appr, shell, q, r) = abstract_state(a);
                let (req, appr, shell) = (req as usize, appr as usize, shell as usize);
                let mut next = path.clone();
                next.push(event);
                assert!(
                    2 * req + q + r + shell <= 2 && req + appr + shell <= 1 && appr <= q,
                    "invariant broken after {next:?}: {:?}",
                    abstract_state(a)
                );
                assert!(
                    q + r <= 2 * appr + 2 * shell,
                    "work left behind after {next:?}"
                );
                if req == 1 {
                    assert!(
                        unanswered_calls(&a.messages).is_empty(),
                        "a request went out with unanswered calls after {next:?}"
                    );
                }
                if req + appr + shell == 0 {
                    assert!(
                        unanswered_calls(&a.messages).is_empty(),
                        "the settled conversation has unanswered calls after {next:?}"
                    );
                }
                let key = (abstract_state(a), a.always.clone(), next.len().min(6));
                if next.len() < 6 && seen.insert(key) {
                    queue.push_back(next);
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(steps > 100, "{steps}");
    }

    #[test]
    fn write_file_refuses_the_open_file_before_it_is_ever_saved() {
        let dir = std::env::temp_dir().join(format!("cano-same-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let open = dir.join("new.txt"); // not on disk yet
        for other in [
            dir.join(".").join("new.txt"),
            dir.join("sub").join("..").join("new.txt"),
        ] {
            assert!(
                same_file(&other, &open),
                "{} vs {}",
                other.display(),
                open.display()
            );
        }
        assert!(!same_file(&dir.join("other.txt"), &open));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_assistant_edit_under_an_open_file_list_is_not_lost() {
        let mut app = app_with_assistant(b"one\ntwo\n");
        app.handle(Input::Control(11));
        type_text(&mut app, "edit");
        app.handle(Input::Enter);
        app.handle(Input::Control(11)); // Ctrl-K closes the panel; the request runs on
        // The buffer is clean, so Ctrl-N opens the explorer without asking.
        app.handle(Input::Control(14));
        assert!(app.explorer.is_some());
        reply(
            &mut app,
            json!([{"type": "tool_use", "id": "e", "name": "edit_buffer",
                    "input": {"start_line": 1, "end_line": 1, "new_text": "ONE"}}]),
            "tool_use",
        );
        reply(
            &mut app,
            json!([{"type": "text", "text": "ok"}]),
            "end_turn",
        );
        assert_eq!(app.editor.buffer.data, b"ONE\ntwo\n");
        // The user picks Cargo.toml if the list is still up (setting the
        // cursor stands in for pressing j) and presses Enter.
        if let Some(explorer) = app.explorer.as_mut() {
            explorer.cursor = explorer
                .entries
                .iter()
                .position(|e| e.path.ends_with("Cargo.toml"))
                .expect("Cargo.toml is listed");
        }
        app.handle(Input::Enter);
        assert_eq!(
            app.editor.buffer.data, b"ONE\ntwo\n",
            "the edit was dropped without asking"
        );
    }

    /// Puts a reply in the slot without polling, as the worker thread does.
    fn deliver(app: &App, content: Value, stop: &str) {
        let Phase::Requesting { slot, .. } = app.assistant.phase.clone() else {
            panic!("no request in flight");
        };
        *slot.lock().unwrap() = Some(Ok(json!({"content": content, "stop_reason": stop})));
    }

    #[test]
    fn a_reply_that_arrives_with_q_is_seen_before_q_decides() {
        // In one main-loop pass, `:q` used to be handled first and the reply
        // polled after: `:q` saw a clean buffer, the edit then made it dirty,
        // and Quit was still applied.
        let start = |app: &mut App| {
            app.handle(Input::Control(11));
            type_text(app, "edit");
            app.handle(Input::Enter);
            app.handle(Input::Control(11));
            type_text(app, ":q");
            deliver(
                app,
                json!([{"type": "tool_use", "id": "e", "name": "edit_buffer",
                        "input": {"start_line": 1, "end_line": 1, "new_text": "ONE"}}]),
                "tool_use",
            );
        };
        // The old order, for the record.
        let mut old = app_with_assistant(b"one\n");
        start(&mut old);
        let mut effects = old.handle(Input::Enter);
        effects.extend(old.assistant_poll());
        assert!(effects.contains(&AppEffect::Quit) && !old.saved());
        // The loop now polls first.
        let mut app = app_with_assistant(b"one\n");
        start(&mut app);
        let effects = app.handle_with_poll(Input::Enter);
        assert_eq!(app.editor.buffer.data, b"ONE\n");
        assert!(!effects.contains(&AppEffect::Quit), "{effects:?}");
    }

    #[test]
    fn opening_another_file_stops_work_meant_for_the_old_one() {
        let root = std::env::temp_dir().join(format!("cano-switch-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let other = root.join("other.txt");
        std::fs::write(&other, std::fs::read("Cargo.toml").unwrap().as_slice()).unwrap();
        let mut app = app_with_assistant(b"one\n");
        app.handle(Input::Control(11));
        type_text(&mut app, "edit line 1");
        app.handle(Input::Enter);
        app.handle(Input::Control(11));
        app.handle(Input::Control(14)); // Ctrl-N over a clean buffer
        assert!(app.explorer.is_some());
        if let Some(explorer) = app.explorer.as_mut() {
            // stands in for pressing j until the file is selected
            explorer.cursor = explorer
                .entries
                .iter()
                .position(|e| e.path.ends_with("Cargo.toml"))
                .unwrap();
        }
        app.handle(Input::Enter);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            app.editor.buffer.data,
            std::fs::read("Cargo.toml").unwrap().as_slice()
        );
        assert!(!app.assistant.busy());
        assert!(unanswered_calls(&app.assistant.messages).is_empty());
    }

    #[test]
    fn shell_commands_wait_for_permission() {
        let mut app = app_with_assistant(b"");
        app.handle(Input::Control(11));
        type_text(&mut app, "run it");
        app.handle(Input::Enter);
        reply(
            &mut app,
            json!([{"type": "tool_use", "id": "s", "name": "run_shell",
                    "input": {"command": "echo hi"}}]),
            "tool_use",
        );
        assert_eq!(app.assistant.approving().unwrap(), "run shell: echo hi");
        assert!(!app.assistant.busy());

        app.handle(Input::Byte(b'n'));
        assert!(app.assistant.approving().is_none());
        let results = &app.assistant.messages[2]["content"][0];
        assert_eq!(results["is_error"], true);
        assert!(app.assistant.busy(), "the refusal goes back to the model");
    }

    #[test]
    fn stopping_a_round_still_answers_every_call() {
        let mut app = app_with_assistant(b"");
        app.handle(Input::Control(11));
        type_text(&mut app, "go");
        app.handle(Input::Enter);
        reply(
            &mut app,
            json!([{"type": "tool_use", "id": "s", "name": "run_shell",
                    "input": {"command": "true"}}]),
            "tool_use",
        );
        app.handle(Input::Escape);
        assert!(app.assistant.approving().is_none());
        assert_eq!(app.assistant.messages[2]["content"][0]["tool_use_id"], "s");
        // The next prompt joins the tool results instead of breaking the
        // user/assistant alternation.
        type_text(&mut app, "never mind");
        app.handle(Input::Enter);
        assert_eq!(app.assistant.messages.len(), 3);
        assert_eq!(
            app.assistant.messages[2]["content"][1]["text"],
            "never mind"
        );
    }

    #[test]
    fn requests_have_the_shape_each_api_expects() {
        let backend = Backend::detect(env(&[("ANTHROPIC_API_KEY", "k")])).unwrap();
        let body = backend.body(&[json!({"role": "user", "content": "hi"})]);
        assert_eq!(body["model"], ANTHROPIC_MODEL);
        assert!(body["tools"][0]["input_schema"].is_object());
        assert!(backend.headers().iter().any(|h| h == "x-api-key: k"));

        let response = json!({
            "stop_reason": "tool_use",
            "content": [
                {"type": "thinking", "thinking": "", "signature": "s"},
                {"type": "text", "text": "Looking."},
                {"type": "tool_use", "id": "t1", "name": "read_buffer", "input": {}},
            ],
        });
        let turn = backend.parse(&response);
        assert_eq!(turn.text, ["Looking."]);
        assert_eq!(turn.calls[0].name, "read_buffer");
        // The thinking block survives into the history unchanged.
        assert_eq!(turn.message["content"][0]["type"], "thinking");

        let openai = Backend::detect(env(&[("OPENAI_API_KEY", "k")])).unwrap();
        let body = openai.body(&[]);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["tools"][0]["type"], "function");
        let turn = openai.parse(
            &json!({"choices": [{"finish_reason": "tool_calls", "message": {
                "content": null,
                "tool_calls": [{"id": "c1", "type": "function",
                    "function": {"name": "read_file", "arguments": "{\"path\":\"a\"}"}}],
            }}]}),
        );
        assert_eq!(turn.calls[0].input["path"], "a");
    }
}
