//! Rendering.

use super::state::{App, Status};
use ansi_to_tui::IntoText;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;
use std::time::Duration;

fn fmt_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 10.0 {
        format!("{secs:.1}s")
    } else if secs < 600.0 {
        format!("{:.0}s", secs)
    } else {
        format!("{}m{:02}s", d.as_secs() / 60, d.as_secs() % 60)
    }
}

fn status_cell(status: &Status) -> Cell<'static> {
    match status {
        Status::Pending => Cell::from("·").style(Style::default().fg(Color::DarkGray)),
        Status::Running(_) => Cell::from("▶").style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Status::Ok => Cell::from("✓").style(
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Status::Skipped => Cell::from("–").style(Style::default().fg(Color::DarkGray)),
        Status::Failed => {
            Cell::from("✗").style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
        }
    }
}

fn detail_style(status: &Status) -> Style {
    match status {
        Status::Pending | Status::Skipped => Style::default().fg(Color::DarkGray),
        Status::Running(_) => Style::default().fg(Color::Yellow),
        Status::Ok => Style::default(),
        Status::Failed => Style::default().fg(Color::Red),
    }
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    if let Some(fatal) = &app.fatal {
        render_fatal(frame, area, fatal, app.done.is_some());
        return;
    }
    // Config refusals are security-relevant ("this branch tried to re-enable
    // install"), so they get their own strip rather than sharing the log pane,
    // where the first step's output would immediately bury them.
    let warnings = app.warnings();
    let warn_h = (warnings.len() as u16).min(3);
    let steps_needed = app.steps.len() as u16 + 2; // borders
    let min_log = 4u16;
    let available = area.height.saturating_sub(1 + 1 + warn_h); // header + footer + warnings
    let steps_h = steps_needed
        .min(available.saturating_sub(min_log))
        .max(3.min(available));
    let [header_area, warn_area, steps_area, log_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(warn_h),
        Constraint::Length(steps_h),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, header_area, app);
    if warn_h > 0 {
        render_warnings(frame, warn_area, warnings, warn_h as usize);
    }
    render_steps(frame, steps_area, app);
    render_log(frame, log_area, app);
    render_footer(frame, footer_area, app);
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let line = match &app.header {
        Some(h) => {
            let mut spans = vec![
                Span::styled(
                    " Worktree Setup ",
                    Style::default()
                        .add_modifier(Modifier::BOLD)
                        .fg(Color::Cyan),
                ),
                Span::raw("─ "),
                Span::styled(
                    h.repo_name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ];
            if let Some(branch) = &h.branch {
                spans.push(Span::raw(" · "));
                spans.push(Span::styled(
                    branch.clone(),
                    Style::default().fg(Color::Magenta),
                ));
            }
            spans.push(Span::raw(" ─ "));
            spans.push(Span::styled(
                h.target.display().to_string(),
                Style::default().fg(Color::DarkGray),
            ));
            if h.dry_run {
                spans.push(Span::styled(
                    "  DRY RUN",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            Line::from(spans)
        }
        None => Line::from(vec![
            Span::styled(
                " Worktree Setup ",
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Cyan),
            ),
            Span::styled(
                format!("─ {}…", app.preparing),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn render_steps(frame: &mut Frame, area: Rect, app: &App) {
    let rows: Vec<Row> = app
        .steps
        .iter()
        .enumerate()
        .map(|(i, step)| {
            let took = match (&step.status, step.took) {
                (Status::Running(since), _) => fmt_duration(since.elapsed()),
                (_, Some(d)) => fmt_duration(d),
                _ => String::new(),
            };
            let num = if i < 9 {
                format!("{} ", i + 1)
            } else {
                "  ".into()
            };
            Row::new(vec![
                status_cell(&step.status),
                Cell::from(Span::styled(num, Style::default().fg(Color::DarkGray))),
                Cell::from(step.meta.name.clone()).style(if step.status == Status::Failed {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default()
                }),
                Cell::from(step.detail.clone()).style(detail_style(&step.status)),
                Cell::from(Line::from(took).right_aligned())
                    .style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(22),
        Constraint::Min(10),
        Constraint::Length(7),
    ];
    let title = if app.steps.is_empty() {
        " steps ".to_string()
    } else {
        let done = app
            .steps
            .iter()
            .filter(|s| matches!(s.status, Status::Ok | Status::Skipped | Status::Failed))
            .count();
        format!(" steps {done}/{} ", app.steps.len())
    };
    let table = Table::new(rows, widths)
        .column_spacing(1)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default();
    if !app.steps.is_empty() {
        state.select(Some(app.selected));
    }
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_log(frame: &mut Frame, area: Rect, app: &mut App) {
    let inner_h = area.height.saturating_sub(2) as usize;
    app.log_viewport = inner_h.max(1);
    let title = match app.steps.get(app.selected) {
        Some(step) => format!(" output: {} ", step.meta.name),
        None => " output ".to_string(),
    };
    let log = app.selected_log();
    let offset = app.log_offset();
    let lines: Vec<Line> = log
        .iter()
        .skip(offset)
        .take(inner_h)
        .map(|raw| match raw.as_bytes().into_text() {
            Ok(text) => text.lines.into_iter().next().unwrap_or_default(),
            Err(_) => Line::from(raw.clone()),
        })
        .collect();
    let scroll_hint = if log.len() > inner_h {
        format!(
            " {}-{}/{} ",
            offset + 1,
            (offset + inner_h).min(log.len()),
            log.len()
        )
    } else {
        String::new()
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::DarkGray));
    if !scroll_hint.is_empty() {
        block = block.title_bottom(Line::from(scroll_hint).right_aligned());
    }
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
}

fn render_warnings(frame: &mut Frame, area: Rect, warnings: &[String], rows: usize) {
    let mut lines: Vec<Line> = Vec::with_capacity(rows);
    let shown = if warnings.len() > rows {
        rows - 1
    } else {
        warnings.len()
    };
    for warning in warnings.iter().take(shown) {
        lines.push(Line::from(vec![
            Span::styled(
                " ! ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(warning.clone(), Style::default().fg(Color::Yellow)),
        ]));
    }
    if warnings.len() > shown {
        lines.push(Line::from(Span::styled(
            format!(
                " ! …and {} more (see the plugin log)",
                warnings.len() - shown
            ),
            Style::default().fg(Color::Yellow),
        )));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let key = |k: &str| {
        Span::styled(
            k.to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    };
    let dim = |t: &str| Span::styled(t.to_string(), Style::default().fg(Color::DarkGray));
    let mut spans = vec![
        Span::raw(" "),
        key("j/k"),
        dim(" step  "),
        key("J/K"),
        dim(" scroll  "),
        key("g/G"),
        dim(" top/end  "),
    ];
    if app.has_failures() && !app.is_running() {
        spans.push(key("r"));
        spans.push(dim(" retry failed  "));
    }
    spans.push(key("q"));
    spans.push(dim(" close"));
    let status = if let Some(secs) = app.remaining_close_secs() {
        Span::styled(
            format!("closing in {secs}s (any key cancels) "),
            Style::default().fg(Color::Green),
        )
    } else if app.retrying {
        Span::styled("retrying… ".to_string(), Style::default().fg(Color::Yellow))
    } else {
        match app.done {
            Some(true) if app.dry_run => Span::styled(
                "dry run complete ".to_string(),
                Style::default().fg(Color::Yellow),
            ),
            Some(true) => Span::styled(
                "ready ✓ ".to_string(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Some(false) => Span::styled(
                "some steps failed ".to_string(),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            None if app.header.is_none() => Span::styled(
                "preparing… ".to_string(),
                Style::default().fg(Color::DarkGray),
            ),
            None => Span::styled("working… ".to_string(), Style::default().fg(Color::Yellow)),
        }
    };
    let left = Line::from(spans);
    let right = Line::from(status).right_aligned();
    let [l, r] = Layout::horizontal([Constraint::Min(10), Constraint::Length(40)]).areas(area);
    frame.render_widget(Paragraph::new(left), l);
    frame.render_widget(Paragraph::new(right), r);
}

fn render_fatal(frame: &mut Frame, area: Rect, message: &str, _finished: bool) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Worktree Setup: error ")
        .border_style(Style::default().fg(Color::Red));
    let text = Text::from(vec![
        Line::from(""),
        Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(Color::Red),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Press any key to close.",
            Style::default().fg(Color::DarkGray),
        )),
    ]);
    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }).block(block),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::pipeline::{Header, Outcome, StepMeta, WorkerEvent};
    use crate::ui::state::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    fn header(warnings: Vec<String>) -> Header {
        Header {
            repo_name: "clyras".into(),
            branch: Some("feature/x".into()),
            source: PathBuf::from("~/developer/clyras"),
            target: PathBuf::from("~/.herdr/worktrees/clyras/feature-x"),
            env_origin: "zsh -lic".into(),
            dry_run: false,
            layers: Vec::new(),
            warnings,
        }
    }

    fn steps() -> Vec<StepMeta> {
        vec![
            StepMeta {
                name: "copy dev state".into(),
                hint: "6 items".into(),
            },
            StepMeta {
                name: "bun install".into(),
                hint: "bun install".into(),
            },
        ]
    }

    /// Render into a fixed-size buffer and return it as plain text.
    fn screen(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A refusal ("this branch tried to re-enable install") must stay on screen
    /// even once the first step has filled the log pane.
    #[test]
    fn config_warnings_survive_step_output() {
        let mut app = App::new(&Config::default());
        app.apply(WorkerEvent::Plan {
            header: header(vec![
                "ignored .herdr-worktree.toml from the worktree".into(),
                ".herdr-worktree.toml: `install` can only be disabled by a repo config; ignored"
                    .into(),
            ]),
            steps: steps(),
        });
        app.apply(WorkerEvent::Started { idx: 0 });
        for i in 0..40 {
            app.apply(WorkerEvent::Line {
                idx: 0,
                text: format!("✓ file-{i}.env  (reflink)"),
            });
        }
        let out = screen(&mut app, 100, 24);
        assert!(
            out.contains("ignored .herdr-worktree.toml from the worktree"),
            "the branch-config warning is not on screen:\n{out}"
        );
        assert!(
            out.contains("can only be disabled"),
            "the refusal warning is not on screen:\n{out}"
        );
        assert!(out.contains("copy dev state"), "steps still render:\n{out}");
        assert!(out.contains("file-39.env"), "log still renders:\n{out}");
    }

    #[test]
    fn many_warnings_collapse_with_a_count() {
        let mut app = App::new(&Config::default());
        app.apply(WorkerEvent::Plan {
            header: header((0..6).map(|i| format!("warning number {i}")).collect()),
            steps: steps(),
        });
        let out = screen(&mut app, 100, 24);
        assert!(out.contains("warning number 0"));
        assert!(out.contains("and 4 more"), "no overflow hint:\n{out}");
    }

    #[test]
    fn a_clean_run_shows_no_warning_strip() {
        let mut app = App::new(&Config::default());
        app.apply(WorkerEvent::Plan {
            header: header(Vec::new()),
            steps: steps(),
        });
        app.apply(WorkerEvent::Finished {
            idx: 0,
            outcome: Outcome::Ok("6 items via reflink".into()),
            took: std::time::Duration::from_millis(20),
        });
        let out = screen(&mut app, 100, 24);
        assert!(!out.contains(" ! "), "a warning strip appeared:\n{out}");
        assert!(out.contains("6 items via reflink"));
        assert!(out.contains("clyras"), "header renders:\n{out}");
    }

    /// The pane can be short; the steps table must not squeeze the log away.
    #[test]
    fn renders_in_a_short_pane_without_panicking() {
        let mut app = App::new(&Config::default());
        app.apply(WorkerEvent::Plan {
            header: header(vec!["one warning".into()]),
            steps: steps(),
        });
        app.apply(WorkerEvent::Started { idx: 1 });
        app.apply(WorkerEvent::Line {
            idx: 1,
            text: "installing…".into(),
        });
        for height in [6u16, 8, 10, 14] {
            let out = screen(&mut app, 80, height);
            assert!(!out.is_empty(), "height {height} produced nothing");
        }
    }

    #[test]
    fn a_failure_is_visible_and_offers_retry() {
        let mut app = App::new(&Config::default());
        app.apply(WorkerEvent::Plan {
            header: header(Vec::new()),
            steps: steps(),
        });
        app.apply(WorkerEvent::Finished {
            idx: 0,
            outcome: Outcome::Failed("1 refused for safety".into()),
            took: std::time::Duration::from_millis(5),
        });
        app.apply(WorkerEvent::Done { ok: false });
        let out = screen(&mut app, 100, 24);
        assert!(out.contains("1 refused for safety"), "{out}");
        assert!(out.contains("retry failed"), "no retry affordance:\n{out}");
        assert!(out.contains("some steps failed"), "{out}");
    }
}
