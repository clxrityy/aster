//! Layout: transcript on the left; levels, inbox, and thought trace on the
//! right; input and status at the bottom. The conversation, inbox, and trace
//! wrap their full text and scroll independently; the focused one is
//! highlighted.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
};
use serde_json::Value;

use crate::app::{App, LEVELS, Mode, Pane, PaneView, Who};

const SPARKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Draw wrapped lines in a bordered pane that scrolls on its own, with a
/// scrollbar when the content is taller than the pane.
fn draw_scrolled(
    frame: &mut Frame,
    app: &App,
    pane: Pane,
    area: Rect,
    lines: Vec<Line<'static>>,
    title: &str,
) {
    let focused = app.focus == pane;
    let border = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    let block = Block::bordered().border_style(border);
    let inner = block.inner(area);
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let height = inner.height as usize;
    let max = paragraph.line_count(inner.width).saturating_sub(height);
    let offset = app.offset(pane, max);
    app.record_view(pane, PaneView { area, max, height });

    let mut title = Span::raw(format!(" {title} "));
    if focused {
        title = title.style(Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    }
    let mut titles = vec![title];
    if max > 0 && offset < max {
        titles.push(Span::styled(
            format!(" ↓ {} more ", max - offset),
            Style::new().fg(Color::Yellow),
        ));
    }
    frame.render_widget(
        paragraph
            .scroll((offset.min(u16::MAX as usize) as u16, 0))
            .block(block.title(Line::from(titles))),
        area,
    );
    if max > 0 {
        let mut state = ScrollbarState::new(max).position(offset);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(border),
            area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut state,
        );
    }
}

pub fn draw(frame: &mut Frame, app: &App) {
    let [header, body, input, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, app, header);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).areas(body);
    draw_transcript(frame, app, left);
    draw_side(frame, app, right);
    draw_input(frame, app, input);
    frame.render_widget(
        Paragraph::new(Line::from(app.status.clone()).dark_gray()),
        status,
    );
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let mode = match app.mode {
        Mode::Chat => Span::styled(" chat ", Style::new().fg(Color::Black).bg(Color::Cyan)),
        Mode::Train => Span::styled(" TRAIN ", Style::new().fg(Color::Black).bg(Color::Yellow)),
    };
    let connection = if app.connected {
        Span::styled(" ● live ", Style::new().fg(Color::Green))
    } else {
        Span::styled(" ○ offline ", Style::new().fg(Color::Red))
    };
    let mut spans = vec![
        Span::styled(
            " ✶ aster ",
            Style::new().add_modifier(Modifier::BOLD).fg(Color::Magenta),
        ),
        mode,
        connection,
    ];
    if app.paused {
        spans.push(Span::styled(" ❚❚ paused ", Style::new().fg(Color::Yellow)));
    }
    if app.resting {
        spans.push(Span::styled(" ☾ resting ", Style::new().fg(Color::Blue)));
    }
    if !app.inbox.is_empty() {
        spans.push(Span::styled(
            format!(" ✉ {} waiting ", app.inbox.len()),
            Style::new().fg(Color::LightMagenta),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_transcript(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    for entry in &app.transcript {
        let (label, style) = match entry.who {
            Who::Owner => (
                "you  ",
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Who::Aster => (
                "aster",
                Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD),
            ),
            Who::System => ("  ·  ", Style::new().fg(Color::DarkGray)),
            Who::Error => ("  !  ", Style::new().fg(Color::Red)),
        };
        let text_style = match entry.who {
            Who::System => Style::new().fg(Color::DarkGray),
            Who::Error => Style::new().fg(Color::Red),
            _ => Style::new(),
        };
        for (i, part) in entry.text.split('\n').enumerate() {
            let mut spans = Vec::new();
            if i == 0 {
                if !entry.at.is_empty() {
                    spans.push(Span::styled(
                        format!("{} ", entry.at),
                        Style::new().fg(Color::DarkGray),
                    ));
                }
                spans.push(Span::styled(format!("{label} "), style));
            } else {
                spans.push(Span::raw("      "));
            }
            spans.push(Span::styled(part.to_string(), text_style));
            if i == 0 {
                match entry.rating {
                    Some(r) if r > 0 => {
                        spans.push(Span::styled(" ▲", Style::new().fg(Color::Green)))
                    }
                    Some(_) => spans.push(Span::styled(" ▼", Style::new().fg(Color::Red))),
                    None => {}
                }
            }
            lines.push(Line::from(spans));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from("Say hello, or type /help.".dark_gray()));
    }
    draw_scrolled(frame, app, Pane::Conversation, area, lines, "conversation");
}

fn draw_side(frame: &mut Frame, app: &App, area: Rect) {
    let constraints = if app.show_trace {
        vec![
            Constraint::Length(9),
            Constraint::Fill(1),
            Constraint::Fill(2),
        ]
    } else {
        vec![Constraint::Length(9), Constraint::Fill(1)]
    };
    let areas = Layout::vertical(constraints).split(area);
    draw_levels(frame, app, areas[0]);
    draw_inbox(frame, app, areas[1]);
    if app.show_trace {
        draw_trace(frame, app, areas[2]);
    } else {
        app.record_view(Pane::Trace, PaneView::default());
    }
}

fn draw_levels(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width.saturating_sub(2) as usize;
    let bar_width = width.saturating_sub(14 + 5 + 10).max(4);
    let mut lines = Vec::new();
    for name in LEVELS {
        let value = app
            .levels
            .as_ref()
            .and_then(|l| l[name].as_f64())
            .unwrap_or(0.0);
        let filled = ((value / 100.0) * bar_width as f64).round() as usize;
        let color = match name {
            "stress" | "cognitive_load" if value > 60.0 => Color::Red,
            "energy" if value < 25.0 => Color::Red,
            _ => Color::Cyan,
        };
        let series = app.level_history.get(name);
        let spark: String = series
            .map(|s| {
                s.iter()
                    .rev()
                    .take(8)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .map(|v| SPARKS[((v / 100.0) * 7.0).round().clamp(0.0, 7.0) as usize])
                    .collect()
            })
            .unwrap_or_default();
        let trend = series
            .and_then(|s| {
                let n = s.len();
                (n >= 2).then(|| s[n - 1] - s[n - 2])
            })
            .map(|d| {
                if d > 0.05 {
                    "↑"
                } else if d < -0.05 {
                    "↓"
                } else {
                    " "
                }
            })
            .unwrap_or(" ");
        lines.push(Line::from(vec![
            Span::raw(format!("{:<14}", name.replace('_', " "))),
            Span::styled("█".repeat(filled), Style::new().fg(color)),
            Span::styled(
                "░".repeat(bar_width - filled.min(bar_width)),
                Style::new().fg(Color::DarkGray),
            ),
            Span::raw(format!(" {value:>3.0}{trend} ")),
            Span::styled(spark, Style::new().fg(Color::DarkGray)),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" levels ")),
        area,
    );
}

fn draw_inbox(frame: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = if app.inbox.is_empty() {
        vec![Line::from("nothing waiting".dark_gray())]
    } else {
        app.inbox
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let request = item["needs_approval"] == true;
                let marker = if request {
                    Span::styled("⚑ ", Style::new().fg(Color::Yellow))
                } else {
                    Span::styled("? ", Style::new().fg(Color::LightMagenta))
                };
                let mut spans = vec![
                    Span::raw(format!("{}. ", i + 1)),
                    marker,
                    Span::raw(
                        item["draft"]["body"]
                            .as_str()
                            .unwrap_or("")
                            .replace('\n', " "),
                    ),
                ];
                if let Some(url) = item["draft"]["source"].as_str() {
                    spans.push(Span::styled(
                        format!(" → {url}"),
                        Style::new().fg(Color::DarkGray),
                    ));
                }
                Line::from(spans)
            })
            .collect()
    };
    draw_scrolled(
        frame,
        app,
        Pane::Inbox,
        area,
        lines,
        "inbox · /answer n · /approve n · /deny n",
    );
}

fn num(value: &Value) -> f64 {
    value.as_f64().unwrap_or(0.0)
}

fn draw_trace(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    match &app.trace {
        None => lines.push(Line::from("no decision yet".dark_gray())),
        Some(trace) => {
            let decision = trace["decision"].as_str().unwrap_or("?");
            let color = if decision == "reply" {
                Color::Green
            } else {
                Color::Yellow
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{decision} "),
                    Style::new().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(
                    "conf {:.2} / thr {:.2} · explore {:.2}{}",
                    num(&trace["confidence"]),
                    num(&trace["threshold"]),
                    num(&trace["exploration"]),
                    if trace["explored"] == true {
                        " (explored)"
                    } else {
                        ""
                    }
                )),
            ]));
            let unknown: Vec<&str> = trace["unknown_terms"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if !unknown.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("unknown ", Style::new().fg(Color::LightMagenta)),
                    Span::raw(unknown.join(", ")),
                ]));
            }
            if let Some(recalled) = trace["recalled"].as_array().filter(|r| !r.is_empty()) {
                lines.push(Line::from("recalled".dark_gray()));
                for (i, item) in recalled.iter().enumerate() {
                    lines.push(Line::from(format!(
                        " {:>2} {:>5.2} {}",
                        i + 1,
                        num(&item["score"]),
                        item["excerpt"].as_str().unwrap_or("").replace('\n', " ")
                    )));
                }
            }
            let chosen = trace["chosen"].as_u64();
            if let Some(candidates) = trace["candidates"].as_array() {
                lines.push(Line::from("candidates".dark_gray()));
                for (i, candidate) in candidates.iter().enumerate() {
                    let mark = if chosen == Some(i as u64) { "▶" } else { " " };
                    let source = candidate["source"].as_str().unwrap_or("");
                    lines.push(Line::from(vec![
                        Span::styled(mark, Style::new().fg(Color::Green)),
                        Span::raw(format!("{:>2} {:>5.2} ", i + 1, num(&candidate["score"]))),
                        Span::styled(format!("{:<11} ", source), Style::new().fg(Color::DarkGray)),
                        Span::raw(candidate["text"].as_str().unwrap_or("").replace('\n', " ")),
                    ]));
                }
            }
            for note in trace["notes"].as_array().into_iter().flatten() {
                lines.push(Line::from(
                    note.as_str().unwrap_or("").to_string().dark_gray(),
                ));
            }
        }
    }
    draw_scrolled(
        frame,
        app,
        Pane::Trace,
        area,
        lines,
        "thought trace · Ctrl-T hides",
    );
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let prompt = match app.mode {
        Mode::Chat => "› ",
        Mode::Train => "✎ ",
    };
    let inner_width = area.width.saturating_sub(4) as usize;
    // Keep the cursor visible on long input by scrolling horizontally.
    let skip = app.cursor.saturating_sub(inner_width.saturating_sub(1));
    let visible: String = app.input.chars().skip(skip).take(inner_width).collect();
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::raw(prompt), Span::raw(visible)]))
            .block(Block::bordered()),
        area,
    );
    frame.set_cursor_position((area.x + 1 + 2 + (app.cursor - skip) as u16, area.y + 1));
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};
    use serde_json::json;

    use crate::{
        Options,
        app::{App, Msg},
        client::Client,
    };

    fn app() -> App {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            Client::new("http://127.0.0.1:1", "t".into()),
            Options {
                server_url: "http://127.0.0.1:1".into(),
                token: "t".into(),
                show_trace: true,
                training_prompt_after_reply: true,
                history_turns: 10,
            },
            tx,
        )
    }

    fn screen(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| super::draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        buffer
            .content
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect()
    }

    #[test]
    fn renders_conversation_levels_inbox_and_trace() {
        let mut app = app();
        app.handle(Msg::Connected(true));
        app.handle(Msg::Event(json!({
            "type": "turn", "at": "2026-11-19T10:00:00Z",
            "turn": { "id": "a", "role": "owner", "text": "hello there", "created_at": "2026-11-19T10:00:00Z" }
        })));
        app.handle(Msg::Event(json!({
            "type": "turn", "at": "2026-11-19T10:00:01Z",
            "turn": { "id": "b", "role": "aster", "text": "Hi, I'm Aster.", "created_at": "2026-11-19T10:00:01Z" }
        })));
        // Duplicates (stream + POST result) are shown once.
        app.handle(Msg::Event(json!({
            "type": "turn", "turn": { "id": "b", "role": "aster", "text": "Hi, I'm Aster." }
        })));
        app.handle(Msg::Event(json!({
            "type": "levels", "cause": "x",
            "state": { "energy": 75, "focus": 70, "cognitive_load": 10, "curiosity": 60,
                       "stimulation": 40, "stress": 10, "confidence": 50 }
        })));
        app.handle(Msg::Event(json!({
            "type": "outbound", "at": "2026-11-19T10:00:02Z",
            "item": { "id": "q1", "status": "pending", "needs_approval": false, "origin_turn": null,
                      "draft": { "body": "What does nebula mean?" } }
        })));
        app.handle(Msg::Event(json!({
            "type": "thought", "turn_id": "b",
            "trace": { "decision": "reply", "confidence": 0.66, "threshold": 0.4, "exploration": 0.1,
                       "unknown_terms": ["nebula"], "recalled": [], "notes": [],
                       "candidates": [{ "text": "Hi, I'm Aster.", "source": "association", "score": 0.66 }],
                       "chosen": 0 }
        })));
        let text = screen(&app, 140, 40);
        for expected in [
            "hello there",
            "Hi, I'm Aster.",
            "energy",
            "confidence",
            "What does nebula mean?",
            "reply",
            "association",
            "live",
        ] {
            assert!(text.contains(expected), "missing {expected}");
        }
        // Once in the transcript (deduplicated) and once as the trace candidate.
        assert_eq!(text.matches("Hi, I'm Aster.").count(), 2);
        // Small terminals must not panic.
        screen(&app, 30, 10);
    }

    fn key(app: &mut App, code: crossterm::event::KeyCode, mods: crossterm::event::KeyModifiers) {
        app.handle(Msg::Input(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(code, mods),
        )));
    }

    #[test]
    fn long_text_wraps_and_every_pane_scrolls() {
        use crate::app::Pane;
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut app = app();
        let long = "I don't know what “photosynthesis” means yet and this question is long \
                    enough that it never fits on one line of the inbox ENDOFQUESTION";
        app.handle(Msg::Event(json!({
            "type": "outbound", "at": "2026-11-19T10:00:02Z",
            "item": { "id": "q1", "status": "pending", "needs_approval": false,
                      "origin_turn": null, "draft": { "body": long } }
        })));
        let notes: Vec<String> = (0..40).map(|i| format!("note number {i}")).collect();
        app.handle(Msg::Event(json!({
            "type": "thought", "turn_id": "b",
            "trace": { "decision": "ask", "confidence": 0.1, "threshold": 0.4,
                       "exploration": 0.1, "unknown_terms": [], "recalled": [],
                       "notes": notes, "candidates": [], "chosen": null }
        })));
        for i in 0..60 {
            app.handle(Msg::Event(json!({
                "type": "turn",
                "turn": { "id": format!("t{i}"), "role": "aster", "text": format!("line {i}") }
            })));
        }

        let text = screen(&app, 120, 40);
        // Wrapped, not clipped with an ellipsis.
        assert!(text.contains("ENDOFQUESTION"), "inbox text was cut off");
        // The conversation follows the newest line; the trace starts at the top.
        assert!(text.contains("line 59") && !text.contains("line 0 "));
        assert!(text.contains("note number 0") && !text.contains("note number 39"));
        assert!(text.contains("more"), "no hint that more is below");

        // Tab to the inbox, then the trace, and page it down to the end.
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.focus, Pane::Inbox);
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.focus, Pane::Trace);
        key(&mut app, KeyCode::End, KeyModifiers::CONTROL);
        let text = screen(&app, 120, 40);
        assert!(text.contains("note number 39"));

        // Scrolling the conversation up holds its place when new lines arrive.
        key(&mut app, KeyCode::BackTab, KeyModifiers::NONE);
        key(&mut app, KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(app.focus, Pane::Conversation);
        key(&mut app, KeyCode::Home, KeyModifiers::CONTROL);
        app.handle(Msg::Event(json!({
            "type": "turn", "turn": { "id": "new", "role": "aster", "text": "brand new line" }
        })));
        let text = screen(&app, 120, 40);
        assert!(text.contains("line 0 ") && !text.contains("brand new line"));
        key(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        assert!(!app.scrolls[Pane::Conversation as usize].follow);
        // Paging past the end reaches the bottom and follows again.
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        assert!(app.scrolls[Pane::Conversation as usize].follow);

        // Sending snaps back to the newest line.
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let text = screen(&app, 120, 40);
        assert!(text.contains("brand new line"));

        // Hiding the trace moves focus off it.
        app.focus = Pane::Trace;
        key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, Pane::Conversation);
        screen(&app, 30, 10);
    }
}
