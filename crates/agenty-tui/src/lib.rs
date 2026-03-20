//! Interactive TUI renderer wired to the `agenty-repl` query loop.
//!
//! Layout, palette and message-box styling follow `UI.md` at the workspace
//! root. The UI is a scrollable stack of bordered message boxes on top, a
//! 3-row input bar, and a 1-row status bar.

use std::collections::BTreeMap;
use std::io::{self, Stdout};

use agenty_providers::anthropic::{AnthropicStreamEvent, BlockKind};
use agenty_repl::Repl;
use agenty_types::{AgentError, ChatMessage, ContentBlock, Role, StopReason};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

type Term = Terminal<CrosstermBackend<Stdout>>;

const MAX_TURNS: usize = 20;

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

mod theme {
    use ratatui::style::Color;

    pub const BG: Color = Color::Rgb(0x0f, 0x11, 0x17);
    pub const SURFACE: Color = Color::Rgb(0x0a, 0x0c, 0x10);
    pub const BORDER_DIM: Color = Color::Rgb(0x1c, 0x1f, 0x2b);
    pub const FG: Color = Color::Rgb(0xb4, 0xbc, 0xd0);
    pub const FG_BRIGHT: Color = Color::Rgb(0xc0, 0xca, 0xf5);
    pub const DIM: Color = Color::Rgb(0x6b, 0x73, 0x94);
    pub const BLUE: Color = Color::Rgb(0x3d, 0x5a, 0xfe);
    pub const PURPLE: Color = Color::Rgb(0x8b, 0x5c, 0xf6);
    pub const PURPLE_LIGHT: Color = Color::Rgb(0xa7, 0x8b, 0xfa);
    pub const AMBER: Color = Color::Rgb(0xf5, 0x9e, 0x0b);
    pub const GREEN: Color = Color::Rgb(0x10, 0xb9, 0x81);
    pub const GREEN_LIGHT: Color = Color::Rgb(0x34, 0xd3, 0x99);
    pub const RED: Color = Color::Rgb(0xef, 0x44, 0x44);
    pub const RED_LIGHT: Color = Color::Rgb(0xf8, 0x71, 0x71);
    pub const GRAY: Color = Color::Rgb(0x6b, 0x73, 0x94);
    pub const STATUS_BG: Color = Color::Rgb(0x12, 0x14, 0x1c);
    pub const INLINE_CODE_BG: Color = Color::Rgb(0x1a, 0x1d, 0x2e);
}

// ---------------------------------------------------------------------------
// ASCII banner (ANSI Shadow figlet font)
// ---------------------------------------------------------------------------

const BANNER: &str = " █████╗  ██████╗ ███████╗███╗   ██╗████████╗██╗   ██╗
██╔══██╗██╔════╝ ██╔════╝████╗  ██║╚══██╔══╝╚██╗ ██╔╝
███████║██║  ███╗█████╗  ██╔██╗ ██║   ██║    ╚████╔╝
██╔══██║██║   ██║██╔══╝  ██║╚██╗██║   ██║     ╚██╔╝
██║  ██║╚██████╔╝███████╗██║ ╚████║   ██║      ██║
╚═╝  ╚═╝ ╚═════╝ ╚══════╝╚═╝  ╚═══╝   ╚═╝      ╚═╝   ";
const BANNER_WIDTH: u16 = 52;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn run(repl: Repl<'_>) -> Result<(), AgentError> {
    let mut terminal = init_terminal().map_err(io_err)?;
    let result = event_loop(&mut terminal, repl).await;
    let _ = restore_terminal(&mut terminal);
    result
}

fn init_terminal() -> io::Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore_terminal(terminal: &mut Term) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), DisableMouseCapture, LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

enum Status {
    Idle,
    Streaming,
    RunningTools,
    Info(String),
    Error(String),
}

struct AppState {
    input: String,
    status: Status,
    model: String,
    connected: bool,
    /// Rows scrolled up from the bottom of the message area. 0 = follow bottom.
    scroll_offset: u16,
    input_tokens: u64,
    output_tokens: u64,
    /// In-flight output_tokens for the current turn (reset on MessageStop).
    last_output: u32,
}

impl AppState {
    fn new(model: String) -> Self {
        Self {
            input: String::new(),
            status: Status::Idle,
            model,
            connected: true,
            scroll_offset: 0,
            input_tokens: 0,
            output_tokens: 0,
            last_output: 0,
        }
    }
}

enum KeyAction {
    None,
    Quit,
    Submit(String),
    ScrollBy(i32),
    ScrollToBottom,
}

const SCROLL_STEP: i32 = 3;
const PAGE_STEP: i32 = 10;

enum SlashAction {
    Clear,
    Exit,
    Unknown(String),
}

// Per-block streaming buffer, keyed by the block index.
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
            | AnthropicStreamEvent::Usage { .. }
            | AnthropicStreamEvent::MessageStop => {}
        }
    }

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

// ---------------------------------------------------------------------------
// Event loop
// ---------------------------------------------------------------------------

async fn event_loop(terminal: &mut Term, repl: Repl<'_>) -> Result<(), AgentError> {
    let mut app = AppState::new(repl.config().model.clone());
    let mut events = EventStream::new();
    let mut conversation: Vec<ChatMessage> = Vec::new();
    let mut streaming = StreamingState::new();
    let mut error_banner: Option<String> = None;

    loop {
        terminal
            .draw(|f| draw(f, &conversation, &streaming, &mut app, error_banner.as_deref()))
            .map_err(io_err)?;

        let Some(event) = events.next().await else {
            return Ok(());
        };
        let event =
            event.map_err(|e| AgentError::Other(format!("terminal event error: {e}")))?;

        let action = match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => handle_key(key, &mut app),
            Event::Mouse(m) => handle_mouse(m).unwrap_or(KeyAction::None),
            _ => continue,
        };

        match action {
            KeyAction::None => {}
            KeyAction::Quit => return Ok(()),
            KeyAction::ScrollBy(delta) => apply_scroll(&mut app, delta),
            KeyAction::ScrollToBottom => app.scroll_offset = 0,
            KeyAction::Submit(input) => {
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    continue;
                }
                error_banner = None;

                if let Some(action) = dispatch_slash(trimmed) {
                    match action {
                        SlashAction::Clear => {
                            conversation.clear();
                            app.status = Status::Info("conversation cleared".into());
                        }
                        SlashAction::Exit => return Ok(()),
                        SlashAction::Unknown(name) => {
                            app.status = Status::Error(format!("unknown command: /{name}"));
                        }
                    }
                    continue;
                }

                app.scroll_offset = 0;
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
                    error_banner = Some(e.to_string());
                    app.status = Status::Error(e.to_string());
                }
            }
        }
    }
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
            .draw(|f| draw(f, conversation, streaming, app, None))
            .map_err(io_err)?;

        let stop_reason =
            stream_one_turn(terminal, repl, conversation, streaming, app, events).await?;

        let assistant_content = streaming.finalize();
        streaming.stop();
        conversation.push(ChatMessage::assistant(assistant_content.clone()));

        if stop_reason != StopReason::ToolUse {
            app.status = Status::Idle;
            return Ok(());
        }

        app.status = Status::RunningTools;
        terminal
            .draw(|f| draw(f, conversation, streaming, app, None))
            .map_err(io_err)?;

        let tool_results = repl.run_tool_calls(&assistant_content);
        if tool_results.is_empty() {
            app.status = Status::Idle;
            return Ok(());
        }
        conversation.push(ChatMessage::user(tool_results));
    }

    Err(AgentError::Other(format!("agent exceeded max turns = {MAX_TURNS}")))
}

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
                match event {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
                        {
                            return Err(AgentError::Other("cancelled".into()));
                        }
                        match key.code {
                            KeyCode::PageUp => apply_scroll(app, PAGE_STEP),
                            KeyCode::PageDown => apply_scroll(app, -PAGE_STEP),
                            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                                apply_scroll(app, SCROLL_STEP);
                            }
                            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                                apply_scroll(app, -SCROLL_STEP);
                            }
                            KeyCode::Home => apply_scroll(app, i32::MAX),
                            KeyCode::End => app.scroll_offset = 0,
                            _ => {}
                        }
                        terminal
                            .draw(|f| draw(f, conversation, streaming, app, None))
                            .map_err(io_err)?;
                    }
                    Event::Mouse(m) => {
                        if let Some(KeyAction::ScrollBy(d)) = handle_mouse(m) {
                            apply_scroll(app, d);
                            terminal
                                .draw(|f| draw(f, conversation, streaming, app, None))
                                .map_err(io_err)?;
                        }
                    }
                    _ => {}
                }
            }
            next = stream.next() => {
                match next {
                    Some(Ok(event)) => {
                        let done = matches!(event, AnthropicStreamEvent::MessageStop);
                        match &event {
                            AnthropicStreamEvent::StopReason(reason) => {
                                stop_reason = *reason;
                            }
                            AnthropicStreamEvent::Usage { input_tokens, output_tokens } => {
                                // message_start carries input_tokens once (+a small
                                // output priming count); message_delta updates the
                                // final output_tokens. Take the max so we never
                                // shrink a counter mid-stream.
                                if *input_tokens > 0 {
                                    app.input_tokens += *input_tokens as u64;
                                }
                                app.last_output = (*output_tokens).max(app.last_output);
                            }
                            _ => {}
                        }
                        streaming.apply(&event);
                        terminal
                            .draw(|f| draw(f, conversation, streaming, app, None))
                            .map_err(io_err)?;
                        if done {
                            app.output_tokens += app.last_output as u64;
                            app.last_output = 0;
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

// ---------------------------------------------------------------------------
// Keymap
// ---------------------------------------------------------------------------

fn handle_key(key: KeyEvent, app: &mut AppState) -> KeyAction {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('d') => return KeyAction::Quit,
            KeyCode::Char('u') => {
                app.input.clear();
                return KeyAction::None;
            }
            KeyCode::Char('b') => return KeyAction::ScrollBy(PAGE_STEP),
            KeyCode::Char('f') => return KeyAction::ScrollBy(-PAGE_STEP),
            _ => {}
        }
    }
    match key.code {
        KeyCode::Esc => KeyAction::Quit,
        KeyCode::PageUp => KeyAction::ScrollBy(PAGE_STEP),
        KeyCode::PageDown => KeyAction::ScrollBy(-PAGE_STEP),
        KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
            KeyAction::ScrollBy(SCROLL_STEP)
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
            KeyAction::ScrollBy(-SCROLL_STEP)
        }
        KeyCode::Home => KeyAction::ScrollBy(i32::MAX),
        KeyCode::End => KeyAction::ScrollToBottom,
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

fn handle_mouse(ev: MouseEvent) -> Option<KeyAction> {
    match ev.kind {
        MouseEventKind::ScrollUp => Some(KeyAction::ScrollBy(SCROLL_STEP)),
        MouseEventKind::ScrollDown => Some(KeyAction::ScrollBy(-SCROLL_STEP)),
        _ => None,
    }
}

fn apply_scroll(app: &mut AppState, delta: i32) {
    if delta >= 0 {
        app.scroll_offset = app.scroll_offset.saturating_add(delta.min(u16::MAX as i32) as u16);
    } else {
        let d = (-delta).min(u16::MAX as i32) as u16;
        app.scroll_offset = app.scroll_offset.saturating_sub(d);
    }
}

// ---------------------------------------------------------------------------
// Top-level rendering
// ---------------------------------------------------------------------------

fn draw(
    f: &mut Frame,
    conversation: &[ChatMessage],
    streaming: &StreamingState,
    app: &mut AppState,
    error_banner: Option<&str>,
) {
    Paragraph::new("")
        .style(Style::default().bg(theme::BG))
        .render(f.area(), f.buffer_mut());

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3), Constraint::Length(1)])
        .split(f.area());

    render_messages(f, chunks[0], conversation, streaming, app, error_banner);
    render_input(f, chunks[1], app);
    render_status(f, chunks[2], app, conversation);
}

// ---------------------------------------------------------------------------
// Message-area rendering
// ---------------------------------------------------------------------------

fn render_messages(
    f: &mut Frame,
    area: Rect,
    conversation: &[ChatMessage],
    streaming: &StreamingState,
    app: &mut AppState,
    error_banner: Option<&str>,
) {
    let boxes = build_boxes(conversation, streaming, error_banner);

    if boxes.is_empty() {
        app.scroll_offset = 0;
        render_welcome(f, area);
        return;
    }

    let gap: u16 = 1;
    let heights: Vec<u16> =
        boxes.iter().map(|b| b.outer_height(area.width)).collect();
    let boxes_total: u16 = heights.iter().copied().sum::<u16>()
        + gap * heights.len().saturating_sub(1) as u16;

    // The ASCII banner is always kept at the top of the scroll stack so
    // scrolling up past the oldest message reveals it.
    let banner_lines = build_banner_lines(area.width);
    let banner_height = banner_lines.len() as u16;
    let banner_top_pad: u16 = if banner_height > 0 { 2 } else { 0 };
    let banner_gap: u16 = if banner_height > 0 { 1 } else { 0 };
    let total = banner_top_pad
        .saturating_add(banner_height)
        .saturating_add(banner_gap)
        .saturating_add(boxes_total);

    let render_stack = |buf: &mut Buffer, origin_x: u16, origin_y: u16| {
        let mut y = origin_y;
        if banner_height > 0 {
            y = y.saturating_add(banner_top_pad);
            let rect = Rect { x: origin_x, y, width: area.width, height: banner_height };
            Paragraph::new(banner_lines.clone())
                .alignment(Alignment::Center)
                .style(Style::default().bg(theme::BG))
                .render(rect, buf);
            y = y.saturating_add(banner_height + banner_gap);
        }
        for (i, b) in boxes.iter().enumerate() {
            let rect = Rect { x: origin_x, y, width: area.width, height: heights[i] };
            b.render(buf, rect);
            y = y.saturating_add(heights[i] + gap);
        }
    };

    if total <= area.height {
        app.scroll_offset = 0;
        render_stack(f.buffer_mut(), area.x, area.y);
        return;
    }

    let max_offset = total - area.height;
    if app.scroll_offset > max_offset {
        app.scroll_offset = max_offset;
    }

    let off_area = Rect::new(0, 0, area.width, total);
    let mut off = Buffer::empty(off_area);
    // Pre-fill with the app background so gap rows between boxes keep BG
    // when blitted (otherwise the terminal's own background shows through).
    Paragraph::new("")
        .style(Style::default().bg(theme::BG))
        .render(off_area, &mut off);

    render_stack(&mut off, 0, 0);

    let src_start_y = max_offset - app.scroll_offset;
    let frame_buf = f.buffer_mut();
    for row in 0..area.height {
        for col in 0..area.width {
            let src_pos = (col, src_start_y + row);
            let dst_pos = (area.x + col, area.y + row);
            if let (Some(src), Some(dst)) =
                (off.cell(src_pos), frame_buf.cell_mut(dst_pos))
            {
                *dst = src.clone();
            }
        }
    }

    if app.scroll_offset > 0 {
        let hint = format!(" ↑ scrolled {} rows · End to jump to bottom ", app.scroll_offset);
        let hint_w = hint.chars().count() as u16;
        if hint_w + 2 <= area.width {
            let x = area.x + area.width - hint_w - 1;
            let y = area.y;
            let rect = Rect { x, y, width: hint_w, height: 1 };
            Paragraph::new(Line::from(Span::styled(
                hint,
                Style::default().fg(theme::AMBER).bg(theme::BG),
            )))
            .render(rect, f.buffer_mut());
        }
    }
}

fn build_banner_lines(width: u16) -> Vec<Line<'static>> {
    let style = Style::default()
        .fg(theme::PURPLE)
        .bg(theme::BG)
        .add_modifier(Modifier::BOLD);
    if width >= BANNER_WIDTH {
        BANNER
            .lines()
            .map(|l| Line::from(Span::styled(l.to_string(), style)))
            .collect()
    } else if width >= "AGENTY".len() as u16 {
        vec![Line::from(Span::styled("AGENTY", style))]
    } else {
        Vec::new()
    }
}

fn render_welcome(f: &mut Frame, area: Rect) {
    let lines: Vec<Line<'static>> = BANNER
        .lines()
        .map(|l| {
            Line::from(Span::styled(
                l.to_string(),
                Style::default()
                    .fg(theme::PURPLE)
                    .bg(theme::BG)
                    .add_modifier(Modifier::BOLD),
            ))
        })
        .collect();
    let banner_height = lines.len() as u16;

    if area.width < BANNER_WIDTH || area.height < banner_height + 3 {
        let fallback = Line::from(Span::styled(
            "AGENTY",
            Style::default()
                .fg(theme::PURPLE)
                .bg(theme::BG)
                .add_modifier(Modifier::BOLD),
        ));
        let rect = Rect {
            x: area.x,
            y: area.y + area.height / 2,
            width: area.width,
            height: 1,
        };
        Paragraph::new(fallback)
            .alignment(Alignment::Center)
            .render(rect, f.buffer_mut());
        return;
    }

    let banner_y = area.y + (area.height.saturating_sub(banner_height + 2)) / 2;
    let banner_x = area.x + (area.width - BANNER_WIDTH) / 2;
    let banner_rect = Rect {
        x: banner_x,
        y: banner_y,
        width: BANNER_WIDTH,
        height: banner_height,
    };
    Paragraph::new(lines)
        .style(Style::default().bg(theme::BG))
        .render(banner_rect, f.buffer_mut());

    let tag_rect = Rect {
        x: area.x,
        y: banner_y + banner_height + 1,
        width: area.width,
        height: 1,
    };
    Paragraph::new(Line::from(Span::styled(
        "ask me anything  ·  Enter to send  ·  Ctrl+C or /exit to quit",
        Style::default().fg(theme::DIM).bg(theme::BG),
    )))
    .alignment(Alignment::Center)
    .render(tag_rect, f.buffer_mut());
}

// ---------------------------------------------------------------------------
// Message-box model
// ---------------------------------------------------------------------------

struct MessageBox {
    label: String,
    border_color: Color,
    footer: Vec<(String, Color)>,
    lines: Vec<Line<'static>>,
}

impl MessageBox {
    fn outer_height(&self, outer_width: u16) -> u16 {
        let inner_width = outer_width.saturating_sub(2);
        if inner_width == 0 {
            return 2;
        }
        let count = Paragraph::new(self.lines.clone())
            .wrap(Wrap { trim: false })
            .line_count(inner_width) as u16;
        count.max(1) + 2
    }

    fn render(&self, buf: &mut Buffer, area: Rect) {
        let title_style = Style::default()
            .fg(self.border_color)
            .bg(theme::BG)
            .add_modifier(Modifier::BOLD);

        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.border_color).bg(theme::BG))
            .title(Line::from(Span::styled(
                format!(" {} ", self.label),
                title_style,
            )));

        if !self.footer.is_empty() {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (text, col) in &self.footer {
                spans.push(Span::styled(
                    " ".to_string(),
                    Style::default().bg(theme::BG),
                ));
                spans.push(Span::styled(
                    format!("[{}]", text),
                    Style::default().fg(*col).bg(theme::BG),
                ));
            }
            spans.push(Span::styled(
                " ".to_string(),
                Style::default().bg(theme::BG),
            ));
            block = block
                .title_bottom(Line::from(spans).alignment(Alignment::Right));
        }

        Paragraph::new(self.lines.clone())
            .block(block)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(theme::FG).bg(theme::BG))
            .render(area, buf);
    }
}

fn build_boxes(
    conversation: &[ChatMessage],
    streaming: &StreamingState,
    error_banner: Option<&str>,
) -> Vec<MessageBox> {
    let mut boxes = Vec::new();

    for msg in conversation {
        match msg.role {
            Role::User => flush_user(msg, &mut boxes),
            Role::Assistant => flush_assistant(msg, &mut boxes),
            Role::Tool => {}
        }
    }

    if streaming.active {
        for block in streaming.blocks.values() {
            match block {
                StreamingBlock::Text(text) if !text.is_empty() => {
                    boxes.push(assistant_box(text, &[], true));
                }
                StreamingBlock::Thinking(text) if !text.is_empty() => {
                    boxes.push(thinking_box(text));
                }
                StreamingBlock::ToolUse { name, partial_json, .. } => {
                    let input = if partial_json.is_empty() {
                        serde_json::Value::Object(Default::default())
                    } else {
                        serde_json::from_str(partial_json)
                            .unwrap_or_else(|_| {
                                serde_json::Value::String(partial_json.clone())
                            })
                    };
                    boxes.push(tool_call_box(name, &input, true));
                }
                _ => {}
            }
        }
    }

    if let Some(err) = error_banner {
        boxes.push(error_box(err));
    }

    boxes
}

fn flush_user(msg: &ChatMessage, boxes: &mut Vec<MessageBox>) {
    let mut text_buf = String::new();
    for block in &msg.content {
        match block {
            ContentBlock::Text { text } => {
                if !text_buf.is_empty() {
                    text_buf.push('\n');
                }
                text_buf.push_str(text);
            }
            ContentBlock::ToolResult { content, is_error, .. } => {
                if !text_buf.is_empty() {
                    boxes.push(user_box(&text_buf));
                    text_buf.clear();
                }
                boxes.push(tool_result_box(content, *is_error));
            }
            ContentBlock::ToolUse { .. } => {}
        }
    }
    if !text_buf.is_empty() {
        boxes.push(user_box(&text_buf));
    }
}

fn flush_assistant(msg: &ChatMessage, boxes: &mut Vec<MessageBox>) {
    let tool_names: Vec<String> = msg
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();

    let mut text_buf = String::new();
    for block in &msg.content {
        match block {
            ContentBlock::Text { text } => {
                if !text_buf.is_empty() {
                    text_buf.push('\n');
                }
                text_buf.push_str(text);
            }
            ContentBlock::ToolUse { name, input, .. } => {
                if !text_buf.is_empty() {
                    boxes.push(assistant_box(&text_buf, &tool_names, false));
                    text_buf.clear();
                }
                boxes.push(tool_call_box(name, input, false));
            }
            ContentBlock::ToolResult { .. } => {}
        }
    }
    if !text_buf.is_empty() {
        boxes.push(assistant_box(&text_buf, &tool_names, false));
    }
}

fn user_box(text: &str) -> MessageBox {
    MessageBox {
        label: "[USER]".into(),
        border_color: theme::BLUE,
        footer: Vec::new(),
        lines: parse_content(text, Style::default().fg(theme::FG_BRIGHT).bg(theme::BG)),
    }
}

fn assistant_box(text: &str, tool_names: &[String], streaming: bool) -> MessageBox {
    let mut footer: Vec<(String, Color)> = tool_names
        .iter()
        .map(|n| (n.clone(), theme::PURPLE_LIGHT))
        .collect();
    if streaming {
        footer.push(("…".into(), theme::PURPLE_LIGHT));
    }
    MessageBox {
        label: "[ASSISTANT] (Agent)".into(),
        border_color: theme::PURPLE,
        footer,
        lines: parse_content(text, Style::default().fg(theme::FG).bg(theme::BG)),
    }
}

fn tool_call_box(name: &str, input: &serde_json::Value, streaming: bool) -> MessageBox {
    let input_text = match input {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    let body = if input_text.is_empty() {
        name.to_string()
    } else {
        format!("{name}(\n{input_text}\n)")
    };
    let mut footer = vec![(name.to_string(), theme::AMBER)];
    if streaming {
        footer.insert(0, ("streaming".into(), theme::AMBER));
    } else {
        footer.insert(0, ("available".into(), theme::AMBER));
    }
    MessageBox {
        label: "[TOOL CALL]".into(),
        border_color: theme::AMBER,
        footer,
        lines: parse_content(&body, Style::default().fg(theme::FG).bg(theme::BG)),
    }
}

fn tool_result_box(content: &str, is_error: bool) -> MessageBox {
    let (tag, tag_col) = if is_error {
        ("error", theme::RED_LIGHT)
    } else {
        ("success", theme::GREEN_LIGHT)
    };
    let border_color = if is_error { theme::RED } else { theme::GREEN };
    let body_style = if is_error {
        Style::default().fg(theme::RED_LIGHT).bg(theme::BG)
    } else {
        Style::default().fg(theme::FG).bg(theme::BG)
    };
    MessageBox {
        label: "[TOOL RESULT]".into(),
        border_color,
        footer: vec![(tag.into(), tag_col)],
        lines: parse_content(content, body_style),
    }
}

fn thinking_box(text: &str) -> MessageBox {
    MessageBox {
        label: "[SYSTEM] thinking".into(),
        border_color: theme::GRAY,
        footer: Vec::new(),
        lines: parse_content(
            text,
            Style::default()
                .fg(theme::DIM)
                .bg(theme::BG)
                .add_modifier(Modifier::ITALIC),
        ),
    }
}

fn error_box(msg: &str) -> MessageBox {
    MessageBox {
        label: "[ERROR]".into(),
        border_color: theme::RED,
        footer: Vec::new(),
        lines: parse_content(msg, Style::default().fg(theme::RED_LIGHT).bg(theme::BG)),
    }
}

// ---------------------------------------------------------------------------
// Content parsing: inline backticks, fenced code, ✓/✕ line prefixes
// ---------------------------------------------------------------------------

fn parse_content(text: &str, base: Style) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut in_fenced = false;

    for raw in text.lines() {
        let trimmed_start = raw.trim_start();
        if trimmed_start.starts_with("```") {
            in_fenced = !in_fenced;
            lines.push(Line::from(Span::styled(
                "─".repeat(12),
                Style::default().fg(theme::BORDER_DIM).bg(theme::BG),
            )));
            continue;
        }
        if in_fenced {
            lines.push(highlight_code_line(raw));
            continue;
        }
        if let Some((level, rest)) = parse_heading(trimmed_start) {
            lines.push(render_heading(level, rest));
            continue;
        }
        lines.push(parse_inline(raw, base));
    }

    if lines.is_empty() {
        lines.push(Line::raw(""));
    }
    lines
}

/// Detect an ATX heading (`#` through `######` followed by at least one space).
/// Returns (level, rest-of-line) where level is 1..=6.
fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let mut level = 0;
    let bytes = line.as_bytes();
    while level < bytes.len() && bytes[level] == b'#' {
        level += 1;
    }
    if level == 0 || level > 6 {
        return None;
    }
    if level >= bytes.len() || bytes[level] != b' ' {
        return None;
    }
    Some((level, line[level + 1..].trim_start()))
}

fn render_heading(level: usize, rest: &str) -> Line<'static> {
    let (fg, modifier) = match level {
        1 => (theme::PURPLE, Modifier::BOLD | Modifier::UNDERLINED),
        2 => (theme::PURPLE, Modifier::BOLD),
        3 => (theme::PURPLE_LIGHT, Modifier::BOLD),
        _ => (theme::FG_BRIGHT, Modifier::BOLD),
    };
    let base = Style::default().fg(fg).bg(theme::BG).add_modifier(modifier);
    let mut spans = emit_inline_spans(rest, base);
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    Line::from(spans)
}

fn parse_inline(line: &str, base: Style) -> Line<'static> {
    let trimmed = line.trim_start();
    let effective = if trimmed.starts_with('✓') {
        Style::default().fg(theme::GREEN_LIGHT).bg(theme::BG)
    } else if trimmed.starts_with('✕') {
        Style::default().fg(theme::RED_LIGHT).bg(theme::BG)
    } else {
        base
    };

    let mut spans = emit_inline_spans(line, effective);
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), effective));
    }
    Line::from(spans)
}

/// Convert markdown inline text into styled spans. Handles inline `code`,
/// `**bold**`, `*italic*`, `***bold-italic***`, and the `_`/`__`/`___` variants.
/// Nesting inside a single emphasis span is not supported; the outer span wins.
fn emit_inline_spans(line: &str, base: Style) -> Vec<Span<'static>> {
    let code_style = Style::default()
        .fg(theme::PURPLE_LIGHT)
        .bg(theme::INLINE_CODE_BG);

    let chars: Vec<char> = line.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    let flush = |buf: &mut String, spans: &mut Vec<Span<'static>>, style: Style| {
        if !buf.is_empty() {
            spans.push(Span::styled(std::mem::take(buf), style));
        }
    };

    while i < chars.len() {
        let c = chars[i];

        if c == '`' {
            if let Some(close) = find_unescaped(&chars, i + 1, '`') {
                flush(&mut buf, &mut spans, base);
                let inner: String = chars[i + 1..close].iter().collect();
                spans.push(Span::styled(inner, code_style));
                i = close + 1;
                continue;
            }
        }

        if (c == '*' || c == '_') && can_open_emphasis(&chars, i) {
            let run = count_marker(&chars, i, c).min(3);
            if let Some(close) = find_closing_marker(&chars, i + run, c, run) {
                flush(&mut buf, &mut spans, base);
                let inner: String = chars[i + run..close].iter().collect();
                let style = match run {
                    1 => base.add_modifier(Modifier::ITALIC),
                    2 => base.add_modifier(Modifier::BOLD),
                    _ => base.add_modifier(Modifier::BOLD | Modifier::ITALIC),
                };
                // Recurse so `**foo `bar`**` still highlights inline code.
                for span in emit_inline_spans(&inner, style) {
                    spans.push(span);
                }
                i = close + run;
                continue;
            }
        }

        buf.push(c);
        i += 1;
    }
    flush(&mut buf, &mut spans, base);
    spans
}

fn find_unescaped(chars: &[char], from: usize, target: char) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            i += 2;
            continue;
        }
        if chars[i] == target {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn count_marker(chars: &[char], from: usize, marker: char) -> usize {
    let mut n = 0;
    while from + n < chars.len() && chars[from + n] == marker {
        n += 1;
    }
    n
}

/// An opener must be preceded by start-of-line, whitespace, or punctuation —
/// and followed by a non-whitespace character (otherwise `*` is just an asterisk).
fn can_open_emphasis(chars: &[char], i: usize) -> bool {
    let prev_ok = i == 0
        || {
            let p = chars[i - 1];
            p.is_whitespace() || "([{\"'`~—-".contains(p)
        };
    let marker = chars[i];
    let run = count_marker(chars, i, marker);
    let next = chars.get(i + run).copied();
    let next_ok = match next {
        Some(c) => !c.is_whitespace(),
        None => false,
    };
    prev_ok && next_ok
}

/// Look for a closing marker run of exactly `run` marker chars. The closer
/// must be preceded by a non-whitespace character.
fn find_closing_marker(
    chars: &[char],
    from: usize,
    marker: char,
    run: usize,
) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            i += 2;
            continue;
        }
        if chars[i] == marker {
            let here = count_marker(chars, i, marker);
            // Closer must have at least `run` markers and a non-space predecessor.
            let prev_ok = i > 0 && !chars[i - 1].is_whitespace();
            if here >= run && prev_ok {
                return Some(i);
            }
            i += here.max(1);
            continue;
        }
        i += 1;
    }
    None
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn",
    "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let",
    "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self",
    "Self", "static", "struct", "super", "trait", "true", "type", "unsafe",
    "use", "where", "while",
];

fn highlight_code_line(line: &str) -> Line<'static> {
    let base = Style::default().fg(theme::FG).bg(theme::SURFACE);
    let kw = Style::default().fg(Color::Rgb(0xc0, 0x84, 0xfc)).bg(theme::SURFACE);
    let string = Style::default().fg(theme::GREEN_LIGHT).bg(theme::SURFACE);
    let number = Style::default().fg(Color::Rgb(0xfb, 0x92, 0x3c)).bg(theme::SURFACE);
    let comment = Style::default()
        .fg(theme::DIM)
        .bg(theme::SURFACE)
        .add_modifier(Modifier::ITALIC);

    let mut spans: Vec<Span<'static>> = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            let rest: String = chars[i..].iter().collect();
            spans.push(Span::styled(rest, comment));
            break;
        }
        if c == '"' {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if i < chars.len() {
                i += 1;
            }
            let s: String = chars[start..i].iter().collect();
            spans.push(Span::styled(s, string));
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_')
            {
                i += 1;
            }
            let s: String = chars[start..i].iter().collect();
            spans.push(Span::styled(s, number));
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let style = if RUST_KEYWORDS.contains(&word.as_str()) { kw } else { base };
            spans.push(Span::styled(word, style));
            continue;
        }
        let start = i;
        while i < chars.len() {
            let d = chars[i];
            if d.is_alphanumeric()
                || d == '_'
                || d == '"'
                || (d == '/' && i + 1 < chars.len() && chars[i + 1] == '/')
            {
                break;
            }
            i += 1;
        }
        if i == start {
            i += 1;
        }
        let s: String = chars[start..i].iter().collect();
        spans.push(Span::styled(s, base));
    }

    Line::from(spans).style(Style::default().bg(theme::SURFACE))
}

// ---------------------------------------------------------------------------
// Input bar
// ---------------------------------------------------------------------------

fn render_input(f: &mut Frame, area: Rect, app: &AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER_DIM).bg(theme::BG))
        .style(Style::default().bg(theme::BG));
    let inner = block.inner(area);
    block.render(area, f.buffer_mut());

    let prompt = Span::styled(
        "❯ ",
        Style::default()
            .fg(theme::BLUE)
            .bg(theme::BG)
            .add_modifier(Modifier::BOLD),
    );
    let text = Span::styled(
        app.input.clone(),
        Style::default().fg(theme::FG_BRIGHT).bg(theme::BG),
    );
    let cursor = Span::styled(
        "█",
        Style::default().fg(theme::FG_BRIGHT).bg(theme::BG),
    );
    let line = Line::from(vec![prompt, text, cursor]);
    Paragraph::new(line)
        .style(Style::default().bg(theme::BG))
        .render(inner, f.buffer_mut());
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

fn render_status(f: &mut Frame, area: Rect, app: &AppState, conversation: &[ChatMessage]) {
    let model_chip = format!(" {} ", app.model);
    let conn = if app.connected { " ● connected " } else { " ● offline   " };
    let conn_style = if app.connected {
        Style::default().fg(theme::GREEN).bg(theme::STATUS_BG)
    } else {
        Style::default().fg(theme::RED_LIGHT).bg(theme::STATUS_BG)
    };

    let status_text = match &app.status {
        Status::Idle => String::new(),
        Status::Streaming => " streaming… ".into(),
        Status::RunningTools => " running tools… ".into(),
        Status::Info(t) => format!(" ⓘ {t} "),
        Status::Error(t) => format!(" ✕ {t} "),
    };
    let status_style = match &app.status {
        Status::Idle | Status::Info(_) => Style::default().fg(theme::DIM).bg(theme::STATUS_BG),
        Status::Streaming | Status::RunningTools => {
            Style::default().fg(theme::AMBER).bg(theme::STATUS_BG)
        }
        Status::Error(_) => Style::default().fg(theme::RED_LIGHT).bg(theme::STATUS_BG),
    };

    let total_tokens = app.input_tokens + app.output_tokens + app.last_output as u64;
    let right_text = format!(
        "  messages: {}  tokens: {} (in {} / out {}) ",
        conversation.len(),
        total_tokens,
        app.input_tokens,
        app.output_tokens + app.last_output as u64,
    );

    let left_width = model_chip.chars().count() as u16
        + conn.chars().count() as u16
        + status_text.chars().count() as u16;
    let right_width = right_text.chars().count() as u16;
    let pad = area
        .width
        .saturating_sub(left_width + right_width) as usize;

    let line = Line::from(vec![
        Span::styled(
            model_chip,
            Style::default()
                .fg(Color::White)
                .bg(theme::BLUE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(conn.to_string(), conn_style),
        Span::styled(status_text, status_style),
        Span::styled(
            " ".repeat(pad),
            Style::default().bg(theme::STATUS_BG),
        ),
        Span::styled(
            right_text,
            Style::default().fg(theme::PURPLE_LIGHT).bg(theme::STATUS_BG),
        ),
    ]);

    Paragraph::new(line)
        .style(Style::default().bg(theme::STATUS_BG))
        .render(area, f.buffer_mut());
}

// ---------------------------------------------------------------------------

fn io_err(e: io::Error) -> AgentError {
    AgentError::Other(format!("terminal I/O error: {e}"))
}
