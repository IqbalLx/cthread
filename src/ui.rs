//! Rendering. Colors come from the terminal's 16-color palette.

use crate::app::{App, Focus, Modal, PullPhase};
use crate::pane::Pane;
use crate::policy::{self, Status};
use crate::text;
use chrono::{DateTime, Local, Utc};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use std::time::Instant;

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;

fn border_style(focused: bool) -> Style {
    if focused { Style::default().fg(ACCENT) } else { Style::default().fg(DIM) }
}

fn dim() -> Style {
    Style::default().fg(DIM)
}

pub fn draw(f: &mut Frame, app: &mut App) {
    app.layout = app.compute_layout();
    let l = app.layout.clone();
    let area = f.area();
    if l.too_small {
        let msg = Paragraph::new(vec![
            Line::from("terminal too small"),
            Line::styled(format!("need at least {}x{}", crate::layout::MIN_W, crate::layout::MIN_H), dim()),
        ])
        .alignment(ratatui::layout::Alignment::Center);
        let y = area.height / 2;
        f.render_widget(msg, Rect { y: y.saturating_sub(1), height: 2.min(area.height), ..area });
        return;
    }

    if let Some(s) = l.sidebar {
        let collapsed = l.narrow;
        draw_sidebar(f, app, s, collapsed);
    }
    if let Some(m) = l.main {
        draw_main(f, app, m);
    }
    if let Some(t) = l.thread {
        draw_thread(f, app, t);
    }
    if let Some(o) = l.overlay {
        f.render_widget(Clear, o);
        draw_sidebar(f, app, o, false);
    }
    draw_status(f, app, l.status);

    let mut cursor: Option<Position> = None;
    if app.modal.is_some() {
        cursor = draw_modal(f, app, area);
    } else if let Some(p) = app.focused_pane() {
        let pty = if app.focus == Focus::Thread { l.thread_pty } else { l.main_pty };
        let parser = p.parser();
        let s = parser.screen();
        if !s.hide_cursor() && s.scrollback() == 0 && p.got_output() {
            let (r, c) = s.cursor_position();
            if r < pty.height && c < pty.width {
                cursor = Some(Position { x: pty.x + c, y: pty.y + r });
            }
        }
    }
    if let Some(c) = cursor {
        f.set_cursor_position(c);
    }
}

// ------------------------------------------------------------------ sidebar

fn status_style(s: Status) -> Style {
    match s {
        Status::NeedsYou => Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        Status::Working => Style::default().fg(ACCENT),
        Status::Unread => Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
        _ => dim(),
    }
}

fn draw_sidebar(f: &mut Frame, app: &App, r: Rect, collapsed: bool) {
    let focused = app.focus == Focus::Sidebar;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(border_style(focused))
        .title(if collapsed { Line::from("") } else { Line::styled(" THREADS ", dim()) });
    let inner = block.inner(r);
    f.render_widget(block, r);
    if inner.height < 3 || inner.width == 0 {
        return;
    }
    let buf = f.buffer_mut();
    let w = inner.width as usize;
    let open = app.open_thread();
    let sel = if focused { Some(app.sidebar_sel) } else { None };

    // main row
    let ms = app.main_status();
    let main_open = app.focus == Focus::Main || open.is_none();
    if collapsed {
        put(buf, inner.x, inner.y, "≡", status_style(ms).add_modifier(Modifier::BOLD), 1);
    } else {
        let mut spans = vec![
            Span::styled(if main_open && app.focus != Focus::Sidebar && open.is_none() { "▌" } else { " " }, Style::default().fg(ACCENT)),
            Span::styled(glyph_or(ms, app.frame, "≡"), status_style(ms).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
            Span::styled("main", Style::default().add_modifier(Modifier::BOLD)),
        ];
        if sel == Some(0) {
            spans.iter_mut().for_each(|s| s.style = s.style.add_modifier(Modifier::REVERSED));
        }
        line(buf, inner.x, inner.y, Line::from(spans), w);
    }
    put(buf, inner.x, inner.y + 1, &"─".repeat(w), dim(), w);

    let rows_avail = inner.height.saturating_sub(if collapsed { 2 } else { 3 }) as usize;
    for (i, t) in app.visible_threads().iter().enumerate().take(rows_avail) {
        let y = inner.y + 2 + i as u16;
        let st = app.thread_status(t);
        let is_open = open == Some(t.id.as_str());
        if collapsed {
            let g = if st == Status::Idle { "·" } else { policy::glyph(st, app.frame) };
            let mut style = status_style(st);
            if is_open {
                style = style.fg(ACCENT).add_modifier(Modifier::BOLD);
            }
            if sel == Some(i + 1) {
                style = style.add_modifier(Modifier::REVERSED);
            }
            put(buf, inner.x, y, g, style, 1);
            continue;
        }
        let mut spans = vec![
            Span::styled(if is_open { "▌" } else { " " }, Style::default().fg(ACCENT)),
            Span::styled(policy::glyph(st, app.frame), status_style(st)),
            Span::raw(" "),
        ];
        let mut used = 3;
        if t.worktree.is_some() {
            spans.push(Span::styled("⎇ ", Style::default().fg(Color::Magenta)));
            used += 2;
        }
        let title_style = if st == Status::Discarded {
            dim()
        } else if st == Status::Unread || is_open {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        spans.push(Span::styled(text::truncate(&t.title, w.saturating_sub(used)), title_style));
        if sel == Some(i + 1) {
            spans.iter_mut().for_each(|s| s.style = s.style.add_modifier(Modifier::REVERSED));
        }
        line(buf, inner.x, y, Line::from(spans), w);
    }

    if !collapsed {
        let fy = inner.y + inner.height - 1;
        let key = format!("{}n", app.prefix.prefix.label());
        let label = "+ New thread";
        let gap = w.saturating_sub(text::width(label) + text::width(&key) + 1).max(1);
        let s = format!(" {label}{}{key}", " ".repeat(gap.saturating_sub(1)));
        put(buf, inner.x, fy, &text::truncate(&s, w), dim(), w);
    }
}

fn glyph_or(s: Status, frame: usize, idle: &'static str) -> &'static str {
    match s {
        Status::Idle | Status::Discarded => idle,
        _ => policy::glyph(s, frame),
    }
}

fn put(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style, w: usize) {
    buf.set_stringn(x, y, s, w, style);
}

fn line(buf: &mut Buffer, x: u16, y: u16, l: Line, w: usize) {
    buf.set_line(x, y, &l, w as u16);
}

// -------------------------------------------------------------------- panes

fn pane_title(app: &App, s: Status, name: &str) -> Line<'static> {
    let g = glyph_or(s, app.frame, "");
    let mut spans = vec![Span::raw(" ")];
    if !g.is_empty() {
        spans.push(Span::styled(g.to_string(), status_style(s)));
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled(name.to_string(), Style::default().add_modifier(Modifier::BOLD)));
    spans.push(Span::raw(" "));
    Line::from(spans)
}

fn draw_main(f: &mut Frame, app: &App, r: Rect) {
    let focused = app.focus == Focus::Main;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(if focused { BorderType::Thick } else { BorderType::Plain })
        .border_style(border_style(focused))
        .title(pane_title(app, app.main_status(), "main"));
    let inner = app.layout.main_pty;
    f.render_widget(block, r);
    if let Some(p) = &app.main.pane {
        draw_pane(f.buffer_mut(), p, inner, app.main.placeholder);
    }
    if app.main_exited {
        let msg = " main exited — press Enter to restart ";
        let w = text::width(msg) as u16;
        let x = inner.x + inner.width.saturating_sub(w) / 2;
        let y = inner.y + inner.height / 2;
        let rr = Rect { x, y, width: w.min(inner.width), height: 1 };
        f.render_widget(Clear, rr);
        put(f.buffer_mut(), x, y, msg, Style::default().fg(Color::Yellow).add_modifier(Modifier::REVERSED), rr.width as usize);
    }
}

pub fn fork_time(t: DateTime<Utc>) -> String {
    let local = t.with_timezone(&Local);
    let today = Local::now().date_naive();
    if local.date_naive() == today {
        format!("today {}", local.format("%H:%M"))
    } else if Some(local.date_naive()) == today.pred_opt() {
        format!("yesterday {}", local.format("%H:%M"))
    } else {
        local.format("%b %-d %H:%M").to_string()
    }
}

fn draw_thread(f: &mut Frame, app: &App, r: Rect) {
    let Some(id) = app.open_thread() else { return };
    let Some(t) = app.store.state.thread(id) else { return };
    let focused = app.focus == Focus::Thread;
    let st = app.thread_status(t);
    let avail = r.width.saturating_sub(8) as usize;
    let title = format!("# {}", text::truncate(&t.title, avail.saturating_sub(2)));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(if focused { BorderType::Thick } else { BorderType::Plain })
        .border_style(border_style(focused))
        .title(pane_title(app, st, &title))
        .title(Line::from(Span::styled(" ✕ ", Style::default())).right_aligned());
    let inner = block.inner(r);
    f.render_widget(block, r);
    let mut sub = format!("forked from main · {}", fork_time(t.created_at));
    if let Some(wt) = &t.worktree {
        sub.push_str(&format!(" · ⎇ {}", wt.branch));
    }
    put(f.buffer_mut(), inner.x, inner.y, &text::truncate(&sub, inner.width as usize), dim(), inner.width as usize);
    let pty = app.layout.thread_pty;
    match app.threads.get(id).and_then(|s| s.pane.as_ref().map(|p| (p, s.placeholder))) {
        Some((p, ph)) => draw_pane(f.buffer_mut(), p, pty, ph),
        None => center(f.buffer_mut(), pty, "◌ discarded — restoring…", dim()),
    }
}

fn center(buf: &mut Buffer, r: Rect, s: &str, style: Style) {
    let w = text::width(s) as u16;
    let x = r.x + r.width.saturating_sub(w) / 2;
    put(buf, x, r.y + r.height / 2, s, style, r.width as usize);
}

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => match i {
            0 => Color::Black,
            1 => Color::Red,
            2 => Color::Green,
            3 => Color::Yellow,
            4 => Color::Blue,
            5 => Color::Magenta,
            6 => Color::Cyan,
            7 => Color::Gray,
            8 => Color::DarkGray,
            9 => Color::LightRed,
            10 => Color::LightGreen,
            11 => Color::LightYellow,
            12 => Color::LightBlue,
            13 => Color::LightMagenta,
            14 => Color::LightCyan,
            15 => Color::White,
            i => Color::Indexed(i),
        },
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Copy a vt100 screen into the buffer.
pub fn draw_pane(buf: &mut Buffer, p: &Pane, area: Rect, placeholder: &str) {
    if !p.got_output() {
        center(buf, area, placeholder, dim());
        return;
    }
    let parser = p.parser();
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    let rows = rows.get().min(area.height);
    let cols = cols.get().min(area.width);
    for row in 0..rows {
        for col in 0..cols {
            let Some(cell) = screen.cell(row, col) else { continue };
            if cell.is_wide_continuation() {
                continue;
            }
            if cell.is_wide() && col + 1 >= cols {
                continue; // a wide char that doesn't fit
            }
            let mut style = Style::default().fg(color(cell.fgcolor())).bg(color(cell.bgcolor()));
            let mut m = Modifier::empty();
            if cell.bold() {
                m |= Modifier::BOLD;
            }
            if cell.dim() {
                m |= Modifier::DIM;
            }
            if cell.italic() {
                m |= Modifier::ITALIC;
            }
            if cell.underline() {
                m |= Modifier::UNDERLINED;
            }
            if cell.inverse() {
                m |= Modifier::REVERSED;
            }
            style = style.add_modifier(m);
            let sym = cell.contents();
            if let Some(bc) = buf.cell_mut((area.x + col, area.y + row)) {
                bc.set_symbol(if sym.is_empty() { " " } else { sym });
                bc.set_style(style);
            }
        }
    }
    let sb = screen.scrollback();
    if sb > 0 {
        let s = format!(" ↑ {sb} lines · type to return ");
        let w = text::width(&s) as u16;
        put(buf, area.x + area.width.saturating_sub(w), area.y, &s, Style::default().fg(Color::Black).bg(Color::Yellow), w as usize);
    }
}

// ------------------------------------------------------------------- status

pub fn hints(app: &App) -> String {
    let p = app.prefix.prefix.label();
    format!("{p}n new  {p}p pull  {p}x close  {p}h/l focus  {p}j/k next/prev  {p}? help")
}

fn draw_status(f: &mut Frame, app: &App, r: Rect) {
    let now = Instant::now();
    let (s, style) = if app.prefix.hint_visible(now) {
        (format!("{}…", app.prefix.prefix.label()), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
    } else if let Some(m) = app.status_text(now) {
        (m.to_string(), Style::default().fg(Color::Yellow))
    } else {
        (hints(app), dim())
    };
    put(f.buffer_mut(), r.x + 1, r.y, &text::truncate(&s, r.width.saturating_sub(1) as usize), style, r.width as usize);
}

// ------------------------------------------------------------------- modals

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn modal_block(title: String) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(Line::from(Span::styled(format!(" {title} "), Style::default().add_modifier(Modifier::BOLD))))
}

fn key_hint(pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![];
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(k.to_string(), Style::default().add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(format!(" {v}"), dim()));
    }
    Line::from(spans)
}

/// Returns the cursor position for input modals.
fn draw_modal(f: &mut Frame, app: &App, area: Rect) -> Option<Position> {
    let modal = app.modal.as_ref()?;
    match modal {
        Modal::NewThread { input, worktree, git_ok, dirty, hint } => {
            let h = 7 + (*git_ok as u16) + (*git_ok && *dirty) as u16;
            let r = centered(area, 60, h);
            f.render_widget(Clear, r);
            let b = modal_block("New thread".into());
            let inner = b.inner(r);
            f.render_widget(b, r);
            let mut lines = vec![Line::styled("Forks main as it is right now.", dim()), Line::raw("")];
            let field_w = inner.width.saturating_sub(10) as usize;
            let (shown, cur) = scroll_field(&input.text, input.cursor, field_w);
            lines.push(Line::from(vec![Span::styled("Title     ", Style::default().add_modifier(Modifier::BOLD)), Span::raw(shown)]));
            if *git_ok {
                lines.push(Line::raw(format!("[{}] Isolate in a git worktree", if *worktree { "x" } else { " " })));
                if *dirty {
                    lines.push(Line::styled(
                        "! Uncommitted changes in main won't be in the worktree",
                        Style::default().fg(Color::Yellow),
                    ));
                }
            }
            match hint {
                Some(hn) => lines.push(Line::styled(hn.clone(), dim().add_modifier(Modifier::ITALIC))),
                None if input.text.trim().is_empty() => lines.push(Line::styled("Type a title to create the thread", dim().add_modifier(Modifier::ITALIC))),
                None => lines.push(Line::raw("")),
            }
            let mut keys = vec![("Enter", "create")];
            if *git_ok {
                keys.push(("Tab", "toggle"));
            }
            keys.push(("Esc", "cancel"));
            lines.push(key_hint(&keys));
            f.render_widget(Paragraph::new(lines), inner);
            Some(Position { x: inner.x + 10 + cur as u16, y: inner.y + 2 })
        }
        Modal::Rename { input, hint, .. } => {
            let r = centered(area, 60, 6);
            f.render_widget(Clear, r);
            let b = modal_block("Rename thread".into());
            let inner = b.inner(r);
            f.render_widget(b, r);
            let field_w = inner.width.saturating_sub(10) as usize;
            let (shown, cur) = scroll_field(&input.text, input.cursor, field_w);
            let lines = vec![
                Line::from(vec![Span::styled("Title     ", Style::default().add_modifier(Modifier::BOLD)), Span::raw(shown)]),
                Line::styled(hint.clone().unwrap_or_default(), dim().add_modifier(Modifier::ITALIC)),
                Line::raw(""),
                key_hint(&[("Enter", "save"), ("Esc", "cancel")]),
            ];
            f.render_widget(Paragraph::new(lines), inner);
            Some(Position { x: inner.x + 10 + cur as u16, y: inner.y })
        }
        Modal::Pull { title, phase, text: ta, editing, .. } => {
            let w = 76.min(area.width.saturating_sub(4));
            let h = (area.height * 2 / 3).max(10);
            let r = centered(area, w, h);
            f.render_widget(Clear, r);
            let b = modal_block(format!("Pull \"{}\" into main", text::truncate(title, (w as usize).saturating_sub(20))));
            let inner = b.inner(r);
            f.render_widget(b, r);
            let body = Rect { height: inner.height.saturating_sub(2), ..inner };
            let foot = Rect { y: inner.y + inner.height - 1, height: 1, ..inner };
            let mut cursor = None;
            match phase {
                PullPhase::Loading => {
                    let s = format!("{} Summarizing…", policy::SPINNER[app.frame % 4]);
                    f.render_widget(Paragraph::new(Line::styled(s, Style::default().fg(ACCENT))), body);
                    f.render_widget(Paragraph::new(key_hint(&[("Esc", "cancel")])), foot);
                }
                PullPhase::Failed(e) => {
                    f.render_widget(
                        Paragraph::new(vec![Line::styled("Summary failed:", Style::default().fg(Color::Red)), Line::raw(e.clone())])
                            .wrap(Wrap { trim: false }),
                        body,
                    );
                    f.render_widget(Paragraph::new(key_hint(&[("r", "retry"), ("Esc", "cancel")])), foot);
                }
                PullPhase::Ready => {
                    let (row, col) = ta.cursor_rowcol();
                    let scroll = if *editing { (row as u16).saturating_sub(body.height.saturating_sub(1)) } else { 0 };
                    let style = Style::default();
                    let para = Paragraph::new(ta.text.clone()).style(style);
                    // Wrap in preview; editing keeps lines unwrapped so the cursor maps 1:1.
                    let para = if *editing { para.scroll((scroll, 0)) } else { para.wrap(Wrap { trim: false }) };
                    f.render_widget(para, body);
                    if *editing {
                        f.render_widget(Paragraph::new(key_hint(&[("Esc", "done editing")])), foot);
                        let y = body.y + row as u16 - scroll;
                        if (col as u16) < body.width && y < body.y + body.height {
                            cursor = Some(Position { x: body.x + col as u16, y });
                        }
                    } else {
                        f.render_widget(
                            Paragraph::new(key_hint(&[("Enter", "insert into main"), ("e", "edit"), ("E", "$EDITOR"), ("r", "regenerate"), ("Esc", "cancel")])),
                            foot,
                        );
                    }
                }
            }
            cursor
        }
        Modal::Archived { selected } => {
            let list = app.archived_threads();
            let h = (list.len() as u16 + 5).clamp(6, area.height.saturating_sub(4));
            let r = centered(area, 56, h);
            f.render_widget(Clear, r);
            let b = modal_block("Archived threads".into());
            let inner = b.inner(r);
            f.render_widget(b, r);
            let mut lines = vec![];
            if list.is_empty() {
                lines.push(Line::styled("No archived threads.", dim()));
            }
            for (i, t) in list.iter().enumerate() {
                let mut s = Style::default();
                if i == *selected {
                    s = s.add_modifier(Modifier::REVERSED);
                }
                let wt = if t.worktree.is_some() { "⎇ " } else { "" };
                lines.push(Line::styled(text::pad(&format!(" {wt}{}", text::truncate(&t.title, inner.width as usize - 4)), inner.width as usize), s));
            }
            let body = Rect { height: inner.height.saturating_sub(2), ..inner };
            f.render_widget(Paragraph::new(lines), body);
            let foot = Rect { y: inner.y + inner.height - 1, height: 1, ..inner };
            f.render_widget(Paragraph::new(key_hint(&[("Enter", "restore"), ("D", "delete"), ("Esc", "close")])), foot);
            None
        }
        Modal::ConfirmDelete { id } => {
            let title = app.store.state.thread(id).map(|t| t.title.clone()).unwrap_or_default();
            confirm(
                f,
                area,
                "Delete thread",
                vec![
                    Line::raw(format!("Delete \"{}\"?", text::truncate(&title, 44))),
                    Line::styled("Claude Code's transcript files are kept.", dim()),
                ],
                &[("y", "delete"), ("n", "cancel")],
            );
            None
        }
        Modal::DeleteWorktree { id, confirm_dirty } => {
            let t = app.store.state.thread(id);
            let branch = t.and_then(|t| t.worktree.as_ref()).map(|w| w.branch.clone()).unwrap_or_default();
            if *confirm_dirty {
                confirm(
                    f,
                    area,
                    "Uncommitted changes",
                    vec![
                        Line::styled(format!("The worktree for {branch} has uncommitted changes."), Style::default().fg(Color::Yellow)),
                        Line::raw("Remove it anyway and lose them?"),
                    ],
                    &[("y", "remove anyway"), ("n", "cancel")],
                );
            } else {
                confirm(
                    f,
                    area,
                    "Delete worktree thread",
                    vec![Line::raw(format!("This thread has a worktree on {branch}.")), Line::raw("Keep the worktree, or remove it and its branch?")],
                    &[("k", "keep worktree"), ("r", "remove worktree + branch"), ("Esc", "cancel")],
                );
            }
            None
        }
        Modal::ConfirmQuit => {
            confirm(
                f,
                area,
                "Quit cthread",
                vec![Line::raw("A session is still working."), Line::raw("Quit and stop every session?")],
                &[("y", "quit"), ("n", "cancel")],
            );
            None
        }
        Modal::Help => {
            let p = app.prefix.prefix.label();
            let rows: Vec<(String, &str)> = vec![
                (format!("{p} n"), "New thread (forks main)"),
                (format!("{p} p"), "Pull the open thread into main"),
                (format!("{p} x"), "Close the thread pane"),
                (format!("{p} h / l"), "Focus left / right"),
                (format!("{p} j / k"), "Open next / previous thread"),
                (format!("{p} 1…9"), "Open thread by position"),
                (format!("{p} m"), "Focus main"),
                (format!("{p} r"), "Rename thread"),
                (format!("{p} J / K"), "Move thread down / up"),
                (format!("{p} a"), "Archive thread"),
                (format!("{p} A"), "Show archived threads"),
                (format!("{p} D"), "Delete thread"),
                (format!("{p} < / >"), "Shrink / grow the thread pane"),
                (format!("{p} b"), "Toggle sidebar"),
                (format!("{p} ?"), "This help"),
                (format!("{p} q"), "Quit"),
                (format!("{p} {p}"), "Send a literal prefix key"),
                ("".into(), ""),
                ("sidebar".into(), "j/k move · Enter open · r a n D J K · Esc back"),
            ];
            let h = (rows.len() as u16 + 4).min(area.height.saturating_sub(2));
            let r = centered(area, 64, h);
            f.render_widget(Clear, r);
            let b = modal_block("Keys".into());
            let inner = b.inner(r);
            f.render_widget(b, r);
            let mut lines: Vec<Line> = rows
                .into_iter()
                .map(|(k, v)| Line::from(vec![Span::styled(text::pad(&k, 14), Style::default().fg(ACCENT)), Span::raw(v)]))
                .collect();
            lines.push(Line::raw(""));
            lines.push(key_hint(&[("Esc", "close")]));
            f.render_widget(Paragraph::new(lines), inner);
            None
        }
    }
}

fn confirm(f: &mut Frame, area: Rect, title: &str, mut lines: Vec<Line<'static>>, keys: &[(&str, &str)]) {
    let r = centered(area, 60, lines.len() as u16 + 4);
    f.render_widget(Clear, r);
    let b = modal_block(title.into());
    let inner = b.inner(r);
    f.render_widget(b, r);
    lines.push(Line::raw(""));
    lines.push(key_hint(keys));
    f.render_widget(Paragraph::new(lines), inner);
}

/// Horizontally scroll a single-line field so the cursor stays visible.
/// Returns the visible text and the cursor's display column within it.
fn scroll_field(s: &str, cursor: usize, w: usize) -> (String, usize) {
    let before = text::width(&s[..cursor]);
    if text::width(s) < w || before < w {
        let shown: String = {
            let mut out = String::new();
            let mut used = 0;
            for g in unicode_segmentation::UnicodeSegmentation::graphemes(s, true) {
                let gw = text::width(g);
                if used + gw > w {
                    break;
                }
                out.push_str(g);
                used += gw;
            }
            out
        };
        return (shown, before);
    }
    // cursor beyond the field: show the tail ending at the cursor
    let mut start = 0;
    let mut width_from = before;
    for (i, g) in unicode_segmentation::UnicodeSegmentation::grapheme_indices(s, true) {
        if width_from < w {
            start = i;
            break;
        }
        width_from -= text::width(g);
    }
    (s[start..cursor].to_string(), text::width(&s[start..cursor]))
}
