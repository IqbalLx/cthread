//! Screen layout: sidebar, main pane, thread pane, status line.

use ratatui::layout::Rect;

pub const MIN_W: u16 = 60;
pub const MIN_H: u16 = 15;
pub const NARROW_SIDEBAR: u16 = 3;

#[derive(Debug, Clone, Copy)]
pub struct LayoutInput {
    pub area: Rect,
    pub sidebar_width: u16,
    pub sidebar_visible: bool,
    pub narrow_breakpoint: u16,
    pub thread_open: bool,
    /// Narrow mode: show the thread (true) or main (false).
    pub narrow_show_thread: bool,
    /// Narrow mode: sidebar expanded as an overlay.
    pub overlay: bool,
    pub split_ratio: f32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    pub too_small: bool,
    pub narrow: bool,
    /// Sidebar block (full or collapsed). None when hidden.
    pub sidebar: Option<Rect>,
    /// Expanded sidebar overlay (narrow mode only).
    pub overlay: Option<Rect>,
    /// Main pane block, if visible.
    pub main: Option<Rect>,
    /// Thread pane block, if visible.
    pub thread: Option<Rect>,
    /// PTY area of main (always computed so a hidden main keeps a sane size).
    pub main_pty: Rect,
    /// PTY area of the thread pane (below its sub-header).
    pub thread_pty: Rect,
    /// Column of the draggable divider (main's right border), wide mode.
    pub divider_x: Option<u16>,
    pub status: Rect,
    /// Width available to the two session panes.
    pub panes_area: Rect,
}

fn inner(r: Rect) -> Rect {
    Rect { x: r.x + 1, y: r.y + 1, width: r.width.saturating_sub(2), height: r.height.saturating_sub(2) }
}

/// PTY area of a thread pane block: inner minus the dim sub-header row.
pub fn thread_pty_area(block: Rect) -> Rect {
    let i = inner(block);
    Rect { y: i.y + 1, height: i.height.saturating_sub(1), ..i }
}

pub fn compute(inp: &LayoutInput) -> Layout {
    let a = inp.area;
    let mut l = Layout::default();
    if a.width < MIN_W || a.height < MIN_H {
        l.too_small = true;
        return l;
    }
    l.narrow = a.width < inp.narrow_breakpoint;
    l.status = Rect { x: a.x, y: a.y + a.height - 1, width: a.width, height: 1 };
    let body = Rect { height: a.height - 1, ..a };

    let side_w = if l.narrow {
        NARROW_SIDEBAR
    } else if inp.sidebar_visible {
        inp.sidebar_width.min(a.width / 3)
    } else {
        0
    };
    if side_w > 0 {
        l.sidebar = Some(Rect { width: side_w, ..body });
    }
    let panes = Rect { x: body.x + side_w, width: body.width - side_w, ..body };
    l.panes_area = panes;

    if l.narrow {
        if inp.overlay {
            l.overlay = Some(Rect { width: inp.sidebar_width.min(body.width), ..body });
        }
        l.main_pty = inner(panes);
        l.thread_pty = thread_pty_area(panes);
        if inp.thread_open && inp.narrow_show_thread {
            l.thread = Some(panes);
        } else {
            l.main = Some(panes);
        }
        return l;
    }

    if inp.thread_open {
        let ratio = inp.split_ratio.clamp(0.2, 0.8);
        let main_w = ((panes.width as f32) * ratio).round() as u16;
        let main_w = main_w.clamp(20, panes.width.saturating_sub(20));
        let m = Rect { width: main_w, ..panes };
        let t = Rect { x: panes.x + main_w, width: panes.width - main_w, ..panes };
        l.main = Some(m);
        l.thread = Some(t);
        l.main_pty = inner(m);
        l.thread_pty = thread_pty_area(t);
        l.divider_x = Some(m.x + m.width - 1);
    } else {
        l.main = Some(panes);
        l.main_pty = inner(panes);
        // A thread opened next would get the default split.
        let main_w = ((panes.width as f32) * inp.split_ratio.clamp(0.2, 0.8)).round() as u16;
        let t = Rect { x: panes.x + main_w, width: panes.width - main_w, ..panes };
        l.thread_pty = thread_pty_area(t);
    }
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inp(w: u16, h: u16, open: bool) -> LayoutInput {
        LayoutInput {
            area: Rect::new(0, 0, w, h),
            sidebar_width: 22,
            sidebar_visible: true,
            narrow_breakpoint: 120,
            thread_open: open,
            narrow_show_thread: true,
            overlay: false,
            split_ratio: 0.5,
        }
    }

    #[test]
    fn wide_open() {
        let l = compute(&inp(160, 40, true));
        assert!(!l.narrow);
        assert_eq!(l.sidebar.unwrap().width, 22);
        let (m, t) = (l.main.unwrap(), l.thread.unwrap());
        assert_eq!(m.x, 22);
        assert_eq!(m.width + t.width, 138);
        assert_eq!(m.width, 69);
        assert_eq!(l.status.y, 39);
        assert_eq!(l.thread_pty.height, 39 - 3);
        assert_eq!(l.divider_x, Some(22 + 69 - 1));
    }

    #[test]
    fn wide_closed_and_hidden_sidebar() {
        let mut i = inp(160, 40, false);
        let l = compute(&i);
        assert!(l.thread.is_none());
        assert_eq!(l.main.unwrap().width, 138);
        i.sidebar_visible = false;
        let l = compute(&i);
        assert!(l.sidebar.is_none());
        assert_eq!(l.main.unwrap().width, 160);
    }

    #[test]
    fn narrow() {
        let l = compute(&inp(100, 30, true));
        assert!(l.narrow);
        assert_eq!(l.sidebar.unwrap().width, 3);
        assert!(l.main.is_none());
        assert_eq!(l.thread.unwrap().width, 97);
        let mut i = inp(100, 30, true);
        i.narrow_show_thread = false;
        i.overlay = true;
        let l = compute(&i);
        assert!(l.thread.is_none());
        assert_eq!(l.overlay.unwrap().width, 22);
    }

    #[test]
    fn too_small() {
        assert!(compute(&inp(59, 30, false)).too_small);
        assert!(compute(&inp(80, 14, false)).too_small);
        assert!(!compute(&inp(60, 15, false)).too_small);
    }
}
