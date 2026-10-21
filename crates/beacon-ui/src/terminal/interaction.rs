//! Selection, scrollback, search, and remote mouse reporting.
use super::*;
use alacritty_terminal::{
    grid::Scroll,
    index::{Direction, Side},
    selection::{Selection, SelectionType},
    term::search::{Match, RegexIter, RegexSearch},
};

#[derive(Default)]
pub(super) struct SearchState {
    pub open: bool,
    pub query: String,
    pub dirty: bool,
    pub matches: Vec<Match>,
    pub current: Option<usize>,
}

#[derive(Clone, Copy)]
pub(super) struct Metrics {
    pub width: f32,
    pub height: f32,
}
impl Default for Metrics {
    fn default() -> Self {
        Self {
            width: 7.8,
            height: 19.,
        }
    }
}

impl TerminalView {
    pub(super) fn send_input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if bytes.is_empty() || !matches!(self.state, State::Running) {
            return;
        }
        if let Some(shell) = &self.shell {
            shell.send(bytes);
            self.term.selection = None;
            self.term.scroll_display(Scroll::Bottom);
            cx.notify();
        }
    }

    pub(super) fn copy_selection(&self, cx: &mut Context<Self>) {
        if let Some(text) = self
            .term
            .selection_to_string()
            .filter(|text| !text.is_empty())
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub(super) fn scroll(&mut self, scroll: Scroll, cx: &mut Context<Self>) {
        self.term.scroll_display(scroll);
        cx.notify();
    }

    pub(super) fn refresh_search(&mut self) {
        if !self.search.open || !self.search.dirty {
            return;
        }
        self.search.dirty = false;
        let previous = self
            .search
            .current
            .and_then(|index| self.search.matches.get(index))
            .cloned();
        self.search.matches.clear();
        if !self.search.query.is_empty()
            && let Ok(mut regex) = RegexSearch::new(&regex::escape(&self.search.query))
        {
            self.search.matches = RegexIter::new(
                Point::new(self.term.topmost_line(), Column(0)),
                Point::new(self.term.bottommost_line(), self.term.last_column()),
                Direction::Right,
                &self.term,
                &mut regex,
            )
            .collect();
        }
        self.search.current = previous
            .as_ref()
            .and_then(|previous| self.search.matches.iter().position(|item| item == previous));
        if self.search.current.is_none() && !self.search.matches.is_empty() {
            let top = Line(-(self.term.grid().display_offset() as i32));
            let index = self
                .search
                .matches
                .iter()
                .position(|item| item.start().line >= top)
                .unwrap_or(0);
            self.search.current = Some(index);
            if previous.is_none() {
                self.term
                    .scroll_to_point(*self.search.matches[index].start());
            }
        }
    }

    pub(super) fn find(&mut self, backwards: bool, cx: &mut Context<Self>) {
        self.refresh_search();
        let count = self.search.matches.len();
        if count == 0 {
            return;
        }
        let index = self.search.current.unwrap_or(0);
        let next = if backwards {
            (index + count - 1) % count
        } else {
            (index + 1) % count
        };
        self.search.current = Some(next);
        self.term
            .scroll_to_point(*self.search.matches[next].start());
        cx.notify();
    }

    pub(super) fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.open = true;
        self.search.dirty = true;
        self.search_input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub(super) fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.open = false;
        self.focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn zoom(&mut self, change: f32, cx: &mut Context<Self>) {
        let base = cx.theme().mono_font_size.as_f32();
        let current = (base + self.font_zoom).clamp(8., 32.);
        self.font_zoom = (current + change).clamp(8., 32.) - base;
        cx.notify();
    }

    /// Returns a buffer coordinate (history uses negative lines) and cell side.
    pub(super) fn cell_at(&self, position: gpui_kit::Point<Pixels>) -> (Point, Side) {
        let bounds = self.grid_bounds.get();
        let x = ((position.x - bounds.origin.x).as_f32() / self.metrics.width).max(0.);
        let y = ((position.y - bounds.origin.y).as_f32() / self.metrics.height).max(0.);
        let column = (x as usize).min(self.term.columns() - 1);
        let row = (y as usize).min(self.term.screen_lines() - 1);
        (
            Point::new(
                Line(row as i32 - self.term.grid().display_offset() as i32),
                Column(column),
            ),
            if x.fract() < 0.5 {
                Side::Left
            } else {
                Side::Right
            },
        )
    }

    pub(super) fn mouse_mode(&self, modifiers: Modifiers) -> bool {
        !modifiers.shift
            && self.term.grid().display_offset() == 0
            && self.term.mode().intersects(TermMode::MOUSE_MODE)
    }

    fn report_mouse(
        &self,
        button: u8,
        position: gpui_kit::Point<Pixels>,
        modifiers: Modifiers,
        released: bool,
    ) {
        let (point, _) = self.cell_at(position);
        if let Some(shell) = &self.shell
            && let Some(bytes) = input::mouse_bytes(
                button,
                point.column.0,
                point.line.0.max(0) as usize,
                modifiers,
                released,
                *self.term.mode(),
            )
        {
            shell.send(bytes);
        }
    }

    pub(super) fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus.focus(window, cx);
        if self.mouse_mode(event.modifiers) {
            let button = match event.button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
                _ => return,
            };
            self.mouse_button = Some(button);
            self.last_mouse = None;
            self.report_mouse(button, event.position, event.modifiers, false);
        } else if event.button == MouseButton::Left {
            let (point, side) = self.cell_at(event.position);
            let ty = match event.click_count {
                2 => SelectionType::Semantic,
                3.. => SelectionType::Lines,
                _ if event.modifiers.alt => SelectionType::Block,
                _ => SelectionType::Simple,
            };
            self.term.selection = Some(Selection::new(ty, point, side));
            self.selecting = true;
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        self.selecting = false;
        self.scrollbar_drag = None;
        self.last_mouse = None;
        if let Some(button) = self.mouse_button.take() {
            self.report_mouse(button, event.position, event.modifiers, true);
        }
        cx.notify();
    }

    pub(super) fn mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button.is_none() {
            self.selecting = false;
            self.scrollbar_drag = None;
            self.mouse_button = None;
        }
        if let Some((start_y, start_offset)) = self.scrollbar_drag {
            let history = self.term.grid().history_size();
            let track = self.grid_bounds.get().size.height.as_f32();
            let thumb =
                (track * self.term.screen_lines() as f32 / self.term.total_lines() as f32).max(20.);
            let delta =
                (event.position.y.as_f32() - start_y) / (track - thumb).max(1.) * history as f32;
            let target = (start_offset as f32 - delta)
                .round()
                .clamp(0., history as f32) as usize;
            let current = self.term.grid().display_offset();
            self.scroll(Scroll::Delta(target as i32 - current as i32), cx);
        } else if self.selecting && event.dragging() {
            let bounds = self.grid_bounds.get();
            if event.position.y < bounds.top() {
                self.term.scroll_display(Scroll::Delta(1));
            } else if event.position.y >= bounds.bottom() {
                self.term.scroll_display(Scroll::Delta(-1));
            }
            let (point, side) = self.cell_at(event.position);
            if let Some(selection) = &mut self.term.selection {
                selection.update(point, side);
            }
            cx.notify();
        } else if self.mouse_mode(event.modifiers)
            && self.grid_bounds.get().contains(&event.position)
            && (self.term.mode().contains(TermMode::MOUSE_MOTION)
                || (self.mouse_button.is_some() && self.term.mode().contains(TermMode::MOUSE_DRAG)))
        {
            let point = self.cell_at(event.position).0;
            let button = self.mouse_button.unwrap_or(3) + 32;
            if self.last_mouse != Some((button, point)) {
                self.report_mouse(button, event.position, event.modifiers, false);
                self.last_mouse = Some((button, point));
            }
        }
    }

    pub(super) fn wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let delta = match event.delta {
            ScrollDelta::Pixels(delta) => delta.y.as_f32() / self.metrics.height,
            ScrollDelta::Lines(delta) => delta.y * 3.,
        };
        self.wheel_remainder += delta;
        let lines = self.wheel_remainder.trunc() as i32;
        self.wheel_remainder -= lines as f32;
        if lines != 0 {
            if self.mouse_mode(event.modifiers) {
                for _ in 0..lines.unsigned_abs().min(100) {
                    self.report_mouse(
                        if lines > 0 { 64 } else { 65 },
                        event.position,
                        event.modifiers,
                        false,
                    );
                }
            } else if !event.modifiers.shift
                && self
                    .term
                    .mode()
                    .contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
            {
                let key = Keystroke::parse(if lines > 0 { "up" } else { "down" }).unwrap();
                for _ in 0..lines.unsigned_abs().min(100) {
                    self.key(&key, cx);
                }
            } else {
                self.scroll(Scroll::Delta(lines), cx);
            }
        }
        cx.stop_propagation();
    }
}
