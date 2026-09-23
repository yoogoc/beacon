//! A shell in a container, on screen.
//!
//! The bytes coming back from a container are not text. They are a terminal
//! protocol: cursor moves, colours, clears, scroll regions. Rendering them as
//! text produces garbage the moment anything interactive runs, so they go
//! through a real VT parser into a real grid, and the grid is what is drawn.
//!
//! The parser and grid are `alacritty_terminal`, which is the same one Zed and
//! Alacritty use. What is written here is the two ends: bytes in, and
//! keystrokes out.
//!
//! Key encoding is the fiddly half and it is pure: a keystroke and the
//! terminal's current application-cursor mode decide a byte sequence, with no
//! I/O involved. It is therefore all unit tested, which matters because it is
//! the half that cannot be checked by looking at a screenshot.

use std::{cell::Cell as StdCell, rc::Rc, sync::Arc};

use alacritty_terminal::{
    Term,
    event::VoidListener,
    grid::Dimensions,
    index::{Column, Line, Point},
    term::{Config, cell::Flags},
    vte::ansi::{Color as VtColor, NamedColor, Processor, Rgb},
};
use beacon_kube::{ClusterSession, Terminal as Session, TerminalEvent};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::bridge::drain_into;
use crate::theme::{BeaconTheme as _, Tone};

/// The grid's starting size. Corrected from the pane's real size on the first
/// layout; a shell that starts at 80x24 and is told the truth a frame later is
/// indistinguishable from one that started right.
const COLUMNS: usize = 100;
const ROWS: usize = 28;

/// How much scrollback the grid keeps.
const HISTORY: usize = 10_000;

/// Fixed-width metrics. A terminal grid is only a grid if every cell is the
/// same size, so the font size and the cell size are decided here rather than
/// measured per glyph.
const CELL_WIDTH: f32 = 7.8;
const CELL_HEIGHT: f32 = 17.0;
const FONT_SIZE: f32 = 13.0;

/// What the pane is doing.
enum State {
    Idle,
    Running,
    Ended,
    Failed(String),
}

pub struct TerminalView {
    session: Arc<ClusterSession>,
    namespace: String,
    pod: String,
    container: Option<String>,

    /// The grid. `VoidListener` because the events it would emit -- bell,
    /// title changes, clipboard requests -- are things this pane does not act
    /// on.
    term: Term<VoidListener>,
    parser: Processor,

    /// `None` until a shell is opened. There is no such thing as a terminal
    /// with nothing attached, so it is absent rather than a placeholder.
    shell: Option<Session>,
    state: State,
    focus: FocusHandle,
    /// The pane size measured during the last prepaint, applied at the start
    /// of the next render.
    ///
    /// Not applied in the prepaint itself: the entity is leased while its own
    /// element tree is being laid out, so an update from there is dropped on
    /// the floor -- silently, which is how the grid stayed at its starting
    /// width while looking like it should have resized.
    measured: Rc<StdCell<Option<(usize, usize)>>>,

    _output: Option<Task<()>>,
}

/// The size the grid was told to be, so a resize is only sent when it changed.
#[derive(Clone, Copy)]
struct Size {
    columns: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

impl TerminalView {
    pub fn new(
        session: Arc<ClusterSession>,
        namespace: String,
        pod: String,
        container: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let size = Size {
            columns: COLUMNS,
            rows: ROWS,
        };
        let config = Config {
            scrolling_history: HISTORY,
            ..Default::default()
        };

        let this = Self {
            session,
            namespace,
            pod,
            container,
            term: Term::new(config, &size, VoidListener),
            parser: Processor::new(),
            shell: None,
            state: State::Idle,
            focus: cx.focus_handle(),
            measured: Rc::new(StdCell::new(None)),
            _output: None,
        };
        this.focus.focus(window, cx);
        this
    }

    /// Opens a shell.
    ///
    /// The shell is chosen by trying the list in [`beacon_kube::terminal::SHELLS`]
    /// -- a container may have bash, or only sh, or neither.
    pub fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.term = Term::new(
            Config {
                scrolling_history: HISTORY,
                ..Default::default()
            },
            &Size {
                columns: COLUMNS,
                rows: ROWS,
            },
            VoidListener,
        );
        self.parser = Processor::new();
        self.state = State::Running;

        // `exec` runs one command, so falling back between shells is done in
        // the command itself rather than by reconnecting three times.
        let command = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exec /bin/bash || exec /bin/sh".to_string(),
        ];

        let handle = crate::bridge::Bridge::global(cx).handle();
        let (shell, output) = beacon_kube::terminal::attach(
            self.session.client().clone(),
            &handle,
            self.namespace.clone(),
            self.pod.clone(),
            self.container.clone(),
            command,
        );
        self.shell = Some(shell);

        self._output = Some(drain_into(
            cx,
            output,
            |view, event, _window, cx| {
                match event {
                    TerminalEvent::Output(bytes) => view.feed(&bytes),
                    TerminalEvent::Closed => view.state = State::Ended,
                    TerminalEvent::Failed(error) => view.state = State::Failed(error),
                }
                cx.notify();
            },
            window,
        ));

        self.focus.focus(window, cx);

        // Send the size again once the shell is up. The first one goes out as
        // soon as the pane is measured, which is before `exec` has finished
        // creating the process -- it reaches the API server, but there is no
        // TTY yet for it to apply to, so the shell starts at the default
        // eighty columns and wraps in the wrong place.
        let shell = self.shell.clone();
        let columns = self.term.columns() as u16;
        let rows = self.term.screen_lines() as u16;
        cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(400))
                .await;
            if let Some(shell) = shell {
                shell.resize(columns, rows);
            }
        })
        .detach();

        cx.notify();
    }

    /// Pushes container output through the VT parser and into the grid.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Sends a keystroke, if it means anything to a terminal.
    fn key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        let application_cursors = self
            .term
            .mode()
            .contains(alacritty_terminal::term::TermMode::APP_CURSOR);

        if let (Some(shell), Some(bytes)) =
            (self.shell.as_ref(), encode(keystroke, application_cursors))
        {
            shell.send(bytes);
            cx.notify();
        }
    }

    /// Tells the grid and the container how big the pane is.
    ///
    /// Returns whether anything changed: this runs on every prepaint, and a
    /// notify per frame would be a repaint loop.
    fn fit(&mut self, columns: usize, rows: usize) -> bool {
        let columns = columns.max(2);
        let rows = rows.max(2);
        if self.term.columns() == columns && self.term.screen_lines() == rows {
            return false;
        }

        tracing::info!(columns, rows, "terminal resized");
        self.term.resize(Size { columns, rows });
        if let Some(shell) = &self.shell {
            shell.resize(columns as u16, rows as u16);
        }
        true
    }

    /// The visible grid, as rows of coloured runs.
    fn rows(&self, cx: &App) -> Vec<Vec<Run>> {
        let grid = self.term.grid();
        let offset = grid.display_offset();
        let cursor = self.term.grid().cursor.point;

        (0..grid.screen_lines())
            .map(|row| {
                let line = Line(row as i32 - offset as i32);
                let mut runs: Vec<Run> = Vec::new();

                for column in 0..grid.columns() {
                    let cell = &grid[Point::new(line, Column(column))];
                    if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                        continue;
                    }

                    let inverse = cell.flags.contains(Flags::INVERSE);
                    let (fg, bg) = if inverse {
                        (colour(cell.bg, cx, true), Some(colour(cell.fg, cx, false)))
                    } else {
                        (
                            colour(cell.fg, cx, false),
                            match cell.bg {
                                VtColor::Named(NamedColor::Background) => None,
                                other => Some(colour(other, cx, true)),
                            },
                        )
                    };

                    let is_cursor = offset == 0 && line == cursor.line && column == cursor.column.0;
                    let bold = cell.flags.contains(Flags::BOLD);

                    match runs.last_mut() {
                        Some(last)
                            if last.fg == fg
                                && last.bg == bg
                                && last.bold == bold
                                && last.cursor == is_cursor =>
                        {
                            last.text.push(cell.c);
                        }
                        _ => runs.push(Run {
                            text: cell.c.to_string(),
                            fg,
                            bg,
                            bold,
                            cursor: is_cursor,
                        }),
                    }
                }

                runs
            })
            .collect()
    }
}

/// A stretch of cells that look the same, drawn as one element.
///
/// Without this a hundred-column row is a hundred elements; with it a shell
/// prompt is three or four.
struct Run {
    text: String,
    fg: Hsla,
    bg: Option<Hsla>,
    bold: bool,
    cursor: bool,
}

/// Maps a terminal colour onto the theme.
///
/// The sixteen named colours come from the theme so that a terminal in a light
/// window is readable; everything else is the exact colour the program asked
/// for, because a program that emits a 24-bit colour means it.
fn colour(color: VtColor, cx: &App, is_background: bool) -> Hsla {
    match color {
        VtColor::Spec(rgb) => rgb_to_hsla(rgb),
        // The first sixteen indices are the named colours, which come from
        // the theme; everything above is the fixed 256-colour palette.
        VtColor::Indexed(index) if index < 16 => {
            named_colour(named_from_index(index), cx, is_background)
        }
        VtColor::Indexed(index) => indexed_colour(index),
        VtColor::Named(named) => named_colour(named, cx, is_background),
    }
}

fn named_colour(named: NamedColor, cx: &App, is_background: bool) -> Hsla {
    let theme = cx.theme();
    match named {
        NamedColor::Background => theme.background,
        NamedColor::Foreground | NamedColor::BrightForeground => theme.foreground,
        NamedColor::Cursor => theme.foreground,
        NamedColor::Black | NamedColor::BrightBlack => {
            if is_background {
                theme.muted
            } else {
                theme.muted_foreground
            }
        }
        NamedColor::Red | NamedColor::BrightRed => theme.danger,
        NamedColor::Green | NamedColor::BrightGreen => theme.success,
        NamedColor::Yellow | NamedColor::BrightYellow => theme.warning,
        NamedColor::Blue | NamedColor::BrightBlue => theme.primary,
        NamedColor::Magenta | NamedColor::BrightMagenta => theme.tone(Tone::Progressing),
        NamedColor::Cyan | NamedColor::BrightCyan => theme.info,
        NamedColor::White | NamedColor::BrightWhite | NamedColor::DimForeground => theme.foreground,
        _ => theme.foreground,
    }
}

/// The first sixteen palette entries, in their standard order.
fn named_from_index(index: u8) -> NamedColor {
    match index {
        0 => NamedColor::Black,
        1 => NamedColor::Red,
        2 => NamedColor::Green,
        3 => NamedColor::Yellow,
        4 => NamedColor::Blue,
        5 => NamedColor::Magenta,
        6 => NamedColor::Cyan,
        7 => NamedColor::White,
        8 => NamedColor::BrightBlack,
        9 => NamedColor::BrightRed,
        10 => NamedColor::BrightGreen,
        11 => NamedColor::BrightYellow,
        12 => NamedColor::BrightBlue,
        13 => NamedColor::BrightMagenta,
        14 => NamedColor::BrightCyan,
        _ => NamedColor::BrightWhite,
    }
}

/// The 256-colour cube and greyscale ramp, by the standard formula.
fn indexed_colour(index: u8) -> Hsla {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];

    if (16..232).contains(&index) {
        let index = index - 16;
        return rgb_to_hsla(Rgb {
            r: STEPS[(index / 36) as usize],
            g: STEPS[((index % 36) / 6) as usize],
            b: STEPS[(index % 6) as usize],
        });
    }

    if index >= 232 {
        let level = 8 + (index - 232) * 10;
        return rgb_to_hsla(Rgb {
            r: level,
            g: level,
            b: level,
        });
    }

    // The first sixteen are handled by the named path; this is unreachable in
    // practice and grey is a safe answer if it ever is not.
    rgb_to_hsla(Rgb {
        r: 128,
        g: 128,
        b: 128,
    })
}

fn rgb_to_hsla(rgb: Rgb) -> Hsla {
    gpui_kit::rgb(((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32).into()
}

/// Turns a keystroke into the bytes a terminal expects.
///
/// `application_cursors` is the mode a full-screen program switches on, and it
/// changes what the arrow keys send -- `ESC O A` instead of `ESC [ A`. Getting
/// that wrong means arrow keys work in a shell and do nothing in `vim`, which
/// is the classic symptom of a hand-rolled terminal.
pub fn encode(keystroke: &Keystroke, application_cursors: bool) -> Option<Vec<u8>> {
    let modifiers = &keystroke.modifiers;
    let key = keystroke.key.as_str();

    // Control characters. `ctrl-c` is 0x03, and the rest of the letters follow
    // the same rule: the low five bits of the letter.
    if modifiers.control
        && !modifiers.alt
        && let Some(character) = key.chars().next()
        && key.chars().count() == 1
    {
        {
            let code = match character {
                'a'..='z' => character as u8 - b'a' + 1,
                '[' => 0x1b,
                '\\' => 0x1c,
                ']' => 0x1d,
                ' ' | '@' => 0x00,
                _ => return None,
            };
            return Some(vec![code]);
        }
    }

    let cursor = |letter: u8| {
        let introducer = if application_cursors { b'O' } else { b'[' };
        Some(vec![0x1b, introducer, letter])
    };

    let bytes = match key {
        "enter" => vec![b'\r'],
        "tab" => vec![b'\t'],
        "backspace" => vec![0x7f],
        "escape" => vec![0x1b],
        "up" => return cursor(b'A'),
        "down" => return cursor(b'B'),
        "right" => return cursor(b'C'),
        "left" => return cursor(b'D'),
        "home" => return cursor(b'H'),
        "end" => return cursor(b'F'),
        "delete" => b"\x1b[3~".to_vec(),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        "space" => vec![b' '],
        _ => {
            // Anything the platform already turned into text -- including
            // everything an input method produced.
            let text = keystroke.key_char.as_deref().unwrap_or(key);
            if text.chars().count() != 1 && keystroke.key_char.is_none() {
                return None;
            }
            let mut bytes = text.as_bytes().to_vec();
            // Alt is the meta prefix: ESC then the character.
            if modifiers.alt {
                bytes.insert(0, 0x1b);
            }
            bytes
        }
    };

    Some(bytes)
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Apply whatever the last prepaint measured. One more frame at the old
        // size is imperceptible; getting the width wrong is not.
        if let Some((columns, rows)) = self.measured.take()
            && self.fit(columns, rows)
        {
            cx.notify();
        }

        if matches!(self.state, State::Idle) {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("An interactive shell in this container."),
                )
                .child(
                    Button::new("start-shell")
                        .primary()
                        .label("Open a shell")
                        .on_click(cx.listener(|view, _, window, cx| view.start(window, cx))),
                )
                .into_any_element();
        }

        let rows = self.rows(cx);
        let status = match &self.state {
            State::Failed(error) => Some((Tone::Critical, error.clone())),
            State::Ended => Some((Tone::Unknown, "The shell exited.".to_string())),
            _ => None,
        };

        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .key_context("BeaconTerminal")
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                view.key(&event.keystroke, cx);
                cx.stop_propagation();
            }))
            .child(
                div()
                    .id("terminal-grid")
                    .relative()
                    .flex_1()
                    .overflow_hidden()
                    .p_2()
                    .bg(cx.theme().background)
                    // A zero-size probe that reports the pane's real size, so
                    // the grid can be told how wide it is. Without it the
                    // shell wraps at whatever the grid was created with and
                    // the wrap lands in the middle of a visible line.
                    .child({
                        let view = cx.entity().downgrade();
                        canvas(
                            move |bounds, _, cx| {
                                let columns = (f32::from(bounds.size.width) / CELL_WIDTH) as usize;
                                let rows = (f32::from(bounds.size.height) / CELL_HEIGHT) as usize;
                                let _ = view.update(cx, |view, cx| {
                                    if view.fit(columns, rows) {
                                        cx.notify();
                                    }
                                });
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full()
                    })
                    .child(
                        v_flex()
                            .font_family("monospace")
                            .text_size(px(FONT_SIZE))
                            .line_height(px(CELL_HEIGHT))
                            .children(rows.into_iter().map(|runs| {
                                h_flex().children(runs.into_iter().map(|run| {
                                    div()
                                        .text_color(run.fg)
                                        .when_some(run.bg, |this, bg| this.bg(bg))
                                        .when(run.bold, |this| this.font_weight(FontWeight::BOLD))
                                        .when(run.cursor, |this| {
                                            this.bg(cx.theme().foreground)
                                                .text_color(cx.theme().background)
                                        })
                                        .child(run.text)
                                }))
                            })),
                    ),
            )
            .children(status.map(|(tone, message)| {
                div()
                    .w_full()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_color(cx.theme().tone(tone))
                    .child(message)
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{CELL_HEIGHT, CELL_WIDTH, encode, indexed_colour};
    use gpui_kit::Keystroke;

    fn press(keystroke: &str) -> Option<Vec<u8>> {
        encode(&Keystroke::parse(keystroke).expect("a keystroke"), false)
    }

    fn press_in_app_mode(keystroke: &str) -> Option<Vec<u8>> {
        encode(&Keystroke::parse(keystroke).expect("a keystroke"), true)
    }

    #[test]
    fn ordinary_keys_are_their_own_bytes() {
        assert_eq!(press("a"), Some(b"a".to_vec()));
        assert_eq!(press("enter"), Some(b"\r".to_vec()));
        assert_eq!(press("tab"), Some(b"\t".to_vec()));
        assert_eq!(press("space"), Some(b" ".to_vec()));
    }

    /// A terminal's backspace is DEL, not BS. Sending 0x08 gives you a shell
    /// that moves the cursor left instead of deleting.
    #[test]
    fn backspace_is_del() {
        assert_eq!(press("backspace"), Some(vec![0x7f]));
    }

    /// The thing people press when something is stuck.
    #[test]
    fn control_letters_are_control_codes() {
        assert_eq!(press("ctrl-c"), Some(vec![0x03]));
        assert_eq!(press("ctrl-d"), Some(vec![0x04]));
        assert_eq!(press("ctrl-a"), Some(vec![0x01]));
        assert_eq!(press("ctrl-z"), Some(vec![0x1a]));
    }

    /// Arrows send a different sequence once a full-screen program turns on
    /// application cursor mode. Ignoring that gives arrow keys that work in a
    /// shell and do nothing in `vim`.
    #[test]
    fn arrows_follow_the_cursor_mode() {
        assert_eq!(press("up"), Some(b"\x1b[A".to_vec()));
        assert_eq!(press("down"), Some(b"\x1b[B".to_vec()));
        assert_eq!(press("right"), Some(b"\x1b[C".to_vec()));
        assert_eq!(press("left"), Some(b"\x1b[D".to_vec()));

        assert_eq!(press_in_app_mode("up"), Some(b"\x1bOA".to_vec()));
        assert_eq!(press_in_app_mode("left"), Some(b"\x1bOD".to_vec()));
    }

    #[test]
    fn navigation_keys_send_their_sequences() {
        assert_eq!(press("delete"), Some(b"\x1b[3~".to_vec()));
        assert_eq!(press("pageup"), Some(b"\x1b[5~".to_vec()));
        assert_eq!(press("pagedown"), Some(b"\x1b[6~".to_vec()));
        assert_eq!(press("escape"), Some(vec![0x1b]));
    }

    /// Alt is the meta prefix: `alt-b` is escape then `b`, which is how
    /// readline's word motions are reached.
    #[test]
    fn alt_prefixes_with_escape() {
        assert_eq!(press("alt-b"), Some(vec![0x1b, b'b']));
    }

    /// A modifier on its own is not a keystroke to send.
    #[test]
    fn keys_with_no_terminal_meaning_send_nothing() {
        assert_eq!(press("ctrl-1"), None);
        assert_eq!(press("f13"), None);
    }

    /// The 256-colour cube and the greyscale ramp, at their known anchors.
    #[test]
    fn indexed_colours_follow_the_standard_cube() {
        // 16 is the bottom of the cube: pure black.
        let black = indexed_colour(16);
        assert!(black.l < 0.01, "{black:?}");

        // 231 is the top: pure white.
        let white = indexed_colour(231);
        assert!(white.l > 0.99, "{white:?}");

        // The greyscale ramp runs from near-black to near-white.
        assert!(indexed_colour(232).l < indexed_colour(255).l);
    }

    /// The pane measures itself in cells, so a wrong cell size is a wrong
    /// column count and a shell that wraps in the wrong place. This pins the
    /// ratio a monospace face at this size actually has.
    #[test]
    fn a_cell_is_taller_than_it_is_wide() {
        let ratio = CELL_HEIGHT / CELL_WIDTH;
        assert!((1.9..2.4).contains(&ratio), "ratio {ratio}");
    }
}
