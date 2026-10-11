use crate::model::EventKind;
use crate::scroll::Pane;
use crate::{App, Attention, Focus, Page};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

const CYAN: Color = Color::Rgb(68, 218, 216);
const OPUS: Color = Color::Rgb(183, 150, 254);
const ORANGE: Color = Color::Rgb(249, 179, 109);
const MUTED: Color = Color::Rgb(119, 137, 157);

fn block<'a>(title: &'a str, color: Color) -> Block<'a> {
    Block::default()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color))
}
/// Terminal-friendly "glow": a moving graded highlight along the outline.
/// Ratatui has no bloom shader; the streak changes border cell colour only,
/// retaining the existing glyphs, contents and interaction.
fn perimeter_point(area: Rect, index: usize) -> (u16, u16) {
    let w = area.width as usize;
    let h = area.height as usize;
    let top = w;
    let right = h.saturating_sub(1);
    let bottom = w.saturating_sub(1);
    if index < top {
        return (area.x + index as u16, area.y);
    }
    let mut i = index - top;
    if i < right {
        return (area.x + w as u16 - 1, area.y + 1 + i as u16);
    }
    i -= right;
    if i < bottom {
        return (area.x + (w - 2 - i) as u16, area.y + (h - 1) as u16);
    }
    i -= bottom;
    (area.x, area.y + (h - 2 - i) as u16)
}
/// One lap takes a fixed amount of time regardless of panel dimensions.
/// The old 76ms-per-cell animation needed 10-25 seconds per lap, while its
/// activity gate expired sooner. The result looked stuck or out of sync.
const BEAM_LAP_MS: u128 = 3_200;
fn beam_head(perimeter: usize, phase_ms: u128) -> usize {
    if perimeter == 0 {
        return 0;
    }
    ((phase_ms % BEAM_LAP_MS) * perimeter as u128 / BEAM_LAP_MS) as usize
}
fn glow_border(frame: &mut Frame, area: Rect, base: Color, phase_ms: u128) {
    if area.width < 4 || area.height < 4 {
        return;
    }
    let perimeter = (area.width as usize * 2) + (area.height as usize * 2) - 4;
    let head = beam_head(perimeter, phase_ms);
    // Wider tail is visible at 80ms refresh without relying on GPU effects.
    let trail = (perimeter / 12).clamp(7, 22);
    let (bright, dim) = if base == CYAN {
        ((244, 255, 255), (32, 91, 106))
    } else if base == OPUS {
        ((251, 240, 255), (75, 46, 123))
    } else {
        ((255, 248, 215), (113, 70, 32))
    };
    for distance in (0..trail).rev() {
        let at = (head + perimeter - distance) % perimeter;
        let (x, y) = perimeter_point(area, at);
        if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
            // Only illuminate actual border glyphs, not title text.
            if !matches!(cell.symbol(), "─" | "│" | "┌" | "┐" | "└" | "┘") {
                continue;
            }
            let numerator = (trail - distance) as u16;
            let d = trail as u16;
            let mix = |hi: u8, lo: u8| -> u8 {
                ((hi as u16 * numerator + lo as u16 * (d - numerator)) / d) as u8
            };
            cell.set_fg(Color::Rgb(
                mix(bright.0, dim.0),
                mix(bright.1, dim.1),
                mix(bright.2, dim.2),
            ));
            if distance == 0 {
                cell.set_bg(Color::Rgb(bright.0 / 10, bright.1 / 10, bright.2 / 10));
            }
        }
    }
}

/// Draws one bordered pane. Content longer than the pane scrolls from the pane's own offset,
/// and the title shows the position while it overflows. Lists do not wrap, because their rows
/// are already shortened to the pane width; everything else wraps so no text is cut off.
fn pane(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    which: Pane,
    title: &str,
    colour: Color,
    content: Vec<Line<'static>>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let wraps = !matches!(which, Pane::Folders | Pane::Issues | Pane::Timeline);
    let mut body = Paragraph::new(content);
    if wraps {
        body = body.wrap(Wrap { trim: false });
    }
    let view = app.scrolls.get(which);
    let inner_width = area.width.saturating_sub(2);
    let inner_height = area.height.saturating_sub(2) as usize;
    let top = view.sync(body.line_count(inner_width), inner_height);
    let title = match view.status() {
        Some(status) => format!("{title}  {status}"),
        None => title.to_owned(),
    };
    frame.render_widget(
        body.block(block(&title, colour))
            .scroll((top.min(u16::MAX as usize) as u16, 0)),
        area,
    );
}
fn divide(area: Rect, items: &[Constraint], horizontal: bool) -> Vec<Rect> {
    Layout::default()
        .direction(if horizontal {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints(items.to_vec())
        .split(area)
        .to_vec()
}
fn lines(s: &str) -> Vec<Line<'static>> {
    s.lines().map(|x| Line::raw(x.to_owned())).collect()
}
fn accent(text: String, style: Color) -> Line<'static> {
    Line::styled(text, Style::default().fg(style))
}
fn small_frame(frame: &mut Frame, msg: &str) {
    frame.render_widget(
        Paragraph::new(msg).block(block(" CLAUDE AGENT TREE ", ORANGE)),
        frame.area(),
    );
}
fn header(frame: &mut Frame, area: Rect, page: Page, demo: bool) {
    let mode = if demo {
        "DEMO - OFFLINE"
    } else if page == Page::Running {
        "LIVE NATIVE TEAM"
    } else {
        "WINDOWS / CONPTY"
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "  CLAUDE CODE AGENT TREE  ",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("HAIKU 5.5 TEAM  ", Style::default().fg(CYAN)),
            Span::styled("OPUS ON CALL  ", Style::default().fg(OPUS)),
            Span::styled(
                mode,
                Style::default().fg(if demo { Color::Yellow } else { Color::Green }),
            ),
        ]))
        .alignment(ratatui::layout::Alignment::Center)
        .block(block("", MUTED)),
        area,
    );
}

// Layout is computed in one place per page. Drawing and mouse hit-testing both use these
// functions, so a wheel event always reaches the pane that is actually on screen.
fn shell(area: Rect) -> Vec<Rect> {
    divide(
        area,
        &[
            Constraint::Length(3),
            Constraint::Min(13),
            Constraint::Length(4),
        ],
        false,
    )
}
fn setup_parts(body: Rect) -> Vec<Rect> {
    divide(
        body,
        &[
            Constraint::Length(5),
            Constraint::Min(10),
            Constraint::Length(8),
        ],
        false,
    )
}
fn browser_parts(body: Rect) -> Vec<Rect> {
    divide(body, &[Constraint::Length(4), Constraint::Min(5)], false)
}
/// Header, issue list, preview. Narrow terminals stack list above preview; wide ones go side by side.
fn issues_parts(body: Rect) -> Vec<Rect> {
    let parts = divide(body, &[Constraint::Length(4), Constraint::Min(5)], false);
    let panes = divide(
        parts[1],
        &[Constraint::Percentage(50), Constraint::Percentage(50)],
        body.width >= 100,
    );
    vec![parts[0], panes[0], panes[1]]
}

fn setup(frame: &mut Frame, body: Rect, app: &App) {
    let parts = setup_parts(body);
    let project_label = if app.focus == Focus::Project {
        "PROJECT PATH  < EDITING >"
    } else {
        "PROJECT PATH"
    };
    pane(
        frame,
        parts[0],
        app,
        Pane::Project,
        project_label,
        if app.focus == Focus::Project {
            CYAN
        } else {
            MUTED
        },
        vec![
            Line::raw(
                "Type full directory path. F2: browse folders, Up/Down: recent project.".to_owned(),
            ),
            accent(
                format!(
                    "{}{}",
                    app.project,
                    if app.focus == Focus::Project {
                        "█"
                    } else {
                        ""
                    }
                ),
                CYAN,
            ),
        ],
    );
    let objective_label = if app.focus == Focus::Objective {
        "OBJECTIVE  < EDITING >  |  Ctrl+L clear all"
    } else {
        "OBJECTIVE  |  Ctrl+L clear all"
    };
    let mut content = lines(&app.objective);
    if content.is_empty() {
        content.push(accent(
            "Explain exactly what the agents should accomplish...".into(),
            MUTED,
        ));
    }
    if app.focus == Focus::Objective {
        content.push(accent("█".into(), CYAN));
    }
    pane(
        frame,
        parts[1],
        app,
        Pane::Objective,
        objective_label,
        if app.focus == Focus::Objective {
            CYAN
        } else {
            MUTED
        },
        content,
    );
    let mut history = vec![accent(
        "RECENT PROJECTS  (Up/Down on project field)".to_owned(),
        OPUS,
    )];
    history.extend(
        app.recent
            .iter()
            .take(3)
            .map(|x| Line::raw(format!("  {x}"))),
    );
    pane(frame, parts[2], app, Pane::Recent, "LAUNCH", OPUS, history);
}
fn browser(frame: &mut Frame, body: Rect, app: &App) {
    let parts = browser_parts(body);
    pane(
        frame,
        parts[0],
        app,
        Pane::BrowserPath,
        "CURRENT DIRECTORY",
        CYAN,
        vec![
            Line::raw(app.browser_dir.display().to_string()),
            accent(
                "Up/Down select   Enter open folder   S select this folder   Esc back".into(),
                CYAN,
            ),
        ],
    );
    let items = app
        .browser_entries
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let name = if i == 0 {
                "..  [PARENT DIRECTORY]".into()
            } else {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            };
            Line::styled(
                format!(
                    " {} {}",
                    if i == app.browser_index { "▶" } else { " " },
                    name
                ),
                Style::default()
                    .fg(if i == app.browser_index {
                        Color::Black
                    } else {
                        Color::Gray
                    })
                    .bg(if i == app.browser_index {
                        CYAN
                    } else {
                        Color::Reset
                    }),
            )
        })
        .collect::<Vec<_>>();
    pane(frame, parts[1], app, Pane::Folders, "FOLDERS", MUTED, items);
}
fn issues(frame: &mut Frame, body: Rect, app: &App) {
    let browser = &app.issues;
    let parts = issues_parts(body);
    let repo = browser
        .repo
        .as_ref()
        .map(|r| r.slug())
        .unwrap_or_else(|| "GitHub".into());
    let visible = browser.visible();
    pane(
        frame,
        parts[0],
        app,
        Pane::IssuesHeader,
        "OPEN ISSUES",
        CYAN,
        vec![
            Line::raw(format!(
                "{repo}  |  {} of {} shown  |  Filter: {}{}",
                visible.len(),
                browser.issues.len(),
                browser.filter,
                if browser.loading { "  (loading)" } else { "" }
            )),
            accent(
                "Type to filter   Up/Down/PgUp/PgDn select   Tab list/preview   Enter use   Ctrl+R refresh   Esc back"
                    .into(),
                CYAN,
            ),
        ],
    );
    if visible.is_empty() {
        let status = if browser.status.is_empty() {
            "No issues match the filter.".to_owned()
        } else {
            browser.status.clone()
        };
        pane(
            frame,
            parts[1],
            app,
            Pane::Issues,
            "ISSUES",
            MUTED,
            vec![Line::raw(status)],
        );
        pane(
            frame,
            parts[2],
            app,
            Pane::Preview,
            "PREVIEW",
            MUTED,
            vec![],
        );
        return;
    }
    let width = parts[1].width.saturating_sub(4) as usize;
    let items = visible
        .iter()
        .enumerate()
        .map(|(i, issue)| {
            let selected = i == browser.index;
            let milestone = issue
                .milestone
                .as_ref()
                .map(|m| format!("  [{}]", m.title))
                .unwrap_or_default();
            Line::styled(
                shorten(
                    &format!(
                        " {} #{} {}{}",
                        if selected { "▶" } else { " " },
                        issue.number,
                        issue.title.trim(),
                        milestone
                    ),
                    width,
                ),
                Style::default()
                    .fg(if selected { Color::Black } else { Color::Gray })
                    .bg(if selected { CYAN } else { Color::Reset }),
            )
        })
        .collect::<Vec<_>>();
    pane(
        frame,
        parts[1],
        app,
        Pane::Issues,
        &status_title(&browser.status),
        MUTED,
        items,
    );
    let mut preview = vec![];
    if let Some(issue) = browser.selected() {
        preview = vec![
            Line::styled(
                format!("#{} {}", issue.number, issue.title.trim()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            accent(
                format!(
                    "State: {}  |  Milestone: {}",
                    if issue.state.is_empty() {
                        "unknown"
                    } else {
                        issue.state.as_str()
                    },
                    issue
                        .milestone
                        .as_ref()
                        .map_or("none", |m| m.title.as_str())
                ),
                OPUS,
            ),
        ];
        if !issue.labels.is_empty() {
            let names: Vec<&str> = issue.labels.iter().map(|l| l.name.as_str()).collect();
            preview.push(Line::raw(format!("Labels: {}", names.join(", "))));
        }
        preview.push(Line::raw(issue.url.clone()));
        preview.push(Line::raw(""));
        let body = issue.body();
        if body.is_empty() {
            preview.push(accent("(no description)".into(), MUTED));
        } else {
            preview.extend(body.lines().map(|l| Line::raw(l.to_owned())));
        }
    }
    pane(
        frame,
        parts[2],
        app,
        Pane::Preview,
        "PREVIEW",
        CYAN,
        preview,
    );
}
fn status_title(status: &str) -> String {
    if status.is_empty() {
        "ISSUES".to_owned()
    } else {
        format!("ISSUES  |  {}", shorten(status, 60))
    }
}
fn vt_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}
fn render_terminal(frame: &mut Frame, area: Rect, app: &App) {
    if area.height < 3 {
        return;
    }
    let (rows, cols) = app.terminal_size;
    let screen = app.terminal.screen();
    // vt100 keeps the history; a non-zero offset means the reader is looking back.
    let back = screen.scrollback();
    let (cursor_y, cursor_x) = screen.cursor_position();
    // The cursor belongs to the live screen, so it is hidden while reading history.
    let show_cursor = back == 0 && app.focus == Focus::Native;
    let mut output: Vec<Line> = Vec::with_capacity(rows as usize);
    for y in 0..rows {
        let mut spans: Vec<Span> = Vec::new();
        let mut text = String::new();
        let mut last_style: Option<Style> = None;
        for x in 0..cols {
            if let Some(cell) = screen.cell(y, x) {
                // vt100 0.15.x exposes is_wide() on the leading cell. Skip the
                // following column to avoid rendering wide glyphs twice.
                if x > 0 && screen.cell(y, x - 1).is_some_and(|prev| prev.is_wide()) {
                    continue;
                }
                let mut style = Style::default()
                    .fg(vt_color(cell.fgcolor()))
                    .bg(vt_color(cell.bgcolor()));
                if cell.bold() {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if cell.italic() {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if cell.underline() {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if cell.inverse() || (show_cursor && y == cursor_y && x == cursor_x) {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                let content = if cell.has_contents() {
                    cell.contents().to_owned()
                } else {
                    " ".to_string()
                };
                if last_style != Some(style) {
                    if let Some(prev) = last_style {
                        spans.push(Span::styled(std::mem::take(&mut text), prev));
                    }
                    last_style = Some(style);
                }
                text.push_str(&content);
            }
        }
        if let Some(style) = last_style {
            spans.push(Span::styled(text, style));
        }
        output.push(Line::from(spans));
    }
    let title = if back > 0 {
        format!("CLAUDE TERMINAL  |  history -{back}  |  Ctrl+End live")
    } else {
        "INTERACTIVE CLAUDE TERMINAL  |  F6 focus".to_owned()
    };
    frame.render_widget(Paragraph::new(output).block(block(&title, ORANGE)), area);
}

// The dashboard retains Claude as the only orchestrator. All boxes render
// observed task/session data; a missing cost or status is shown as unknown.
fn shorten(value: &str, max: usize) -> String {
    let len = value.chars().count();
    if len <= max {
        return value.to_owned();
    }
    format!(
        "{}…",
        value
            .chars()
            .take(max.saturating_sub(1))
            .collect::<String>()
    )
}
fn count(v: u64) -> String {
    if v >= 1_000_000 {
        format!("{:.2}m", v as f64 / 1_000_000.)
    } else if v >= 1_000 {
        format!("{:.1}k", v as f64 / 1_000.)
    } else {
        v.to_string()
    }
}
fn uptime(seconds: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}
/// An estimate only when the normalised `Usage.cost_usd` is observed. Otherwise it is a lower bound.
fn cost_text(t: &crate::telemetry::Totals, basis: &str) -> String {
    if t.calls == 0 {
        return "Cost unknown".into();
    }
    match t.usage().cost_usd.get() {
        Some(usd) => format!("Cost ~${usd:.4} {basis}"),
        None => format!(
            "Cost >=${:.4} + {}",
            t.list_usd,
            if t.has_pricing_gap {
                "unpriced"
            } else {
                "incomplete usage"
            }
        ),
    }
}
fn stats_lines(t: Option<&crate::telemetry::Totals>, max: usize) -> Vec<Line<'static>> {
    let Some(t) = t else {
        return vec![
            accent("Tokens / cost: not linked".into(), MUTED),
            accent("Context: unknown".into(), MUTED),
        ];
    };
    let context = if t.calls > 0 {
        format!("{:.1}%", t.context_percent())
    } else {
        "--".into()
    };
    vec![
        Line::raw(format!(
            "Tokens {}  I:{}  O:{}",
            count(t.tokens()),
            count(t.input),
            count(t.output)
        )),
        Line::raw(format!(
            "Cache R:{}  W:{}",
            count(t.cache_read),
            count(t.cache_write)
        )),
        accent(format!("Context ~{} / 1m", context), CYAN),
        accent(cost_text(t, "list"), ORANGE),
        Line::raw(shorten(
            &format!(
                "{} | {} calls",
                if t.model.is_empty() {
                    "Model unknown"
                } else {
                    &t.model
                },
                t.calls
            ),
            max,
        )),
    ]
}
fn active_agent(app: &App, role: &str) -> bool {
    !app.output_end
        && !app.live.objective_complete
        && !app.telemetry.review.objective_complete
        && app.activity.recent(role, std::time::Duration::from_secs(5))
}
fn review_started(app: &App) -> bool {
    app.live.review_started || app.telemetry.review.started
}
fn review_finished(app: &App) -> bool {
    app.live.objective_complete || app.telemetry.review.objective_complete
}
fn lead_and_opus(frame: &mut Frame, lead_area: Rect, advisor_area: Rect, app: &App) {
    let lead = app.telemetry.actors.get("main");
    let mut content = vec![
        accent(
            if active_agent(app, "main") {
                "HAIKU 5.5 // RECENT OBSERVED ACTIVITY".into()
            } else {
                "HAIKU 5.5 // PLANS + COORDINATES".into()
            },
            ORANGE,
        ),
        Line::raw(format!(
            "Runtime: {} | tools: {}",
            uptime(app.run_elapsed().as_secs()),
            lead.map(|t| t.tools).unwrap_or(0)
        )),
    ];
    content.extend(stats_lines(
        lead,
        lead_area.width.saturating_sub(3) as usize,
    ));
    pane(
        frame,
        lead_area,
        app,
        Pane::Lead,
        "HAIKU 5.5 / MAIN",
        ORANGE,
        content,
    );
    if active_agent(app, "main") {
        glow_border(frame, lead_area, ORANGE, app.started.elapsed().as_millis());
    }
    let mut advisor = vec![accent("OPUS 5.5  //  ON CALL".into(), OPUS)];
    let calls = app
        .telemetry
        .advisor_invocations
        .max(app.telemetry.advisor.calls);
    let observed = app.live.advisor_seen || calls > 0 || app.telemetry.observed_opus;
    if calls > 0 {
        advisor.push(Line::raw(format!("Advisor calls observed: {calls}")));
        if app.telemetry.advisor_completed > 0 {
            advisor.push(Line::raw(format!(
                "Results observed: {}",
                app.telemetry.advisor_completed
            )));
        }
    } else if app.telemetry.observed_opus {
        advisor.push(accent("Opus use recorded; type unknown".into(), OPUS));
    } else if observed {
        advisor.push(accent("Advisor seen in Claude UI".into(), OPUS));
        advisor.push(Line::raw("Count not available in transcript"));
    } else {
        advisor.push(accent("No advisor invocation observed".into(), MUTED));
    }
    if app.telemetry.advisor.calls > 0 {
        let t = &app.telemetry.advisor;
        advisor.push(Line::raw(format!(
            "Tokens {}  I:{}  O:{}",
            count(t.tokens()),
            count(t.input),
            count(t.output)
        )));
        advisor.push(accent(cost_text(t, "API list"), ORANGE));
    } else if observed {
        advisor.push(Line::raw("Advisor tokens/cost: unavailable"));
    }
    if app.live.advisor_in_flight {
        advisor.push(accent("Currently advising (live terminal)".into(), OPUS));
    }
    advisor.push(accent("Advice does not confirm final review".into(), MUTED));
    pane(
        frame,
        advisor_area,
        app,
        Pane::Advisor,
        "OPUS ADVISOR",
        OPUS,
        advisor,
    );
    let advisor_busy = !review_finished(app)
        && (app.live.advisor_in_flight
            || (calls > app.telemetry.advisor_completed
                && app
                    .activity
                    .recent("main", std::time::Duration::from_secs(5))));
    if advisor_busy {
        glow_border(frame, advisor_area, OPUS, app.started.elapsed().as_millis());
    }
}
// Summary strip. This deliberately does not claim to perform routing or
// System One/JEV reasoning: native Claude Code owns orchestration.
fn run_totals(frame: &mut Frame, area: Rect, app: &App) {
    let elapsed = uptime(app.run_elapsed().as_secs());
    let status = if review_finished(app) {
        match app
            .telemetry
            .review
            .result
            .as_deref()
            .or(app.live.review_result.as_deref())
        {
            Some("PASS")
                if app.telemetry.review.advisor_evidence || app.live.advisor_after_review =>
            {
                "COMPLETE / OPUS OBSERVED"
            }
            Some("PASS") => "COMPLETE / REVIEW UNVERIFIED",
            Some("NEEDS_CHANGES") => "ENDED / NEEDS CHANGES",
            Some("BLOCKED") => "ENDED / BLOCKED",
            _ => "OBJECTIVE ENDED / REVIEW UNKNOWN",
        }
    } else if app.output_end {
        "CLAUDE SESSION ENDED"
    } else if review_started(app) {
        "FINAL REVIEW STAGE"
    } else if app.clock_paused_manually {
        "CLOCK PAUSED MANUALLY"
    } else if app.attention.is_some() {
        "NEEDS INPUT"
    } else if app.team.detected {
        "NATIVE TEAM ACTIVE"
    } else if !app.native_screen_visible && !app.demo {
        "CLAUDE STARTING"
    } else {
        "WAITING FOR TEAM"
    };
    let t = &app.telemetry.combined;
    let total = if app.telemetry.files_found == 0 {
        "Observed tokens: --    Est. API cost: --".to_owned()
    } else {
        format!(
            "Observed {} tokens  (I:{} O:{} Cache R:{} W:{})",
            count(t.tokens()),
            count(t.input),
            count(t.output),
            count(t.cache_read),
            count(t.cache_write)
        )
    };
    let missing_advisor_cost = (app.telemetry.advisor_invocations > 0 || app.live.advisor_seen)
        && app.telemetry.advisor.calls == 0;
    let spend = if app.telemetry.files_found == 0 || t.calls == 0 {
        "Estimated API cost: --".to_owned()
    } else {
        match t.usage().cost_usd.get() {
            Some(usd) if !missing_advisor_cost => format!("Estimated API cost ~${usd:.4} USD list"),
            _ => format!("Estimated API cost >=${:.4} + unobserved usage", t.list_usd),
        }
    };
    pane(
        frame,
        area,
        app,
        Pane::Totals,
        "RUN TOTALS / OBSERVED",
        CYAN,
        vec![
            accent(
                format!(
                    "{}  |  Runtime {}{}  |  {} teammates  |  {} tools",
                    status,
                    elapsed,
                    if app.finished_elapsed.is_some() {
                        " (stopped)"
                    } else {
                        ""
                    },
                    app.team.agents.len(),
                    t.tools
                ),
                CYAN,
            ),
            Line::raw(total),
            accent(
                format!(
                    "{}  |  {}",
                    spend,
                    shorten(
                        &app.telemetry.coverage,
                        area.width.saturating_sub(40) as usize
                    )
                ),
                ORANGE,
            ),
        ],
    );
    if !app.output_end
        && !review_finished(app)
        && app.activity.any_recent(std::time::Duration::from_secs(5))
    {
        glow_border(frame, area, CYAN, app.started.elapsed().as_millis());
    }
}
fn worker_pane(idx: usize) -> Pane {
    [Pane::Worker0, Pane::Worker1, Pane::Worker2][idx]
}
fn worker_card(frame: &mut Frame, area: Rect, app: &App, idx: usize) {
    let Some(worker) = app.team.agents.get(idx) else {
        pane(
            frame,
            area,
            app,
            worker_pane(idx),
            "HAIKU / AVAILABLE",
            CYAN,
            vec![accent("Waiting for Claude teammate".into(), MUTED)],
        );
        return;
    };
    let max = area.width.saturating_sub(3) as usize;
    let state_label = if active_agent(app, &worker.name) {
        format!("{} / RECENT I/O", worker.state)
    } else {
        worker.state.clone()
    };
    let mut content = vec![
        accent(
            shorten(&format!("{}  •  {}", worker.name, state_label), max),
            CYAN,
        ),
        Line::raw(shorten(
            if worker.task.is_empty() {
                if worker.role.is_empty() {
                    "No task assigned"
                } else {
                    &worker.role
                }
            } else {
                &worker.task
            },
            max,
        )),
    ];
    let observed = app
        .teammate_elapsed(&worker.name)
        .map(|i| uptime(i.as_secs()))
        .unwrap_or_else(|| "--".into());
    content.push(Line::raw(format!("Since first seen: {}", observed)));
    content.extend(stats_lines(app.telemetry.actors.get(&worker.name), max));
    if let Some(t) = app.telemetry.actors.get(&worker.name) {
        if !t.action.is_empty() {
            content.push(Line::raw(shorten(&format!("Last: {}", t.action), max)));
        }
    }
    pane(
        frame,
        area,
        app,
        worker_pane(idx),
        &format!("HAIKU / {}", shorten(&worker.name, max.saturating_sub(10))),
        CYAN,
        content,
    );
    if worker.state != "TASK COMPLETE"
        && worker.state != "WAITING"
        && active_agent(app, &worker.name)
    {
        glow_border(
            frame,
            area,
            CYAN,
            app.started.elapsed().as_millis() + idx as u128 * 470,
        );
    }
}
fn workers(frame: &mut Frame, areas: [Rect; 3], app: &App) {
    for (i, part) in areas.into_iter().enumerate() {
        worker_card(frame, part, app, i);
    }
}
fn review_strip(frame: &mut Frame, area: Rect, app: &App) {
    let review = &app.telemetry.review;
    let started = review_started(app);
    let ended = review_finished(app);
    let result = review.result.as_ref().or(app.live.review_result.as_ref());
    let opus_seen = review.advisor_evidence || app.live.advisor_after_review;
    let label = if ended {
        "FINAL AUDIT RESULT REPORTED"
    } else if started {
        "FINAL AUDIT IN PROGRESS"
    } else {
        "FINAL AUDIT PENDING"
    };
    let detail = match result.map(String::as_str) {
        Some(status) if review.advisor_evidence => {
            format!("{} reported / Opus observed after audit started", status)
        }
        Some(status) if opus_seen => format!("{} reported / Opus call seen in terminal", status),
        Some(status) => format!(
            "{} reported / Opus review not independently confirmed",
            status
        ),
        None if started && app.live.advisor_in_flight && app.live.advisor_after_review => {
            "Opus final consultation active (live terminal)".into()
        }
        None if started && opus_seen => "Opus consultation observed; awaiting lead verdict".into(),
        None if started => "Review requested; awaiting Opus consultation / result".into(),
        None => "No final review stage observed yet".into(),
    };
    pane(
        frame,
        area,
        app,
        Pane::Review,
        "OPUS 5.5 / FINAL REVIEW",
        OPUS,
        vec![
            accent(label.into(), OPUS),
            Line::raw(shorten(&detail, area.width.saturating_sub(3) as usize)),
        ],
    );
    // A review announcement by itself must NOT animate an idle Opus panel.
    if started && !ended && app.live.advisor_in_flight && app.live.advisor_after_review {
        glow_border(frame, area, OPUS, app.started.elapsed().as_millis() + 400);
    }
}
/// UTC clock time of an event, or a placeholder when the source had no valid timestamp.
fn clock(at: Option<u64>) -> String {
    match at {
        Some(ms) => {
            let s = (ms / 1000) % 86_400;
            format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
        }
        None => "--:--:--".into(),
    }
}
fn event_text(kind: &EventKind) -> String {
    match kind {
        EventKind::ToolUsed { tool, target } => {
            if target.is_empty() {
                tool.clone()
            } else {
                format!("{tool} {target}")
            }
        }
        EventKind::AdvisorInvoked => "Consultation invoked".into(),
        EventKind::AdvisorCompleted => "Consultation completed".into(),
        EventKind::ReviewStarted => "Review stage requested by lead".into(),
        EventKind::ObjectiveCompleted => "Objective marked complete".into(),
    }
}
fn timeline(frame: &mut Frame, area: Rect, app: &App) {
    let rows: Vec<Line<'static>> = if app.telemetry.events.is_empty() {
        vec![accent(
            "Waiting for tool activity in local Claude JSONL transcripts".into(),
            MUTED,
        )]
    } else {
        app.telemetry
            .events
            .iter()
            .map(|e| {
                Line::from(vec![
                    Span::styled(format!("{}  ", clock(e.at)), Style::default().fg(MUTED)),
                    Span::styled(
                        format!("{:<16}", shorten(&e.actor, 15)),
                        Style::default().fg(CYAN),
                    ),
                    Span::raw(shorten(&event_text(&e.kind), 70)),
                ])
            })
            .collect()
    };
    pane(
        frame,
        area,
        app,
        Pane::Timeline,
        "SESSION LOG / LIVE TOOL ACTIVITY",
        MUTED,
        rows,
    );
}
/// Every dashboard panel's rectangle, shared by drawing and hit-testing.
struct Dash {
    short: bool,
    totals: Rect,
    lead: Rect,
    advisor: Rect,
    workers: [Rect; 3],
    review: Rect,
    timeline: Rect,
}
fn dash_layout(area: Rect) -> Dash {
    let none = Rect::default();
    if area.height < 24 {
        let p = divide(area, &[Constraint::Length(5), Constraint::Min(5)], false);
        return Dash {
            short: true,
            totals: p[0],
            lead: none,
            advisor: none,
            workers: [none; 3],
            review: none,
            timeline: p[1],
        };
    }
    let parts = divide(
        area,
        &[
            Constraint::Length(5),
            Constraint::Length(9),
            Constraint::Min(10),
            Constraint::Length(4),
            Constraint::Length(7),
        ],
        false,
    );
    let lead = divide(
        parts[1],
        &[Constraint::Percentage(68), Constraint::Percentage(32)],
        true,
    );
    let w = divide(
        parts[2],
        &[
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ],
        parts[2].width >= 65,
    );
    Dash {
        short: false,
        totals: parts[0],
        lead: lead[0],
        advisor: lead[1],
        workers: [w[0], w[1], w[2]],
        review: parts[3],
        timeline: parts[4],
    }
}
fn dash_regions(area: Rect) -> Vec<(Pane, Rect)> {
    let d = dash_layout(area);
    if d.short {
        return vec![(Pane::Totals, d.totals), (Pane::Timeline, d.timeline)];
    }
    vec![
        (Pane::Totals, d.totals),
        (Pane::Lead, d.lead),
        (Pane::Advisor, d.advisor),
        (Pane::Worker0, d.workers[0]),
        (Pane::Worker1, d.workers[1]),
        (Pane::Worker2, d.workers[2]),
        (Pane::Review, d.review),
        (Pane::Timeline, d.timeline),
    ]
}
fn agent_dashboard(frame: &mut Frame, area: Rect, app: &App) {
    let d = dash_layout(area);
    run_totals(frame, d.totals, app);
    if d.short {
        timeline(frame, d.timeline, app);
        return;
    }
    lead_and_opus(frame, d.lead, d.advisor, app);
    workers(frame, d.workers, app);
    review_strip(frame, d.review, app);
    timeline(frame, d.timeline, app);
}
/// The approval overlay, centred over the running page. Only shown while Claude is waiting.
fn attention_rect(body: Rect, app: &App) -> Option<Rect> {
    app.attention
        .as_ref()
        .filter(|_| app.focus == Focus::Response)?;
    let width = body.width.saturating_sub(10).min(106);
    let height = 13u16.min(body.height.saturating_sub(2)).max(4);
    Some(Rect {
        x: body.x + (body.width.saturating_sub(width)) / 2,
        y: body.y + (body.height.saturating_sub(height)) / 2,
        width,
        height,
    })
}
fn attention_panel(frame: &mut Frame, area: Rect, att: &Attention, app: &App) {
    if area.width < 30 || area.height < 4 {
        return;
    }
    let content = vec![
        accent(format!("ISSUE: {}", att.title), Color::Yellow),
        Line::raw(format!("WHY: {}", att.reason)),
        accent(format!("QUESTION: {}", att.question), CYAN),
        Line::raw(if att.options.is_empty() {
            "".into()
        } else {
            format!("OPTIONS: {}", att.options.join("  |  "))
        }),
        Line::raw(format!(
            "ANSWER: {}{}",
            app.response,
            if app.focus == Focus::Response {
                "█"
            } else {
                ""
            }
        )),
        accent(
            "F7 edit answer  ·  Enter send  ·  F6 native Claude controls".into(),
            MUTED,
        ),
    ];
    frame.render_widget(Clear, area);
    pane(
        frame,
        area,
        app,
        Pane::Attention,
        "NEEDS ATTENTION  //  HUMAN INPUT REQUIRED",
        Color::Yellow,
        content,
    );
}
/// Where the running page places each pane. The attention overlay, when shown, comes last.
fn running_regions(body: Rect, app: &App) -> Vec<(Pane, Rect)> {
    let mut out = Vec::new();
    if app.dashboard_expanded {
        out.extend(dash_regions(body));
    } else if app.terminal_expanded {
        out.push((Pane::Terminal, body));
    } else {
        let seg = divide(
            body,
            &[Constraint::Percentage(58), Constraint::Percentage(42)],
            true,
        );
        out.extend(dash_regions(seg[0]));
        out.push((Pane::Terminal, seg[1]));
    }
    if let Some(rect) = attention_rect(body, app) {
        out.push((Pane::Attention, rect));
    }
    out
}
fn running(frame: &mut Frame, body: Rect, app: &App) {
    if app.dashboard_expanded {
        agent_dashboard(frame, body, app);
    } else if app.terminal_expanded {
        render_terminal(frame, body, app);
    } else {
        let segments = divide(
            body,
            &[Constraint::Percentage(58), Constraint::Percentage(42)],
            true,
        );
        agent_dashboard(frame, segments[0], app);
        render_terminal(frame, segments[1], app);
    }
    if let (Some(a), Some(rect)) = (app.attention.as_ref(), attention_rect(body, app)) {
        attention_panel(frame, rect, a, app);
    }
}
fn footer(frame: &mut Frame, area: Rect, app: &App) {
    let text = match app.page {
        Page::Setup => {
            "F1 controls | Tab | F2 folders | F3 issues | Ctrl+L clear | F5 start | Esc".to_owned()
        }
        Page::Browser => {
            "F1 controls | Up/Down browse | Enter folder | S select | Esc back".to_owned()
        }
        Page::Issues => {
            "F1 controls | Type filter | Up/Down | Enter use | Tab | Ctrl+R | Esc".to_owned()
        }
        Page::Running => {
            if app.close_confirm {
                "CTRL+Q again to exit and terminate embedded Claude".into()
            } else {
                "F1 controls | F4 clock | F5 panel | F6 Claude | F8 terminal | Ctrl+Q x2".into()
            }
        }
        Page::Finished => {
            "Claude session ended | F10 or Enter new project/objective | Esc quit".into()
        }
    };
    pane(
        frame,
        area,
        app,
        Pane::Footer,
        "CONTROLS / STATUS",
        MUTED,
        vec![accent(text, CYAN), Line::raw(app.message.clone())],
    );
}

/// Every control, in one scrollable panel. F1 opens it on any page.
fn help_lines() -> Vec<Line<'static>> {
    let section = |title: &str| accent(title.to_owned(), ORANGE);
    vec![
        section("SCROLLING (every panel)"),
        Line::raw("  Mouse wheel             scroll the panel under the pointer"),
        Line::raw("  Ctrl+Up / Ctrl+Down     scroll the focused panel one line"),
        Line::raw("  Ctrl+PgUp / Ctrl+PgDn   scroll one page"),
        Line::raw("  Ctrl+Home / Ctrl+End    top / bottom"),
        Line::raw("  Scrolling never changes the selection or sends keys to Claude."),
        section("SETUP"),
        Line::raw("  Tab switch project/objective    Ctrl+Up/Down scroll the objective"),
        Line::raw("  F2 folders   F3 GitHub issues   F5 start   Ctrl+L clear   Esc quit"),
        section("GITHUB ISSUES"),
        Line::raw("  Up/Down PgUp/PgDn select; the list follows the selection"),
        Line::raw("  Tab moves Ctrl+scroll between the list and the preview"),
        Line::raw("  Enter use issue   Ctrl+R refresh   Esc back (cancels a pending load)"),
        section("RUNNING"),
        Line::raw("  F6 focus Claude   F7 answer attention   F8 full terminal   F9 full dashboard"),
        Line::raw("  F5 select the next dashboard panel   F4 pause/resume clock"),
        Line::raw("  Claude terminal history: Ctrl+Up/Down, Ctrl+PgUp/PgDn, Ctrl+Home (oldest)."),
        Line::raw(
            "  Ctrl+End or typing returns to the live screen. Output keeps arriving meanwhile.",
        ),
        Line::raw("  Fullscreen Claude (alternate screen) has no history. The wheel is passed"),
        Line::raw("  to Claude only when Claude asked for mouse input."),
        Line::raw("  Mouse capture is on, so text selection needs your terminal's bypass key."),
        Line::raw("  Ctrl+Q twice quit"),
        section("HELP"),
        Line::raw("  F1 or Esc close this panel"),
    ]
}
fn help_rect(area: Rect) -> Rect {
    let width = area.width.saturating_sub(4).min(92);
    let height = area.height.saturating_sub(2).clamp(4, 30);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}
fn help(frame: &mut Frame, area: Rect, app: &App) {
    frame.render_widget(Clear, area);
    pane(
        frame,
        area,
        app,
        Pane::Help,
        "CONTROLS  |  wheel or Ctrl+Up/Down scroll  |  F1 close",
        CYAN,
        help_lines(),
    );
}

/// Every pane on the current page, with its rectangle. Mouse hit-testing reads this, and
/// `draw` lays out from the same helpers, so they agree on what is on screen.
pub fn regions(area: Rect, app: &App) -> Vec<(Pane, Rect)> {
    if area.width < 78 || area.height < 21 {
        return vec![];
    }
    let shell = shell(area);
    let body = shell[1];
    let mut out = match app.page {
        Page::Setup => {
            let p = setup_parts(body);
            vec![
                (Pane::Project, p[0]),
                (Pane::Objective, p[1]),
                (Pane::Recent, p[2]),
            ]
        }
        Page::Browser => {
            let p = browser_parts(body);
            vec![(Pane::BrowserPath, p[0]), (Pane::Folders, p[1])]
        }
        Page::Issues => {
            let p = issues_parts(body);
            vec![
                (Pane::IssuesHeader, p[0]),
                (Pane::Issues, p[1]),
                (Pane::Preview, p[2]),
            ]
        }
        Page::Running | Page::Finished => running_regions(body, app),
    };
    out.push((Pane::Footer, shell[2]));
    if app.help_open {
        out.push((Pane::Help, help_rect(area)));
    }
    out
}
/// The pane under a screen position, if any. The topmost overlay wins.
pub fn pane_at(area: Rect, app: &App, column: u16, row: u16) -> Option<(Pane, Rect)> {
    regions(area, app).into_iter().rev().find(|(_, r)| {
        column >= r.x && column < r.x + r.width && row >= r.y && row < r.y + r.height
    })
}

pub fn native_dimensions(whole: Rect, expanded: bool) -> (u16, u16) {
    let central = whole.height.saturating_sub(3 + 4); // header + footer
    let width = if expanded {
        whole.width
    } else {
        whole.width.saturating_mul(42) / 100
    };
    (
        central.saturating_sub(2).max(4),
        width.saturating_sub(2).max(20),
    )
}
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < 78 || area.height < 21 {
        small_frame(frame, "Increase terminal to at least 78 columns x 21 rows.");
        return;
    }
    let shell = shell(area);
    header(frame, shell[0], app.page, app.demo);
    match app.page {
        Page::Setup => setup(frame, shell[1], app),
        Page::Browser => browser(frame, shell[1], app),
        Page::Issues => issues(frame, shell[1], app),
        Page::Running | Page::Finished => running(frame, shell[1], app),
    }
    footer(frame, shell[2], app);
    if app.help_open {
        help(frame, help_rect(area), app);
    }
}

#[cfg(test)]
mod issue_page_tests {
    use super::*;
    use crate::github::{GithubIssue, IssueBrowser, Milestone, RepoRef};
    use ratatui::{backend::TestBackend, Terminal};

    fn screen(width: u16, height: u16, app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        buffer.content().iter().map(|c| c.symbol()).collect()
    }
    fn app_with_issues() -> App {
        let mut app = App::new(crate::Args {
            project: Some(std::env::current_dir().unwrap()),
            issue: None,
            objective: None,
            demo: true,
        });
        app.page = Page::Issues;
        app.issues = IssueBrowser {
            repo: Some(RepoRef {
                owner: "acme".into(),
                name: "tool".into(),
            }),
            issues: vec![GithubIssue {
                number: 38,
                title: "GitHub Issue selector".into(),
                body: Some("## Goal\nSelect issues".into()),
                state: "OPEN".into(),
                url: "https://github.com/acme/tool/issues/38".into(),
                milestone: Some(Milestone {
                    title: "v0.5".into(),
                }),
                labels: vec![],
            }],
            status: "1 open issue(s) in acme/tool.".into(),
            ..Default::default()
        };
        app
    }
    #[test]
    fn issues_page_renders_list_and_preview_in_narrow_and_wide_terminals() {
        let app = app_with_issues();
        let narrow = screen(78, 21, &app);
        assert!(narrow.contains("#38"), "{narrow}");
        assert!(narrow.contains("[v0.5]"));
        assert!(narrow.contains("PREVIEW"));
        let wide = screen(120, 30, &app);
        assert!(wide.contains("Select issues"));
        assert!(wide.contains("Enter use"));
    }
    #[test]
    fn hostile_issue_text_never_reaches_the_screen_as_control_codes() {
        // Goes through parse_issues, the real ingestion path, rather than building the struct directly.
        let json = r#"[{"number":5,"title":"Evil \u001b]0;pwned\u0007 \u001b[2J","body":"ok\u001b[2J\u001b]52;c;eA==\u0007\u0000 end","state":"OPEN","url":"u","milestone":null,"labels":[]}]"#;
        let mut app = app_with_issues();
        app.issues.issues = crate::github::parse_issues(json).unwrap();
        for (width, height) in [(78, 21), (120, 30)] {
            let text = screen(width, height, &app);
            assert!(text.contains("#5"), "{text}");
            assert!(!text.chars().any(char::is_control), "{text}");
        }
    }
    #[test]
    fn issues_page_explains_loading_and_empty_states() {
        let mut app = app_with_issues();
        app.issues = IssueBrowser {
            loading: true,
            status: "Loading open issues from GitHub...".into(),
            ..Default::default()
        };
        assert!(screen(78, 21, &app).contains("Loading open issues"));
        app.issues.status = "gh failed: boom".into();
        assert!(screen(78, 21, &app).contains("gh failed: boom"));
    }
}

#[cfg(test)]
mod animation_tests {
    use super::*;
    #[test]
    fn beam_speed_does_not_depend_on_panel_perimeter() {
        for length in [32, 96, 340] {
            assert_eq!(beam_head(length, 0), 0);
            assert_eq!(beam_head(length, BEAM_LAP_MS / 2), length / 2);
            assert_eq!(beam_head(length, BEAM_LAP_MS), 0);
        }
    }
    #[test]
    fn glow_path_covers_each_edge_without_leaving_rect() {
        let area = Rect::new(10, 20, 30, 12);
        let len = 2 * (area.width as usize + area.height as usize) - 4;
        let mut seen = std::collections::HashSet::new();
        for i in 0..len {
            let (x, y) = perimeter_point(area, i);
            assert!(x >= area.x && x < area.x + area.width);
            assert!(y >= area.y && y < area.y + area.height);
            assert!(
                x == area.x
                    || x == area.x + area.width - 1
                    || y == area.y
                    || y == area.y + area.height - 1
            );
            seen.insert((x, y));
        }
        assert_eq!(seen.len(), len);
    }
    #[test]
    fn timeline_wording_and_clock_match_the_event_contract() {
        assert_eq!(clock(Some(1_791_594_000_000)), "01:00:00");
        assert_eq!(clock(None), "--:--:--");
        let tool = |target: &str| EventKind::ToolUsed {
            tool: "Read".into(),
            target: target.into(),
        };
        assert_eq!(event_text(&tool("main.rs")), "Read main.rs");
        assert_eq!(event_text(&tool("")), "Read");
        assert_eq!(
            event_text(&EventKind::AdvisorCompleted),
            "Consultation completed"
        );
    }
    #[test]
    fn cost_is_an_estimate_only_when_usage_cost_is_observed() {
        use crate::telemetry::{Missing, Totals};
        let observed = Totals {
            calls: 1,
            list_usd: 0.5,
            ..Default::default()
        };
        assert_eq!(cost_text(&observed, "list"), "Cost ~$0.5000 list");
        let incomplete = Totals {
            missing: Missing {
                cache_read: true,
                ..Default::default()
            },
            ..observed.clone()
        };
        assert_eq!(
            cost_text(&incomplete, "list"),
            "Cost >=$0.5000 + incomplete usage"
        );
        let unpriced = Totals {
            has_pricing_gap: true,
            ..observed.clone()
        };
        assert_eq!(cost_text(&unpriced, "list"), "Cost >=$0.5000 + unpriced");
        assert_eq!(cost_text(&Totals::default(), "list"), "Cost unknown");
    }
}
