//! Interactive TUI renderer wired to the `agenty-repl` query loop.
//!
//! Layout (top to bottom):
//!   - message list (conversation history, with live streaming preview)
//!   - input box (single-line editor)
//!   - status bar

use std::collections::BTreeMap;
use std::io::{self, Stdout};

use agenty_providers::anthropic::{AnthropicStreamEvent, BlockKind};
use agenty_repl::Repl;
use agenty_types::{AgentError, ChatMessage, ContentBlock, Role, StopReason};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

type Term = Terminal<CrosstermBackend<Stdout>>;

const MAX_TURNS: usize = 20;

/// Run the TUI with a [`Repl`] instance. Blocks until the user quits
/// (Ctrl+C, Esc, `/exit`) or an error occurs.
pub async fn run(repl: Repl<'_>) -> Result<(), AgentError> {
    let mut terminal = init_terminal().map_err(io_err)?;
    let result = event_loop(&mut terminal, repl).await;
    let _ = restore_terminal(&mut terminal);
    result
}

fn init_terminal() -> io::Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore_terminal(terminal: &mut Term) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

enum Status {
    Hint(String),
    Streaming,
    RunningTools,
    Info(String),
    Error(String),
}

struct AppState {
    input: String,
    status: Status,
}

impl AppState {
    fn new() -> Self {
        Self {
            input: String::new(),
            status: Status::Hint("Enter to send · Ctrl+C to quit".into()),
        }
    }
}

enum KeyAction {
    None,
    Quit,
    Submit(String),
}

/// Per-block streaming buffer, keyed by the block `index` assigned by the
/// provider. Order is preserved because we use `BTreeMap`.
enum StreamingBlock {
    Text(String),
    Thinking(String),
    ToolUse { id: String, name: String, partial_json: String },
}

struct StreamingState {
    active: bool,
    blocks: BTreeMap<u32, StreamingBlock>,
}

impl StreamingState {
    fn new() -> Self {
        Self { active: false, blocks: BTreeMap::new() }
    }

    fn start(&mut self) {
        self.active = true;
        self.blocks.clear();
    }

    fn stop(&mut self) {
        self.active = false;
        self.blocks.clear();
    }

    fn apply(&mut self, event: &AnthropicStreamEvent) {
        match event {
            AnthropicStreamEvent::BlockStart { index, kind } => {
                let block = match kind {
                    BlockKind::Text => StreamingBlock::Text(String::new()),
                    BlockKind::Thinking => StreamingBlock::Thinking(String::new()),
                    BlockKind::ToolUse { id, name } => StreamingBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        partial_json: String::new(),
                    },
                };
                self.blocks.insert(*index, block);
            }
            AnthropicStreamEvent::TextDelta { index, text } => {
                if let Some(StreamingBlock::Text(buf)) = self.blocks.get_mut(index) {
                    buf.push_str(text);
                }
            }
            AnthropicStreamEvent::ThinkingDelta { index, text } => {
                if let Some(StreamingBlock::Thinking(buf)) = self.blocks.get_mut(index) {
                    buf.push_str(text);
                }
            }
            AnthropicStreamEvent::ToolInputDelta { index, partial_json } => {
                if let Some(StreamingBlock::ToolUse { partial_json: buf, .. }) =
                    self.blocks.get_mut(index)
                {
                    buf.push_str(partial_json);
                }
            }
            AnthropicStreamEvent::BlockStop { .. }
            | AnthropicStreamEvent::StopReason(_)
            | AnthropicStreamEvent::MessageStop => {}
        }
    }

    /// Build the assistant `ContentBlock`s from the streamed blocks. Thinking
    /// blocks are dropped (sending them back to the API requires a signature
    /// we don't currently capture).
    fn finalize(&self) -> Vec<ContentBlock> {
        self.blocks
            .values()
            .filter_map(|b| match b {
                StreamingBlock::Text(text) => {
                    Some(ContentBlock::Text { text: text.clone() })
                }
                StreamingBlock::ToolUse { id, name, partial_json } => {
                    let input = if partial_json.is_empty() {
                        serde_json::Value::Object(serde_json::Map::new())
                    } else {
                        serde_json::from_str(partial_json)
                            .unwrap_or(serde_json::Value::Null)
                    };
                    Some(ContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input,
                    })
                }
                StreamingBlock::Thinking(_) => None,
            })
            .collect()
    }
}

async fn event_loop(terminal: &mut Term, repl: Repl<'_>) -> Result<(), AgentError> {
    let mut app = AppState::new();
    let mut events = EventStream::new();
    let mut conversation: Vec<ChatMessage> = Vec::new();
    let mut streaming = StreamingState::new();

    loop {
        terminal
            .draw(|f| draw(f, &conversation, &streaming, &app))
            .map_err(io_err)?;

        let Some(event) = events.next().await else {
            return Ok(());
        };
        let event =
            event.map_err(|e| AgentError::Other(format!("terminal event error: {e}")))?;

        let Event::Key(key) = event else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match handle_key(key, &mut app) {
            KeyAction::None => {}
            KeyAction::Quit => return Ok(()),
            KeyAction::Submit(input) => {
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    continue;
                }

                if let Some(action) = dispatch_slash(trimmed) {
                    match action {
                        SlashAction::Clear => {
                            conversation.clear();
                            app.status = Status::Info("conversation cleared".into());
                        }
                        SlashAction::Exit => return Ok(()),
                        SlashAction::Unknown(name) => {
                            app.status =
                                Status::Error(format!("unknown command: /{name}"));
                        }
                    }
                    continue;
                }

                repl.add_user_message(&mut conversation, trimmed);

                if let Err(e) = run_agent_loop(
                    terminal,
                    &repl,
                    &mut conversation,
                    &mut streaming,
                    &mut app,
                    &mut events,
                )
                .await
                {
                    app.status = Status::Error(e.to_string());
                }
            }
        }
    }
}

enum SlashAction {
    Clear,
    Exit,
    Unknown(String),
}

fn dispatch_slash(input: &str) -> Option<SlashAction> {
    let rest = input.strip_prefix('/')?;
    let name = rest.split_whitespace().next().unwrap_or("");
    Some(match name {
        "clear" => SlashAction::Clear,
        "exit" | "quit" => SlashAction::Exit,
        other => SlashAction::Unknown(other.to_string()),
    })
}

/// Run one user turn: loop through streaming assistant responses and any tool
/// calls they produce, until the model stops calling tools (or we hit the cap).
async fn run_agent_loop(
    terminal: &mut Term,
    repl: &Repl<'_>,
    conversation: &mut Vec<ChatMessage>,
    streaming: &mut StreamingState,
    app: &mut AppState,
    events: &mut EventStream,
) -> Result<(), AgentError> {
    for _ in 0..MAX_TURNS {
        app.status = Status::Streaming;
        streaming.start();
        terminal
            .draw(|f| draw(f, conversation, streaming, app))
            .map_err(io_err)?;

        let stop_reason =
            stream_one_turn(terminal, repl, conversation, streaming, app, events).await?;

        let assistant_content = streaming.finalize();
        streaming.stop();
        conversation.push(ChatMessage::assistant(assistant_content.clone()));

        if stop_reason != StopReason::ToolUse {
            app.status = Status::Hint("Enter to send · Ctrl+C to quit".into());
            return Ok(());
        }

        app.status = Status::RunningTools;
        terminal
            .draw(|f| draw(f, conversation, streaming, app))
            .map_err(io_err)?;

        let tool_results = repl.run_tool_calls(&assistant_content);
        if tool_results.is_empty() {
            app.status = Status::Hint("Enter to send · Ctrl+C to quit".into());
            return Ok(());
        }
        conversation.push(ChatMessage::user(tool_results));
    }

    Err(AgentError::Other(format!(
        "agent exceeded max turns = {MAX_TURNS}"
    )))
}

/// Drive one streaming request, applying deltas to `streaming` and redrawing
/// between events. Also polls the keyboard so Ctrl+C / Esc can bail early.
async fn stream_one_turn(
    terminal: &mut Term,
    repl: &Repl<'_>,
    conversation: &[ChatMessage],
    streaming: &mut StreamingState,
    app: &mut AppState,
    events: &mut EventStream,
) -> Result<StopReason, AgentError> {
    let specs = repl.tool_specs();
    let stream = repl
        .client()
        .stream_with_tools(repl.config(), conversation, &specs)
        .await?;
    futures::pin_mut!(stream);

    let mut stop_reason = StopReason::EndTurn;

    loop {
        tokio::select! {
            biased;
            maybe_key = events.next() => {
                let Some(event) = maybe_key else { continue };
                let event = event.map_err(|e| {
                    AgentError::Other(format!("terminal event error: {e}"))
                })?;
                if let Event::Key(key) = event {
                    if key.kind == KeyEventKind::Press
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
                    {
                        return Err(AgentError::Other("cancelled".into()));
                    }
                }
            }
            next = stream.next() => {
                match next {
                    Some(Ok(event)) => {
                        let done = matches!(event, AnthropicStreamEvent::MessageStop);
                        if let AnthropicStreamEvent::StopReason(reason) = &event {
                            stop_reason = *reason;
                        }
                        streaming.apply(&event);
                        terminal
                            .draw(|f| draw(f, conversation, streaming, app))
                            .map_err(io_err)?;
                        if done {
                            return Ok(stop_reason);
                        }
                    }
                    Some(Err(e)) => return Err(e),
                    None => return Ok(stop_reason),
                }
            }
        }
    }
}

fn handle_key(key: KeyEvent, app: &mut AppState) -> KeyAction {
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
    {
        return KeyAction::Quit;
    }
    match key.code {
        KeyCode::Esc => KeyAction::Quit,
        KeyCode::Enter => {
            if app.input.trim().is_empty() {
                return KeyAction::None;
            }
            KeyAction::Submit(std::mem::take(&mut app.input))
        }
        KeyCode::Char(c) => {
            app.input.push(c);
            KeyAction::None
        }
        KeyCode::Backspace => {
            app.input.pop();
            KeyAction::None
        }
        _ => KeyAction::None,
    }
}

fn draw(
    f: &mut Frame,
    conversation: &[ChatMessage],
    streaming: &StreamingState,
    app: &AppState,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(f.area());

    render_messages(f, chunks[0], conversation, streaming);
    render_input(f, chunks[1], app);
    render_status(f, chunks[2], app);
}

fn render_messages(
    f: &mut Frame,
    area: Rect,
    conversation: &[ChatMessage],
    streaming: &StreamingState,
) {
    let mut lines: Vec<Line> = Vec::new();

    for msg in conversation {
        let (label, colour) = match msg.role {
            Role::User => ("You", Color::Cyan),
            Role::Assistant => ("Assistant", Color::Green),
            Role::Tool => ("Tool", Color::Yellow),
        };
        lines.push(Line::from(Span::styled(
            format!("{label}:"),
            Style::default().fg(colour).add_modifier(Modifier::BOLD),
        )));

        for block in &msg.content {
            render_content_block(block, &mut lines);
        }
        lines.push(Line::raw(""));
    }

    if streaming.active && !streaming.blocks.is_empty() {
        lines.push(Line::from(Span::styled(
            "Assistant:",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )));
        for block in streaming.blocks.values() {
            render_streaming_block(block, &mut lines);
        }
        lines.push(Line::styled(
            "  [streaming…]",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ));
    }

    let total = lines.len() as u16;
    let visible = area.height.saturating_sub(2);
    let scroll_y = total.saturating_sub(visible);

    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Conversation "),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll_y, 0));
    f.render_widget(para, area);
}

fn render_content_block(block: &ContentBlock, lines: &mut Vec<Line>) {
    match block {
        ContentBlock::Text { text } => {
            for l in text.lines() {
                lines.push(Line::raw(l.to_string()));
            }
            if text.is_empty() {
                lines.push(Line::raw(""));
            }
        }
        ContentBlock::ToolUse { name, input, .. } => {
            lines.push(Line::styled(
                format!("  → tool_use: {name}({input})"),
                Style::default().fg(Color::Yellow),
            ));
        }
        ContentBlock::ToolResult { content, is_error, .. } => {
            let (marker, style) = if *is_error {
                ("✗", Style::default().fg(Color::Red))
            } else {
                ("✓", Style::default().fg(Color::DarkGray))
            };
            lines.push(Line::styled(
                format!("  {marker} tool_result: {content}"),
                style,
            ));
        }
    }
}

fn render_streaming_block(block: &StreamingBlock, lines: &mut Vec<Line>) {
    match block {
        StreamingBlock::Text(text) => {
            for l in text.lines() {
                lines.push(Line::raw(l.to_string()));
            }
        }
        StreamingBlock::Thinking(text) => {
            lines.push(Line::styled(
                "  thinking:",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ));
            for l in text.lines() {
                lines.push(Line::styled(
                    format!("    {l}"),
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::ITALIC),
                ));
            }
        }
        StreamingBlock::ToolUse { name, partial_json, .. } => {
            lines.push(Line::styled(
                format!("  → tool_use: {name}({partial_json})"),
                Style::default().fg(Color::Yellow),
            ));
        }
    }
}

fn render_input(f: &mut Frame, area: Rect, app: &AppState) {
    let para = Paragraph::new(app.input.as_str())
        .block(Block::default().borders(Borders::ALL).title(" Input "));
    f.render_widget(para, area);

    let max_x = area.x + area.width.saturating_sub(2);
    let cursor_x = (area.x + 1 + app.input.len() as u16).min(max_x);
    f.set_cursor_position((cursor_x, area.y + 1));
}

fn render_status(f: &mut Frame, area: Rect, app: &AppState) {
    let (text, style) = match &app.status {
        Status::Hint(t) => (t.clone(), Style::default().fg(Color::DarkGray)),
        Status::Streaming => (
            "streaming…".to_string(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Status::RunningTools => (
            "running tools…".to_string(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Status::Info(t) => (format!("ⓘ  {t}"), Style::default().fg(Color::Blue)),
        Status::Error(t) => (format!("✗ {t}"), Style::default().fg(Color::Red)),
    };
    f.render_widget(Paragraph::new(text).style(style), area);
}

fn io_err(e: io::Error) -> AgentError {
    AgentError::Other(format!("terminal I/O error: {e}"))
}
