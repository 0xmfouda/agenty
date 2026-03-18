//! Interactive TUI renderer wired to the `agenty-repl` query loop.
//!
//! Layout (top to bottom):
//!   - message list (conversation history)
//!   - input box (single-line editor)
//!   - status bar

use std::io::{self, Stdout};
use std::collections::HashMap;

use agenty_repl::{Repl, StreamDelta};
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
use tokio::sync::mpsc;

type Term = Terminal<CrosstermBackend<Stdout>>;

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
    Thinking,
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
            status: Status::Hint(
                "Enter to send · Ctrl+C to quit".into(),
            ),
        }
    }
}

enum KeyAction {
    None,
    Quit,
    Submit(String),
}

struct StreamingState {
    is_streaming: bool,
    partial_text: HashMap<u32, String>,
    partial_tool_inputs: HashMap<u32, String>,
    active_tool_uses: HashMap<u32, (String, String)>,
}

impl StreamingState {
    fn new() -> Self {
        Self {
            is_streaming: false,
            partial_text: HashMap::new(),
            partial_tool_inputs: HashMap::new(),
            active_tool_uses: HashMap::new(),
        }
    }

    fn reset(&mut self) {
        self.is_streaming = false;
        self.partial_text.clear();
        self.partial_tool_inputs.clear();
        self.active_tool_uses.clear();
    }

    fn handle_delta(&mut self, delta: StreamDelta) {
        match delta {
            StreamDelta::TextDelta { index, text } => {
                self.partial_text
                    .entry(index)
                    .or_insert_with(String::new)
                    .push_str(&text);
            }
            StreamDelta::ToolUseStart { index, id, name } => {
                self.active_tool_uses.insert(index, (id, name));
            }
            StreamDelta::ToolInputDelta { index, partial_json } => {
                self.partial_tool_inputs
                    .entry(index)
                    .or_insert_with(String::new)
                    .push_str(&partial_json);
            }
            StreamDelta::BlockStop { index } => {
                self.partial_text.remove(&index);
                self.partial_tool_inputs.remove(&index);
                self.active_tool_uses.remove(&index);
            }
            StreamDelta::MessageComplete { .. } => {
                self.reset();
            }
            StreamDelta::Error(_) => {
                self.reset();
            }
        }
    }
}

async fn event_loop(
    terminal: &mut Term,
    repl: Repl<'_>,
) -> Result<(), AgentError> {
    let mut app = AppState::new();
    let mut events = EventStream::new();
    let mut conversation: Vec<ChatMessage> = Vec::new();
    let mut streaming_state = StreamingState::new();

    let (tx, mut rx) = mpsc::channel::<StreamDelta>(100);

    loop {
        terminal
            .draw(|f| draw(f, &conversation, &streaming_state, &app))
            .map_err(io_err)?;

        tokio::select! {
            Some(event) = events.next() => {
                let event = event.map_err(|e| {
                    AgentError::Other(format!("terminal event error: {e}"))
                })?;

                let Event::Key(key) = event else { continue };
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match handle_key(key, &mut app) {
                    KeyAction::None => {}
                    KeyAction::Quit => return Ok(()),
                    KeyAction::Submit(input) => {
                        if input.trim().is_empty() {
                            continue;
                        }
                        if input.starts_with('/') {
                            match input.trim() {
                                "/clear" => {
                                    conversation.clear();
                                    app.status = Status::Info("conversation cleared".into());
                                }
                                "/exit" | "/quit" => return Ok(()),
                                _ => {
                                    app.status = Status::Error(format!("unknown command: {}", input));
                                }
                            }
                            continue;
                        }

                        app.status = Status::Thinking;
                        streaming_state.reset();
                        streaming_state.is_streaming = true;

                        let (stop_reason, new_content) = run_streaming_turn(
                            &repl,
                            &mut conversation,
                            &input,
                            tx.clone(),
                            &mut streaming_state,
                        )
                        .await;

                        match stop_reason {
                            Ok(StopReason::ToolUse) => {
                                let tool_results = repl.run_tool_calls(&new_content);
                                if !tool_results.is_empty() {
                                    conversation.push(ChatMessage::assistant(new_content));
                                    conversation.push(ChatMessage::user(tool_results));
                                }
                                app.status = Status::Hint(
                                    "Enter to send · Ctrl+C to quit".into(),
                                );
                            }
                            Ok(_) => {
                                conversation.push(ChatMessage::assistant(new_content));
                                app.status = Status::Hint(
                                    "Enter to send · Ctrl+C to quit".into(),
                                );
                            }
                            Err(e) => {
                                app.status = Status::Error(e.to_string());
                            }
                        }
                    }
                }
            }
            Some(delta) = rx.recv() => {
                streaming_state.handle_delta(delta);
                terminal
                    .draw(|f| draw(f, &conversation, &streaming_state, &app))
                    .map_err(io_err)?;
            }
            else => return Ok(()),
        }
    }
}

async fn run_streaming_turn(
    repl: &Repl<'_>,
    conversation: &mut Vec<ChatMessage>,
    prompt: &str,
    tx: mpsc::Sender<StreamDelta>,
    streaming_state: &mut StreamingState,
) -> (Result<StopReason, AgentError>, Vec<ContentBlock>) {
    let mut new_content = Vec::new();
    let mut stop_reason: Option<StopReason> = None;
    let mut error_message: Option<String> = None;

    let stream = match repl.client().stream_with_tools(
        repl.config(),
        conversation,
        &repl.tool_specs(),
    ).await {
        Ok(s) => s,
        Err(e) => return (Err(e), vec![]),
    };

    repl.add_user_message(conversation, prompt);

    futures::pin_mut!(stream);

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => {
                match event {
                    agenty_providers::anthropic::AnthropicStreamEvent::BlockStart { index, kind } => {
                        if let agenty_providers::anthropic::BlockKind::ToolUse { id, name } = kind {
                            let _ = tx.send(StreamDelta::ToolUseStart { index, id, name }).await;
                        }
                    }
                    agenty_providers::anthropic::AnthropicStreamEvent::TextDelta { index, text } => {
                        streaming_state.partial_text
                            .entry(index)
                            .or_insert_with(String::new)
                            .push_str(&text);
                        let _ = tx.send(StreamDelta::TextDelta { index, text }).await;
                    }
                    agenty_providers::anthropic::AnthropicStreamEvent::ToolInputDelta { index, partial_json } => {
                        streaming_state.partial_tool_inputs
                            .entry(index)
                            .or_insert_with(String::new)
                            .push_str(&partial_json);
                        let _ = tx.send(StreamDelta::ToolInputDelta { index, partial_json }).await;
                    }
                    agenty_providers::anthropic::AnthropicStreamEvent::BlockStop { index } => {
                        streaming_state.partial_text.remove(&index);
                        streaming_state.partial_tool_inputs.remove(&index);
                        let _ = tx.send(StreamDelta::BlockStop { index }).await;
                    }
                    agenty_providers::anthropic::AnthropicStreamEvent::StopReason(reason) => {
                        stop_reason = Some(reason);
                    }
                    agenty_providers::anthropic::AnthropicStreamEvent::MessageStop => {
                        new_content = finalize_streaming_content(streaming_state);
                        let reason = stop_reason.unwrap_or(StopReason::EndTurn);
                        let _ = tx.send(StreamDelta::MessageComplete { content: new_content.clone(), stop_reason: reason }).await;
                        break;
                    }
                }
            }
            Err(e) => {
                error_message = Some(e.to_string());
                let _ = tx.send(StreamDelta::Error(error_message.clone().unwrap())).await;
                break;
            }
        }
    }

    let final_stop_reason = match error_message {
        Some(msg) => Err(AgentError::Provider(msg)),
        None => Ok(stop_reason.unwrap_or(StopReason::EndTurn)),
    };

    (final_stop_reason, new_content)
}

fn finalize_streaming_content(streaming_state: &StreamingState) -> Vec<ContentBlock> {
    let mut content = Vec::new();
    
    for (_index, text) in &streaming_state.partial_text {
        content.push(ContentBlock::Text { text: text.clone() });
    }
    
    for (index, (id, name)) in &streaming_state.active_tool_uses {
        let input_json = streaming_state.partial_tool_inputs.get(index)
            .map(|s| serde_json::from_str(s).unwrap_or(serde_json::Value::Null))
            .unwrap_or(serde_json::Value::Null);
        content.push(ContentBlock::ToolUse {
            id: id.clone(),
            name: name.clone(),
            input: input_json,
        });
    }
    
    content
}

fn handle_key(key: KeyEvent, app: &mut AppState) -> KeyAction {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        if matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d')) {
            return KeyAction::Quit;
        }
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

    if streaming.is_streaming {
        if !streaming.partial_text.is_empty() || !streaming.active_tool_uses.is_empty() {
            lines.push(Line::from(Span::styled(
                "Assistant:",
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            )));

            let mut sorted_indices: Vec<u32> = streaming.partial_text.keys().copied().collect();
            sorted_indices.sort();
            for index in sorted_indices {
                if let Some(text) = streaming.partial_text.get(&index) {
                    for l in text.lines() {
                        lines.push(Line::raw(l.to_string()));
                    }
                }
            }

            let mut sorted_tool_indices: Vec<u32> = streaming.active_tool_uses.keys().copied().collect();
            sorted_tool_indices.sort();
            for index in sorted_tool_indices {
                if let Some((_id, name)) = streaming.active_tool_uses.get(&index) {
                    let input = streaming.partial_tool_inputs.get(&index).cloned().unwrap_or_default();
                    lines.push(Line::styled(
                        format!("  -> tool_use: {name}({input})..."),
                        Style::default().fg(Color::Yellow),
                    ));
                }
            }

            lines.push(Line::styled(
                "  [streaming...]",
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            ));
        }
    }

    let total = lines.len() as u16;
    let visible = area.height.saturating_sub(2);
    let scroll_y = total.saturating_sub(visible);

    let para = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" Conversation "))
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
                format!("  -> tool_use: {name}({input})"),
                Style::default().fg(Color::Yellow),
            ));
        }
        ContentBlock::ToolResult { content, is_error, .. } => {
            let (marker, style) = if *is_error {
                ("x", Style::default().fg(Color::Red))
            } else {
                ("ok", Style::default().fg(Color::DarkGray))
            };
            lines.push(Line::styled(
                format!("  {marker} tool_result: {content}"),
                style,
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
        Status::Thinking => (
            "thinking...".to_string(),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Status::Info(t) => (format!("i  {t}"), Style::default().fg(Color::Blue)),
        Status::Error(t) => (format!("x {t}"), Style::default().fg(Color::Red)),
    };
    f.render_widget(Paragraph::new(text).style(style), area);
}

fn io_err(e: io::Error) -> AgentError {
    AgentError::Other(format!("terminal I/O error: {e}"))
}
