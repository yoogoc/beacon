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

use std::{
    cell::{Cell as StdCell, RefCell},
    rc::Rc,
    sync::Arc,
};

use alacritty_terminal::{
    Term,
    event::{Event, EventListener, WindowSize},
    grid::Dimensions,
    index::{Column, Line, Point},
    term::{Config, cell::Flags},
    vte::ansi::{Color as VtColor, NamedColor, Processor, Rgb},
};
use beacon_kube::{ClusterSession, Terminal as Session, TerminalEvent};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
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
    Connecting,
    Running,
    Ended,
    Failed(String),
}

/// VT queries (notably DuckDB/readline's cursor-position query) need a reply
/// on stdin. Dropping these events can leave a connected terminal waiting
/// forever before it prints a prompt.
#[derive(Clone, Default)]
struct Replies(Rc<RefCell<Vec<Event>>>);

impl EventListener for Replies {
    fn send_event(&self, event: Event) {
        if matches!(
            event,
            Event::PtyWrite(_) | Event::ColorRequest(..) | Event::TextAreaSizeRequest(_)
        ) {
            self.0.borrow_mut().push(event);
        }
    }
}

pub struct TerminalView {
    session: Arc<ClusterSession>,
    namespace: String,
    pod: String,
    container: Option<String>,

    term: Term<Replies>,
    replies: Replies,
    parser: Processor,

    /// `None` until a shell is opened. There is no such thing as a terminal
    /// with nothing attached, so it is absent rather than a placeholder.
    shell: Option<Session>,
    state: State,
    /// What to run. Empty means the fallback chain, which is what a container
    /// with only `sh` needs and what one with `bash` prefers. It is here
    /// because neither is always right: a distroless image has neither, a
    /// busybox one has `/bin/ash`, and sometimes the thing worth attaching to
    /// is not a shell at all.
    command: Entity<InputState>,
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
    pub(crate) fn set_command(
        &mut self,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.command
            .update(cx, |input, cx| input.set_value(command, window, cx));
    }
    pub(crate) fn is_active(&self) -> bool {
        matches!(self.state, State::Connecting | State::Running)
    }

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
        let replies = Replies::default();

        let this = Self {
            session,
            namespace,
            pod,
            container,
            term: Term::new(config, &size, replies.clone()),
            replies,
            parser: Processor::new(),
            shell: None,
            state: State::Idle,
            command: cx
                .new(|cx| InputState::new(window, cx).placeholder("bash, falling back to sh")),
            focus: cx.focus_handle(),
            measured: Rc::new(StdCell::new(None)),
            _output: None,
        };
        this.focus.focus(window, cx);
        this
    }

    /// What to exec: whatever was typed, or the fallback chain.
    ///
    /// `exec` runs one command, so the fallback is done inside the command
    /// rather than by connecting three times and seeing which one survives.
    /// Anything typed is split the way a shell splits a command line --
    /// quoting only, no expansion -- because the argv goes to the container
    /// as it is.
    fn command(&self, cx: &App) -> Vec<String> {
        let typed = self.command.read(cx).value();
        let typed = typed.trim();
        if typed.is_empty() {
            return vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                // `TERM` is the whole reason anything in here has colour.
                // Kubernetes `exec` has no way to pass an environment, and
                // with `TERM` unset a shell's rc file takes the no-colour
                // branch and `ls` gives up too -- so it is exported by the
                // command itself, before the shell it hands over to.
                "TERM=xterm-256color; export TERM; if command -v bash >/dev/null 2>&1; then exec bash; else exec /bin/sh; fi".to_string(),
            ];
        }

        let split = beacon_kube::exec::split(typed);
        // All quotes and no words. Falling back beats exec-ing nothing.
        match split.is_empty() {
            true => vec!["/bin/sh".to_string()],
            false => split,
        }
    }

    /// Opens a shell, or opens another one over the top of the last.
    ///
    /// Replacing `shell` drops the previous session, which closes its
    /// WebSocket -- so this doubles as "try a different shell", which is the
    /// whole reason the command is editable.
    pub fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let size = Size {
            columns: self.term.columns(),
            rows: self.term.screen_lines(),
        };
        self._output = None;
        self.shell = None;
        self.replies.0.borrow_mut().clear();
        self.term = Term::new(
            Config {
                scrolling_history: HISTORY,
                ..Default::default()
            },
            &size,
            self.replies.clone(),
        );
        self.parser = Processor::new();
        self.state = State::Connecting;

        let command = self.command(cx);
        tracing::info!(context = %self.session.id(), namespace = %self.namespace, pod = %self.pod, ?command, "opening a shell");

        let (shell, output) = self.session.terminal(
            self.namespace.clone(),
            self.pod.clone(),
            self.container.clone(),
            command,
        );
        self.shell = Some(shell);
        if let Some(shell) = &self.shell {
            shell.resize(size.columns as u16, size.rows as u16);
        }

        self._output = Some(drain_into(
            cx,
            output,
            |view, event, _window, cx| {
                match event {
                    TerminalEvent::Connected => view.state = State::Running,
                    TerminalEvent::Output(bytes) => view.feed(&bytes, cx),
                    TerminalEvent::Closed => {
                        view.shell = None;
                        view.state = State::Ended;
                    }
                    TerminalEvent::Failed(error) => {
                        tracing::warn!(pod = %view.pod, %error, "shell failed");
                        view.shell = None;
                        view.state = State::Failed(error);
                    }
                }
                cx.notify();
            },
            window,
        ));

        self.focus.focus(window, cx);

        cx.notify();
    }

    /// Pushes container output through the VT parser and into the grid.
    pub fn feed(&mut self, bytes: &[u8], cx: &App) {
        self.parser.advance(&mut self.term, bytes);
        for event in self.replies.0.borrow_mut().drain(..) {
            let reply = match event {
                Event::PtyWrite(reply) => reply,
                Event::ColorRequest(index, format) => {
                    let color = self.term.colors()[index].unwrap_or_else(|| {
                        let color = match index {
                            0..16 => palette(index, cx),
                            16..256 => indexed_colour(index as u8),
                            n if n == NamedColor::Background as usize => cx.theme().background,
                            _ => cx.theme().foreground,
                        }
                        .to_rgb();
                        Rgb {
                            r: (color.r * 255.).round() as u8,
                            g: (color.g * 255.).round() as u8,
                            b: (color.b * 255.).round() as u8,
                        }
                    });
                    format(color)
                }
                Event::TextAreaSizeRequest(format) => format(WindowSize {
                    num_cols: self.term.columns() as u16,
                    num_lines: self.term.screen_lines() as u16,
                    cell_width: CELL_WIDTH as u16,
                    cell_height: CELL_HEIGHT as u16,
                }),
                _ => continue,
            };
            if let Some(shell) = &self.shell {
                shell.send(reply.into_bytes());
            }
        }
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

/// The sixteen ANSI colours on a dark background, in their standard order:
/// black, red, green, yellow, blue, magenta, cyan, white, then the bright
/// eight. These are One Dark's terminal palette, which is widely used for
/// exactly this and known to be legible.
const DARK_PALETTE: [u32; 16] = [
    0x3f4451, 0xe05561, 0x8cc265, 0xd18f52, 0x4aa5f0, 0xc162de, 0x42b3c2, 0xd7dae0, 0x4f5666,
    0xff616e, 0xa5e075, 0xf0a45d, 0x4dc4ff, 0xde73ff, 0x4cd1e0, 0xe6e6e6,
];

/// The same sixteen on a light background.
///
/// The first eight are One Light. The bright eight are *darker* rather than
/// lighter, which is the opposite of what "bright" means and the only thing
/// that works here: a lighter red on a white background is a red nobody can
/// read. White and bright white become greys for the same reason -- a
/// terminal that is accurate and illegible is worse than one that is legible
/// and says so.
const LIGHT_PALETTE: [u32; 16] = [
    0x383a42, 0xe45649, 0x50a14f, 0xc18401, 0x0184bc, 0xa626a4, 0x0997b3, 0xa0a1a7, 0x696c77,
    0xc62f23, 0x3c7a3b, 0x96670a, 0x01669a, 0x7f1d7e, 0x07748a, 0x6b6d75,
];

/// One of the sixteen, for whichever background the window has.
fn palette(index: usize, cx: &App) -> Hsla {
    let palette = match cx.theme().is_dark() {
        true => DARK_PALETTE,
        false => LIGHT_PALETTE,
    };
    gpui_kit::rgb(palette[index.min(15)]).into()
}

/// Maps the named colours.
///
/// The sixteen come from the palette above rather than from the theme's own
/// tokens. Tokens were the first attempt and they are the wrong shape: the
/// theme has one danger colour, so red and bright red came out identical,
/// and `ls` painting directories in bright blue looked the same as ordinary
/// blue. Sixteen distinct colours is what a program emitting them expects.
///
/// The three that are not colours -- background, foreground, cursor -- do
/// still come from the theme, so the pane matches the window around it.
fn named_colour(named: NamedColor, cx: &App, is_background: bool) -> Hsla {
    let theme = cx.theme();
    let index = match named {
        NamedColor::Background => return theme.background,
        NamedColor::Foreground | NamedColor::BrightForeground => return theme.foreground,
        NamedColor::Cursor => return theme.foreground,
        NamedColor::DimForeground => return theme.muted_foreground,

        // A black background is the one place the palette is wrong to use:
        // on a light window it would paint a black block behind the text.
        NamedColor::Black | NamedColor::BrightBlack if is_background => return theme.muted,

        NamedColor::Black => 0,
        NamedColor::Red | NamedColor::DimRed => 1,
        NamedColor::Green | NamedColor::DimGreen => 2,
        NamedColor::Yellow | NamedColor::DimYellow => 3,
        NamedColor::Blue | NamedColor::DimBlue => 4,
        NamedColor::Magenta | NamedColor::DimMagenta => 5,
        NamedColor::Cyan | NamedColor::DimCyan => 6,
        NamedColor::White | NamedColor::DimWhite => 7,
        NamedColor::BrightBlack => 8,
        NamedColor::BrightRed => 9,
        NamedColor::BrightGreen => 10,
        NamedColor::BrightYellow => 11,
        NamedColor::BrightBlue => 12,
        NamedColor::BrightMagenta => 13,
        NamedColor::BrightCyan => 14,
        NamedColor::BrightWhite => 15,
        _ => return theme.foreground,
    };

    palette(index, cx)
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

impl TerminalView {
    /// The command box and the button that runs it.
    ///
    /// Empty means the fallback chain, so the common case stays one click and
    /// the placeholder says what that click will do.
    fn render_launcher(&self, label: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_2()
            .items_center()
            .child(div().w(px(260.)).child(Input::new(&self.command).small()))
            .child(
                Button::new("start-shell")
                    .primary()
                    .small()
                    .label(label)
                    .on_click(cx.listener(|view, _, window, cx| view.start(window, cx))),
            )
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
                .child(self.render_launcher("Open a shell", cx))
                .into_any_element();
        }

        let rows = self.rows(cx);
        let output = rows
            .iter()
            .map(|runs| {
                runs.iter()
                    .map(|run| run.text.as_str())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let status = match &self.state {
            State::Connecting => Some((
                Tone::Progressing,
                "Connecting to the container…".to_string(),
            )),
            State::Failed(error) => Some((Tone::Critical, error.clone())),
            State::Ended => Some((Tone::Unknown, "The shell exited.".to_string())),
            _ => None,
        };

        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .key_context("BeaconTerminal")
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                // Only the grid's focus sends keys to the shell. Child inputs,
                // selectable errors and menus keep their normal shortcuts.
                if !view.focus.is_focused(window) {
                    return;
                }
                view.key(&event.keystroke, cx);
                cx.stop_propagation();
            }))
            .child(
                div()
                    .id("terminal-grid")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .p_2()
                    .bg(cx.theme().background)
                    // A zero-size probe that reports the pane's real size, so
                    // the grid can be told how wide it is. Without it the
                    // shell wraps at whatever the grid was created with and
                    // the wrap lands in the middle of a visible line.
                    .child({
                        let measured = self.measured.clone();
                        let entity_id = cx.entity_id();
                        let current = (self.term.columns(), self.term.screen_lines());
                        canvas(
                            move |bounds, _, cx| {
                                let columns = (f32::from(bounds.size.width) / CELL_WIDTH) as usize;
                                let rows = (f32::from(bounds.size.height) / CELL_HEIGHT) as usize;
                                let size = (columns.max(2), rows.max(2));
                                if size != current {
                                    measured.set(Some(size));
                                    cx.notify(entity_id);
                                }
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
                    )
                    .context_menu(move |menu, _, _| {
                        menu.item(crate::copyable_text::copy_item(
                            "Copy terminal output",
                            output.clone(),
                        ))
                    }),
            )
            .children(status.map(|(tone, message)| {
                h_flex()
                    .w_full()
                    .flex_shrink_0()
                    .px_3()
                    .py_1p5()
                    .gap_3()
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().tone(tone))
                            .child(crate::copyable_text::copyable_text("shell-status", message)),
                    )
                    // "No such file or directory" is the commonest way a shell
                    // ends, and the answer to it is a different shell. Putting
                    // the box here means trying one is where the failure is,
                    // rather than somewhere else.
                    .child(self.render_launcher("Restart", cx))
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{CELL_HEIGHT, CELL_WIDTH, DARK_PALETTE, LIGHT_PALETTE, encode, indexed_colour};
    use gpui_kit::Keystroke;

    #[test]
    fn cursor_queries_produce_replies_for_interactive_programs() {
        use super::*;
        let replies = Replies::default();
        let mut term = Term::new(
            Config::default(),
            &Size {
                columns: 100,
                rows: 28,
            },
            replies.clone(),
        );
        let mut parser: Processor = Processor::new();
        // A split network frame must still be decoded as one query.
        parser.advance(&mut term, b"\x1b[2;5H\x1b[");
        parser.advance(&mut term, b"6n");
        assert!(
            matches!(replies.0.borrow().as_slice(), [Event::PtyWrite(reply)] if reply == "\x1b[2;5R")
        );
    }

    #[test]
    fn duckdb_background_color_queries_are_not_discarded() {
        use super::*;
        let replies = Replies::default();
        let mut term = Term::new(
            Config::default(),
            &Size {
                columns: 100,
                rows: 28,
            },
            replies.clone(),
        );
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x1b]11;?\x07");
        let events = replies.0.borrow();
        let [Event::ColorRequest(index, format)] = events.as_slice() else {
            panic!("no background color reply");
        };
        assert_eq!(*index, NamedColor::Background as usize);
        let reply = format(Rgb {
            r: 255,
            g: 255,
            b: 255,
        });
        assert!(reply.starts_with("\x1b]11;rgb:ffff/ffff/ffff"), "{reply:?}");
    }

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

    /// Red and bright red were the same colour when these came from the
    /// theme's tokens, and a terminal with eight colours where a program
    /// sent sixteen is what made this look nothing like a real one.
    #[test]
    fn the_bright_eight_are_not_the_ordinary_eight() {
        for index in 0..8 {
            assert_ne!(DARK_PALETTE[index], DARK_PALETTE[index + 8], "dark {index}");
            assert_ne!(
                LIGHT_PALETTE[index],
                LIGHT_PALETTE[index + 8],
                "light {index}"
            );
        }
    }

    /// On a white background "bright" cannot mean lighter, or it disappears.
    #[test]
    fn the_light_palette_stays_dark_enough_to_read() {
        for (index, colour) in LIGHT_PALETTE.iter().enumerate() {
            let (r, g, b) = (colour >> 16 & 0xff, colour >> 8 & 0xff, colour & 0xff);
            // Rec. 601 luma, the usual quick test for "is this readable on
            // white".
            let luma = (299 * r + 587 * g + 114 * b) / 1000;
            assert!(luma < 180, "{index} is too light to read: {luma}");
        }
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
