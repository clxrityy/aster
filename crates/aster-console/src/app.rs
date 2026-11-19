//! Console state, event handling, and slash commands.

use std::{
    cell::Cell,
    collections::{BTreeMap, HashSet, VecDeque},
};

use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use reqwest::Method;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::{Options, client::Client};

pub const LEVELS: [&str; 7] = [
    "energy",
    "focus",
    "cognitive_load",
    "curiosity",
    "stimulation",
    "stress",
    "confidence",
];
const SPARK_POINTS: usize = 24;

pub enum Msg {
    Input(TermEvent),
    Event(Value),
    Connected(bool),
    Resync,
    Notice(String, bool),
    Turns(Vec<Value>),
    Inbox(Vec<Value>),
    Status(Value),
    Chat(Value),
    Print(Vec<String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Who {
    Owner,
    Aster,
    System,
    Error,
}

pub struct Entry {
    pub who: Who,
    pub text: String,
    pub at: String,
    pub turn_id: Option<String>,
    pub rating: Option<i8>,
}

/// The scrollable panes, in focus order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pane {
    Conversation = 0,
    Inbox = 1,
    Trace = 2,
}

pub const PANES: [Pane; 3] = [Pane::Conversation, Pane::Inbox, Pane::Trace];
const WHEEL_LINES: usize = 3;

/// Rows scrolled from the top. `follow` keeps the view pinned to the bottom
/// (the conversation follows new lines until you scroll up).
#[derive(Clone, Copy, Debug, Default)]
pub struct Scroll {
    pub offset: usize,
    pub follow: bool,
}

/// What the last draw measured for a pane, so input can clamp and hit-test.
#[derive(Clone, Copy, Debug, Default)]
pub struct PaneView {
    pub area: Rect,
    pub max: usize,
    pub height: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Chat,
    Train,
}

pub struct App {
    pub client: Client,
    pub options: Options,
    tx: UnboundedSender<Msg>,
    pub transcript: Vec<Entry>,
    pub focus: Pane,
    pub scrolls: [Scroll; 3],
    pub views: Cell<[PaneView; 3]>,
    pub mouse: bool,
    pub input: String,
    pub cursor: usize,
    history: Vec<String>,
    history_pos: Option<usize>,
    pub mode: Mode,
    pub levels: Option<Value>,
    pub level_history: BTreeMap<&'static str, VecDeque<f64>>,
    pub inbox: Vec<Value>,
    pub trace: Option<Value>,
    pub show_trace: bool,
    pub last_aster_turn: Option<String>,
    pub awaiting_rating: bool,
    pub connected: bool,
    pub paused: bool,
    pub resting: bool,
    pub status: String,
    pub quit: bool,
    seen_turns: HashSet<String>,
}

fn time_of(value: &Value) -> String {
    value
        .as_str()
        .and_then(|s| s.get(11..19))
        .unwrap_or("")
        .to_string()
}

fn short(text: &str, max: usize) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}

pub const HELP: &[&str] = &[
    "Talk to Aster by typing. Slash commands:",
    "  /train  /chat               switch mode (train asks you to rate each reply)",
    "  /good  /bad  /correct <t>   rate the last reply (in train mode: + or -)",
    "  /teach <prompt> => <reply>  teach a reply directly",
    "  /answer <n> <text>          answer inbox item n",
    "  /approve <n>  /deny <n>     decide on request n",
    "  /remember <text>            store a memory     /forget <memory-id>",
    "  /ingest <path>              learn from a file inside server.ingest_dir",
    "  /set <key> <value>          change aster.toml live    /get <key>",
    "  /config [section]           show configuration",
    "  /pause  /resume             freeze or resume regulation and initiative",
    "  /status  /history           mind statistics / recent level changes",
    "  /snapshot [reason]  /snapshots  /rollback <version>",
    "  /logs <category> [n]        tail today's JSONL log (conversations, thoughts, levels, …)",
    "  /trace (Ctrl-T)             toggle the thought-trace panel",
    "  /mouse                      toggle mouse scrolling (off lets you select text)",
    "  /clear  /help  /quit",
    "Keys: Enter send · ↑/↓ input history · Esc clear · Ctrl-C quit",
    "Scroll: Tab/Shift-Tab focus a pane · PgUp/PgDn page · Shift-↑/↓ line ·",
    "        Ctrl-Home/End top/bottom · mouse wheel scrolls the pane under the pointer",
];

impl App {
    pub fn new(client: Client, options: Options, tx: UnboundedSender<Msg>) -> Self {
        let show_trace = options.show_trace;
        Self {
            client,
            options,
            tx,
            transcript: Vec::new(),
            focus: Pane::Conversation,
            scrolls: [
                Scroll {
                    offset: 0,
                    follow: true,
                },
                Scroll::default(),
                Scroll::default(),
            ],
            views: Cell::new([PaneView::default(); 3]),
            mouse: true,
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_pos: None,
            mode: Mode::Chat,
            levels: None,
            level_history: BTreeMap::new(),
            inbox: Vec::new(),
            trace: None,
            show_trace,
            last_aster_turn: None,
            awaiting_rating: false,
            connected: false,
            paused: false,
            resting: false,
            status: "connecting…".into(),
            quit: false,
            seen_turns: HashSet::new(),
        }
    }

    fn system(&mut self, text: impl Into<String>) {
        self.push(Who::System, text.into(), String::new(), None);
    }

    fn error(&mut self, text: impl Into<String>) {
        self.push(Who::Error, text.into(), String::new(), None);
    }

    fn push(&mut self, who: Who, text: String, at: String, turn_id: Option<String>) {
        self.transcript.push(Entry {
            who,
            text,
            at,
            turn_id,
            rating: None,
        });
    }

    pub fn view(&self, pane: Pane) -> PaneView {
        self.views.get()[pane as usize]
    }

    pub fn record_view(&self, pane: Pane, view: PaneView) {
        let mut views = self.views.get();
        views[pane as usize] = view;
        self.views.set(views);
    }

    /// The offset a pane is drawn at, given its current maximum.
    pub fn offset(&self, pane: Pane, max: usize) -> usize {
        let scroll = self.scrolls[pane as usize];
        if scroll.follow {
            max
        } else {
            scroll.offset.min(max)
        }
    }

    pub fn scroll_up(&mut self, pane: Pane, rows: usize) {
        let current = self.offset(pane, self.view(pane).max);
        self.scrolls[pane as usize] = Scroll {
            offset: current.saturating_sub(rows),
            follow: false,
        };
    }

    pub fn scroll_down(&mut self, pane: Pane, rows: usize) {
        let max = self.view(pane).max;
        let next = (self.offset(pane, max) + rows).min(max);
        self.scrolls[pane as usize] = Scroll {
            offset: next,
            follow: pane == Pane::Conversation && next >= max,
        };
    }

    fn scroll_to_end(&mut self, pane: Pane, bottom: bool) {
        self.scrolls[pane as usize] = Scroll {
            offset: if bottom { self.view(pane).max } else { 0 },
            follow: bottom && pane == Pane::Conversation,
        };
    }

    fn page(&self, pane: Pane) -> usize {
        self.view(pane).height.saturating_sub(1).max(1)
    }

    fn visible(&self, pane: Pane) -> bool {
        pane != Pane::Trace || self.show_trace
    }

    fn cycle_focus(&mut self, forward: bool) {
        let mut index = self.focus as usize;
        loop {
            index = if forward {
                (index + 1) % PANES.len()
            } else {
                (index + PANES.len() - 1) % PANES.len()
            };
            if self.visible(PANES[index]) {
                self.focus = PANES[index];
                return;
            }
        }
    }

    fn toggle_trace(&mut self) {
        self.show_trace = !self.show_trace;
        if !self.show_trace && self.focus == Pane::Trace {
            self.focus = Pane::Conversation;
        }
    }

    fn set_trace(&mut self, trace: Value) {
        self.trace = Some(trace);
        self.scrolls[Pane::Trace as usize] = Scroll::default();
    }

    fn pane_at(&self, column: u16, row: u16) -> Option<Pane> {
        PANES.into_iter().find(|pane| {
            self.visible(*pane) && self.view(*pane).area.contains(Position::new(column, row))
        })
    }

    fn set_mouse(&mut self, on: bool) {
        self.mouse = on;
        let _ = if on {
            crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)
        } else {
            crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture)
        };
    }

    /// Run a request in the background and feed its result back as a message.
    fn spawn<F, Fut>(&self, work: F)
    where
        F: FnOnce(Client) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Msg> + Send,
    {
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(work(client).await);
        });
    }

    fn call(&self, method: Method, path: String, body: Option<Value>, ok: &'static str) {
        self.spawn(move |client| async move {
            match client.json(method, &path, body).await {
                Ok(value) => {
                    let note = value
                        .get("note")
                        .and_then(Value::as_str)
                        .map(|n| format!("{ok}: {n}"))
                        .unwrap_or_else(|| ok.to_string());
                    Msg::Notice(note, false)
                }
                Err(error) => Msg::Notice(error, true),
            }
        });
    }

    pub fn resync(&self) {
        let limit = self.options.history_turns;
        self.spawn(move |client| async move {
            match client.get(&format!("/v1/turns?limit={limit}")).await {
                Ok(Value::Array(turns)) => Msg::Turns(turns),
                Ok(_) => Msg::Turns(vec![]),
                Err(error) => Msg::Notice(error, true),
            }
        });
        self.spawn(|client| async move {
            match client.get("/v1/outbound?status=pending&limit=100").await {
                Ok(Value::Array(items)) => Msg::Inbox(items),
                Ok(_) => Msg::Inbox(vec![]),
                Err(error) => Msg::Notice(error, true),
            }
        });
        self.spawn(|client| async move {
            match client.get("/v1/runtime").await {
                Ok(value) => Msg::Status(value),
                Err(error) => Msg::Notice(error, true),
            }
        });
    }

    pub fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Input(event) => self.on_input(event),
            Msg::Event(event) => self.on_event(event),
            Msg::Connected(connected) => {
                self.connected = connected;
                self.status = if connected {
                    "connected — type /help for commands".into()
                } else {
                    "disconnected — retrying…".into()
                };
            }
            Msg::Resync => self.resync(),
            Msg::Notice(text, is_error) => {
                if is_error {
                    self.error(text);
                } else {
                    self.system(text);
                }
            }
            Msg::Turns(turns) => {
                for turn in turns {
                    self.add_turn(&turn);
                }
            }
            Msg::Inbox(items) => {
                self.inbox = items;
                self.inbox.reverse();
            }
            Msg::Status(status) => {
                self.paused = status["paused"].as_bool().unwrap_or(false);
                self.resting = status["resting"].as_bool().unwrap_or(false);
                self.set_levels(status["levels"].clone());
            }
            Msg::Chat(result) => {
                // The stream normally delivers these first; dedupe by id.
                self.add_turn(&result["owner"]);
                self.add_turn(&result["aster"]);
                self.set_trace(result["trace"].clone());
                self.set_levels(result["levels"].clone());
            }
            Msg::Print(lines) => {
                for line in lines {
                    self.system(line);
                }
            }
        }
    }

    fn set_levels(&mut self, levels: Value) {
        if !levels.is_object() {
            return;
        }
        for name in LEVELS {
            if let Some(value) = levels[name].as_f64() {
                let series = self.level_history.entry(name).or_default();
                series.push_back(value);
                while series.len() > SPARK_POINTS {
                    series.pop_front();
                }
            }
        }
        self.levels = Some(levels);
    }

    fn add_turn(&mut self, turn: &Value) {
        let Some(id) = turn["id"].as_str() else {
            return;
        };
        if !self.seen_turns.insert(id.to_string()) {
            return;
        }
        let who = if turn["role"] == "aster" {
            Who::Aster
        } else {
            Who::Owner
        };
        let text = turn["text"].as_str().unwrap_or("").to_string();
        self.push(
            who,
            text,
            time_of(&turn["created_at"]),
            Some(id.to_string()),
        );
        if who == Who::Aster {
            self.last_aster_turn = Some(id.to_string());
            if self.mode == Mode::Train && self.options.training_prompt_after_reply {
                self.awaiting_rating = true;
                self.status = "rate the reply: + good · - bad · /correct <better reply>".into();
            }
        }
    }

    fn on_event(&mut self, envelope: Value) {
        match envelope["type"].as_str().unwrap_or("") {
            "turn" => self.add_turn(&envelope["turn"]),
            "thought" => self.set_trace(envelope["trace"].clone()),
            "levels" => self.set_levels(envelope["state"].clone()),
            "outbound" => {
                let item = envelope["item"].clone();
                let id = item["id"].clone();
                let pending = item["status"] == "pending";
                let known = self.inbox.iter().any(|i| i["id"] == id);
                self.inbox.retain(|i| i["id"] != id);
                if pending {
                    if item["origin_turn"].is_null() && !known {
                        let label = if item["needs_approval"] == true {
                            "Aster requests"
                        } else {
                            "Aster asks"
                        };
                        let body = item["draft"]["body"].as_str().unwrap_or("").to_string();
                        self.push(
                            Who::Aster,
                            format!("[{label}] {body}"),
                            time_of(&envelope["at"]),
                            None,
                        );
                    }
                    self.inbox.push(item);
                } else if known {
                    self.system(format!(
                        "inbox: “{}” {}",
                        short(item["draft"]["body"].as_str().unwrap_or(""), 50),
                        item["status"].as_str().unwrap_or("resolved")
                    ));
                }
            }
            "memory" => {
                let action = envelope["action"].as_str().unwrap_or("");
                let detail = envelope["content"]
                    .as_str()
                    .map(|c| short(c, 60))
                    .unwrap_or_else(|| envelope["id"].as_str().unwrap_or("").to_string());
                self.system(format!("memory {action}: {detail}"));
            }
            "learned" => {
                let kind = envelope["kind"].as_str().unwrap_or("");
                let note = envelope["note"].as_str().unwrap_or("");
                if !note.is_empty() {
                    self.system(format!("learned ({kind}): {note}"));
                }
            }
            "config_changed" => {
                let keys: Vec<&str> = envelope["keys"]
                    .as_array()
                    .map(|k| k.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                let mut text = format!("config reloaded: {}", keys.join(", "));
                if envelope["restart_required"]
                    .as_array()
                    .is_some_and(|r| !r.is_empty())
                {
                    text.push_str(" (some keys need a server restart)");
                }
                self.system(text);
            }
            "config_rejected" => {
                let error = envelope["error"].as_str().unwrap_or("invalid");
                self.error(format!("config edit rejected, previous kept: {error}"));
            }
            "runtime" => {
                self.paused = envelope["paused"].as_bool().unwrap_or(self.paused);
                let message = envelope["message"].as_str().unwrap_or("").to_string();
                if message.starts_with("resting") {
                    self.resting = true;
                }
                self.system(message);
            }
            _ => {}
        }
    }

    fn on_input(&mut self, event: TermEvent) {
        if let TermEvent::Mouse(mouse) = event {
            let Some(pane) = self.pane_at(mouse.column, mouse.row) else {
                return;
            };
            match mouse.kind {
                MouseEventKind::ScrollUp => self.scroll_up(pane, WHEEL_LINES),
                MouseEventKind::ScrollDown => self.scroll_down(pane, WHEEL_LINES),
                MouseEventKind::Down(MouseButton::Left) => self.focus = pane,
                _ => {}
            }
            return;
        }
        let TermEvent::Key(key) = event else {
            return;
        };
        if key.kind != KeyEventKind::Press {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let line_step = key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
        let focus = self.focus;
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('d') if ctrl => self.quit = true,
            KeyCode::Char('t') if ctrl => self.toggle_trace(),
            KeyCode::Up if line_step => self.scroll_up(focus, 1),
            KeyCode::Down if line_step => self.scroll_down(focus, 1),
            KeyCode::Home if ctrl => self.scroll_to_end(focus, false),
            KeyCode::End if ctrl => self.scroll_to_end(focus, true),
            KeyCode::Char('u') if ctrl => {
                self.input.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c) => {
                let byte = self.byte_index();
                self.input.insert(byte, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                let byte = self.byte_index();
                self.input.remove(byte);
            }
            KeyCode::Delete if self.cursor < self.input.chars().count() => {
                let byte = self.byte_index();
                self.input.remove(byte);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.input.chars().count()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.chars().count(),
            KeyCode::Esc => {
                self.input.clear();
                self.cursor = 0;
            }
            KeyCode::PageUp => self.scroll_up(focus, self.page(focus)),
            KeyCode::PageDown => self.scroll_down(focus, self.page(focus)),
            KeyCode::Up => self.recall_history(true),
            KeyCode::Down => self.recall_history(false),
            KeyCode::Tab => self.cycle_focus(true),
            KeyCode::BackTab => self.cycle_focus(false),
            KeyCode::Enter => {
                // Sending jumps back to the latest conversation.
                self.scroll_to_end(Pane::Conversation, true);
                let line = std::mem::take(&mut self.input);
                self.cursor = 0;
                self.history_pos = None;
                let line = line.trim().to_string();
                if !line.is_empty() {
                    if self.history.last() != Some(&line) {
                        self.history.push(line.clone());
                    }
                    self.submit(&line);
                }
            }
            _ => {}
        }
    }

    fn byte_index(&self) -> usize {
        self.input
            .char_indices()
            .nth(self.cursor)
            .map_or(self.input.len(), |(i, _)| i)
    }

    fn recall_history(&mut self, older: bool) {
        if self.history.is_empty() {
            return;
        }
        let next = match (self.history_pos, older) {
            (None, true) => Some(self.history.len() - 1),
            (None, false) => None,
            (Some(0), true) => Some(0),
            (Some(i), true) => Some(i - 1),
            (Some(i), false) if i + 1 < self.history.len() => Some(i + 1),
            (Some(_), false) => None,
        };
        self.history_pos = next;
        self.input = next.map(|i| self.history[i].clone()).unwrap_or_default();
        self.cursor = self.input.chars().count();
    }

    fn inbox_item(&mut self, index: &str) -> Option<Value> {
        let item = index
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|n| self.inbox.get(n).cloned());
        if item.is_none() {
            self.error(format!("no inbox item {index}; see the inbox panel"));
        }
        item
    }

    fn rate(&mut self, rating: i8, correction: Option<String>) {
        let Some(turn_id) = self.last_aster_turn.clone() else {
            self.error("no reply to rate yet");
            return;
        };
        if let Some(entry) = self
            .transcript
            .iter_mut()
            .rev()
            .find(|e| e.turn_id.as_deref() == Some(turn_id.as_str()))
        {
            entry.rating = Some(rating);
        }
        self.awaiting_rating = false;
        self.status = "thanks — feedback sent".into();
        let mut body = json!({ "turn_id": turn_id, "rating": rating });
        if let Some(correction) = correction {
            body["correction"] = json!(correction);
        }
        self.call(
            Method::POST,
            "/v1/feedback".into(),
            Some(body),
            "feedback recorded",
        );
    }

    fn submit(&mut self, line: &str) {
        if self.awaiting_rating && (line == "+" || line == "-") {
            self.rate(if line == "+" { 1 } else { -1 }, None);
            return;
        }
        let Some(command) = line.strip_prefix('/') else {
            self.awaiting_rating = false;
            let mode = if self.mode == Mode::Train {
                "train"
            } else {
                "chat"
            };
            let body = json!({ "text": line, "mode": mode });
            self.status = "Aster is thinking…".into();
            self.spawn(move |client| async move {
                match client.post("/v1/chat", body).await {
                    Ok(result) => Msg::Chat(result),
                    Err(error) => Msg::Notice(error, true),
                }
            });
            return;
        };
        let (name, rest) = command
            .split_once(char::is_whitespace)
            .map(|(n, r)| (n, r.trim()))
            .unwrap_or((command, ""));
        match name {
            "help" | "?" => {
                for line in HELP {
                    self.system(*line);
                }
            }
            "quit" | "exit" | "q" => self.quit = true,
            "clear" => self.transcript.clear(),
            "train" => {
                self.mode = Mode::Train;
                self.system("training mode: rate each reply with + / - or /correct <reply>");
            }
            "chat" => {
                self.mode = Mode::Chat;
                self.awaiting_rating = false;
                self.system("chat mode");
            }
            "trace" => self.toggle_trace(),
            "mouse" => {
                let on = !self.mouse;
                self.set_mouse(on);
                self.system(if on {
                    "mouse on: wheel scrolls the pane under the pointer, click focuses it"
                } else {
                    "mouse off: drag to select and copy text; /mouse turns scrolling back on"
                });
            }
            "good" => self.rate(1, None),
            "bad" => self.rate(-1, None),
            "correct" if !rest.is_empty() => self.rate(-1, Some(rest.to_string())),
            "teach" => match rest.split_once("=>") {
                Some((prompt, reply)) if !prompt.trim().is_empty() && !reply.trim().is_empty() => {
                    let body = json!({ "prompt": prompt.trim(), "reply": reply.trim() });
                    self.call(Method::POST, "/v1/teach".into(), Some(body), "taught");
                }
                _ => self.error("usage: /teach <prompt> => <reply>"),
            },
            "answer" => match rest.split_once(char::is_whitespace) {
                Some((index, text)) if !text.trim().is_empty() => {
                    if let Some(item) = self.inbox_item(index) {
                        let path =
                            format!("/v1/outbound/{}/answer", item["id"].as_str().unwrap_or(""));
                        self.call(
                            Method::POST,
                            path,
                            Some(json!({ "text": text.trim() })),
                            "answered",
                        );
                    }
                }
                _ => self.error("usage: /answer <n> <text>"),
            },
            "approve" | "deny" => {
                if let Some(item) = self.inbox_item(rest) {
                    let path = format!("/v1/outbound/{}/{name}", item["id"].as_str().unwrap_or(""));
                    let ok = if name == "approve" {
                        "approved"
                    } else {
                        "denied"
                    };
                    self.call(Method::POST, path, None, ok);
                }
            }
            "remember" if !rest.is_empty() => {
                let body = json!({ "content": rest, "provenance": "owner via console" });
                self.call(
                    Method::POST,
                    "/v1/memories".into(),
                    Some(body),
                    "remembered",
                );
            }
            "forget" if !rest.is_empty() => {
                self.call(
                    Method::DELETE,
                    format!("/v1/memories/{rest}"),
                    None,
                    "forgotten",
                );
            }
            "ingest" if !rest.is_empty() => {
                self.system(format!("ingesting {rest}…"));
                self.call(
                    Method::POST,
                    "/v1/ingest".into(),
                    Some(json!({ "path": rest })),
                    "ingested",
                );
            }
            "set" => match rest.split_once(char::is_whitespace) {
                Some((key, value)) if !value.trim().is_empty() => {
                    let body = json!({ key: format!("={}", value.trim()) });
                    self.spawn(move |client| async move {
                        match client.json(Method::PATCH, "/v1/config", Some(body)).await {
                            Ok(result) => {
                                Msg::Notice(format!("set: changed {}", result["changed"]), false)
                            }
                            Err(error) => Msg::Notice(error, true),
                        }
                    });
                }
                _ => self.error("usage: /set <section.key> <value>"),
            },
            "get" | "config" => {
                let key = rest.to_string();
                self.spawn(move |client| async move {
                    match client.get("/v1/config").await {
                        Ok(value) => {
                            let mut node = &value["config"];
                            for part in key.split('.').filter(|p| !p.is_empty()) {
                                node = &node[part];
                            }
                            let mut lines = vec![format!(
                                "{} = (from {})",
                                if key.is_empty() { "config" } else { &key },
                                value["path"].as_str().unwrap_or("aster.toml")
                            )];
                            let pretty = serde_json::to_string_pretty(node).unwrap_or_default();
                            lines.extend(pretty.lines().map(|l| format!("  {l}")));
                            Msg::Print(lines)
                        }
                        Err(error) => Msg::Notice(error, true),
                    }
                });
            }
            "pause" | "resume" => {
                self.call(
                    Method::POST,
                    format!("/v1/runtime/{name}"),
                    None,
                    if name == "pause" { "paused" } else { "resumed" },
                );
            }
            "status" => self.spawn(|client| async move {
                match client.get("/v1/runtime").await {
                    Ok(status) => {
                        let mut lines = vec![format!(
                            "engine {} · paused {} · resting {} · idle {}s",
                            status["engine"].as_str().unwrap_or("?"),
                            status["paused"],
                            status["resting"],
                            status["idle_secs"]
                        )];
                        if let Some(mind) = status["mind"].as_object() {
                            for (key, value) in mind {
                                lines.push(format!(
                                    "  {key}: {}",
                                    value
                                        .as_f64()
                                        .map(|v| format!("{v:.3}"))
                                        .unwrap_or_default()
                                ));
                            }
                        }
                        Msg::Print(lines)
                    }
                    Err(error) => Msg::Notice(error, true),
                }
            }),
            "history" => self.spawn(|client| async move {
                match client.get("/v1/state/history?limit=8").await {
                    Ok(Value::Array(entries)) => Msg::Print(
                        entries
                            .iter()
                            .map(|e| {
                                let levels = LEVELS
                                    .iter()
                                    .map(|l| {
                                        format!(
                                            "{}={:.0}",
                                            &l[..3],
                                            e["state"][l].as_f64().unwrap_or(0.0)
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                format!(
                                    "{} {:<26} {levels}",
                                    time_of(&e["recorded_at"]),
                                    e["cause"].as_str().unwrap_or("")
                                )
                            })
                            .collect(),
                    ),
                    Ok(_) => Msg::Print(vec![]),
                    Err(error) => Msg::Notice(error, true),
                }
            }),
            "snapshot" => {
                let reason = if rest.is_empty() { "manual" } else { rest };
                let body = json!({ "reason": reason });
                self.spawn(move |client| async move {
                    match client.post("/v1/mind/snapshots", body).await {
                        Ok(info) => {
                            Msg::Notice(format!("snapshot v{} saved", info["version"]), false)
                        }
                        Err(error) => Msg::Notice(error, true),
                    }
                });
            }
            "snapshots" => self.spawn(|client| async move {
                match client.get("/v1/mind/snapshots?limit=10").await {
                    Ok(Value::Array(items)) => Msg::Print(
                        items
                            .iter()
                            .map(|s| {
                                format!(
                                    "v{} {} {} · associations {} · vocabulary {}",
                                    s["version"],
                                    time_of(&s["created_at"]),
                                    s["reason"].as_str().unwrap_or(""),
                                    s["stats"]["associations"],
                                    s["stats"]["vocabulary"]
                                )
                            })
                            .collect(),
                    ),
                    Ok(_) => Msg::Print(vec![]),
                    Err(error) => Msg::Notice(error, true),
                }
            }),
            "rollback" => match rest.trim_start_matches('v').parse::<i64>() {
                Ok(version) => self.spawn(move |client| async move {
                    match client
                        .post("/v1/mind/rollback", json!({ "version": version }))
                        .await
                    {
                        Ok(info) => Msg::Notice(
                            format!("rolled back to v{version}; saved as v{}", info["version"]),
                            false,
                        ),
                        Err(error) => Msg::Notice(error, true),
                    }
                }),
                Err(_) => self.error("usage: /rollback <version>"),
            },
            "logs" => {
                let mut parts = rest.split_whitespace();
                let category = parts.next().unwrap_or("conversations").to_string();
                let count: usize = parts.next().and_then(|n| n.parse().ok()).unwrap_or(10);
                self.spawn(move |client| async move { tail_log(client, category, count).await });
            }
            _ => self.error(format!("unknown or incomplete command /{name}; try /help")),
        }
    }
}

/// Read the tail of today's local log file for `category`.
async fn tail_log(client: Client, category: String, count: usize) -> Msg {
    let dir = match client.get("/v1/config").await {
        Ok(value) => value["config"]["logs"]["dir"]
            .as_str()
            .unwrap_or("data/logs")
            .to_string(),
        Err(error) => return Msg::Notice(error, true),
    };
    // Files are named by date, so the lexically last one is the newest.
    let today = latest_file(&format!("{dir}/{category}"));
    let Some(path) = today else {
        return Msg::Notice(format!("no log files under {dir}/{category}"), true);
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let lines: Vec<&str> = text.lines().collect();
            let start = lines.len().saturating_sub(count);
            let mut out = vec![format!("── {path} (last {}) ──", lines.len() - start)];
            out.extend(lines[start..].iter().map(|l| short(l, 200)));
            Msg::Print(out)
        }
        Err(_) => Msg::Notice(format!("cannot read {path}"), true),
    }
}

fn latest_file(dir: &str) -> Option<String> {
    let mut files: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path().display().to_string())
        .filter(|p| p.ends_with(".jsonl"))
        .collect();
    files.sort();
    files.pop()
}
