//! Keyboard, IME, and mouse bytes for the remote terminal.
use super::*;
use std::ops::Range;

pub(super) fn text_key(key: &Keystroke) -> bool {
    !key.modifiers.control
        && !key.modifiers.alt
        && !key.modifiers.platform
        && !matches!(key.key.as_str(), "enter" | "tab" | "escape" | "backspace")
        && (key.key == "space"
            || key
                .key_char
                .as_ref()
                .is_some_and(|text| text.chars().all(|ch| !ch.is_control()))
            || key.key.chars().count() == 1)
}

/// Xterm modifier encoding, shared by navigation and function keys.
fn modifier(key: &Keystroke) -> u8 {
    1 + u8::from(key.modifiers.shift)
        + 2 * u8::from(key.modifiers.alt)
        + 4 * u8::from(key.modifiers.control)
}

pub(super) fn key_bytes(key: &Keystroke, app_cursor: bool) -> Option<Vec<u8>> {
    if key.modifiers.platform {
        return None;
    }
    let code = modifier(key);
    let special = match key.key.as_str() {
        "up" => Some('A'),
        "down" => Some('B'),
        "right" => Some('C'),
        "left" => Some('D'),
        "home" => Some('H'),
        "end" => Some('F'),
        _ => None,
    };
    if let Some(final_byte) = special {
        return Some(
            if code != 1 {
                format!("\x1b[1;{code}{final_byte}")
            } else {
                format!("\x1b{}{final_byte}", if app_cursor { 'O' } else { '[' })
            }
            .into_bytes(),
        );
    }
    if key.key == "tab" && key.modifiers.shift {
        return Some(b"\x1b[Z".to_vec());
    }
    let function = match key.key.as_str() {
        "f1" => Some('P'),
        "f2" => Some('Q'),
        "f3" => Some('R'),
        "f4" => Some('S'),
        _ => None,
    };
    if let Some(final_byte) = function {
        return Some(
            if code == 1 {
                format!("\x1bO{final_byte}")
            } else {
                format!("\x1b[1;{code}{final_byte}")
            }
            .into_bytes(),
        );
    }
    let tilde = match key.key.as_str() {
        "insert" => Some(2),
        "delete" => Some(3),
        "pageup" => Some(5),
        "pagedown" => Some(6),
        "f5" => Some(15),
        "f6" => Some(17),
        "f7" => Some(18),
        "f8" => Some(19),
        "f9" => Some(20),
        "f10" => Some(21),
        "f11" => Some(23),
        "f12" => Some(24),
        _ => None,
    };
    if let Some(number) = tilde {
        return Some(
            if code == 1 {
                format!("\x1b[{number}~")
            } else {
                format!("\x1b[{number};{code}~")
            }
            .into_bytes(),
        );
    }
    if key.modifiers.control && !key.modifiers.alt && key.key.chars().count() == 1 {
        return key
            .key
            .chars()
            .next()
            .and_then(|character| match character {
                'a'..='z' => Some(vec![character as u8 - b'a' + 1]),
                '[' => Some(vec![0x1b]),
                '\\' => Some(vec![0x1c]),
                ']' => Some(vec![0x1d]),
                '^' => Some(vec![0x1e]),
                '_' => Some(vec![0x1f]),
                ' ' | '@' => Some(vec![0]),
                _ => None,
            });
    }
    let mut bytes = match key.key.as_str() {
        "enter" => vec![b'\r'],
        "tab" => vec![b'\t'],
        "backspace" => vec![0x7f],
        "escape" => vec![0x1b],
        "space" => vec![b' '],
        _ => {
            let text = key.key_char.as_deref().unwrap_or(&key.key);
            if text.chars().count() != 1 && key.key_char.is_none() {
                return None;
            }
            text.as_bytes().to_vec()
        }
    };
    if key.modifiers.alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

pub(super) fn mouse_bytes(
    button: u8,
    column: usize,
    row: usize,
    modifiers: Modifiers,
    released: bool,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let code = button
        + 4 * u8::from(modifiers.shift)
        + 8 * u8::from(modifiers.alt)
        + 16 * u8::from(modifiers.control);
    if mode.contains(TermMode::SGR_MOUSE) {
        return Some(
            format!(
                "\x1b[<{code};{};{}{}",
                column + 1,
                row + 1,
                if released { 'm' } else { 'M' }
            )
            .into_bytes(),
        );
    }
    let code = if released { 3 + (code & 28) } else { code };
    let mut bytes = vec![0x1b, b'[', b'M', code + 32];
    for position in [column, row] {
        if mode.contains(TermMode::UTF8_MOUSE) {
            let character = char::from_u32(u32::try_from(position).ok()?.checked_add(33)?)?;
            bytes.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
        } else {
            bytes.push(u8::try_from(position).ok()?.checked_add(33)?);
        }
    }
    Some(bytes)
}

/// Compositions are local until committed; partial syllables never reach stdin.
#[derive(Default)]
pub(super) struct Composition {
    pub text: String,
    pub selected: Range<usize>,
}
impl Composition {
    fn byte_index(&self, utf16: usize) -> usize {
        let mut offset = 0;
        for (byte, ch) in self.text.char_indices() {
            if offset >= utf16 {
                return byte;
            }
            offset += ch.len_utf16();
        }
        self.text.len()
    }
    fn range(&self, range: Range<usize>) -> Range<usize> {
        self.byte_index(range.start)..self.byte_index(range.end.max(range.start))
    }
    pub fn clear(&mut self) {
        self.text.clear();
        self.selected = 0..0;
    }
}

impl EntityInputHandler for TerminalView {
    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            return;
        }
        if let Some(text) = item.text() {
            let bytes = encode_paste(&text, self.term.mode().contains(TermMode::BRACKETED_PASTE));
            self.composition.clear();
            self.send_input(bytes, cx);
        }
    }
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let bytes = self.composition.range(range);
        let start = self.composition.text[..bytes.start].encode_utf16().count();
        let text = self.composition.text[bytes].to_owned();
        *adjusted = Some(start..start + text.encode_utf16().count());
        Some(text)
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.composition.selected.clone(),
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        (!self.composition.text.is_empty()).then(|| 0..self.composition.text.encode_utf16().count())
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.composition.clear();
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composition.clear();
        if self.focus.is_focused(window) {
            self.send_input(text.as_bytes().to_vec(), cx);
        }
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.focus.is_focused(window) || !matches!(self.state, State::Running) {
            return;
        }
        let bytes = range
            .map(|range| self.composition.range(range))
            .unwrap_or(0..self.composition.text.len());
        let start = self.composition.text[..bytes.start].encode_utf16().count();
        self.composition.text.replace_range(bytes, text);
        let count = text.encode_utf16().count();
        let selected = selected.unwrap_or(count..count);
        self.composition.selected =
            start + selected.start.min(count)..start + selected.end.min(count);
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let cursor = self.term.grid().cursor.point;
        Some(Bounds::new(
            bounds.origin
                + point(
                    px(cursor.column.0 as f32 * self.metrics.width),
                    px(cursor.line.0.max(0) as f32 * self.metrics.height),
                ),
            size(px(self.metrics.width), px(self.metrics.height)),
        ))
    }
    fn character_index_for_point(
        &mut self,
        _: gpui_kit::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.composition.selected.end)
    }
    fn accepts_text_input(&self, window: &mut Window, _: &mut Context<Self>) -> bool {
        self.focus.is_focused(window) && matches!(self.state, State::Running)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn xterm_navigation_function_keys_and_shift_tab_keep_modifiers() {
        for (key, expected) in [
            ("ctrl-left", "\x1b[1;5D"),
            ("shift-up", "\x1b[1;2A"),
            ("alt-delete", "\x1b[3;3~"),
            ("shift-tab", "\x1b[Z"),
            ("f1", "\x1bOP"),
            ("ctrl-f4", "\x1b[1;5S"),
            ("f12", "\x1b[24~"),
        ] {
            assert_eq!(
                key_bytes(&Keystroke::parse(key).unwrap(), false).unwrap(),
                expected.as_bytes(),
                "{key}"
            );
        }
    }

    #[::core::prelude::v1::test]
    fn sgr_and_legacy_mouse_coordinates_and_releases_are_encoded() {
        let mode = TermMode::SGR_MOUSE;
        assert_eq!(
            mouse_bytes(0, 3, 2, Modifiers::default(), false, mode).unwrap(),
            b"\x1b[<0;4;3M"
        );
        assert_eq!(
            mouse_bytes(0, 3, 2, Modifiers::default(), true, mode).unwrap(),
            b"\x1b[<0;4;3m"
        );
        assert_eq!(
            mouse_bytes(64, 3, 2, Modifiers::default(), false, mode).unwrap(),
            b"\x1b[<64;4;3M"
        );
        assert_eq!(
            mouse_bytes(0, 0, 0, Modifiers::default(), false, TermMode::NONE).unwrap(),
            b"\x1b[M !!"
        );
        assert_eq!(
            mouse_bytes(0, 0, 0, Modifiers::default(), true, TermMode::NONE).unwrap(),
            b"\x1b[M#!!"
        );
        assert!(mouse_bytes(0, 223, 0, Modifiers::default(), false, TermMode::NONE).is_none());
        assert!(
            mouse_bytes(0, 300, 0, Modifiers::default(), false, TermMode::UTF8_MOUSE).is_some()
        );
    }

    #[::core::prelude::v1::test]
    fn composition_utf16_ranges_do_not_split_surrogate_pairs() {
        let composition = Composition {
            text: "a😀中".into(),
            selected: 0..0,
        };
        assert_eq!(composition.range(1..3), 1..5);
        assert_eq!(&composition.text[composition.range(1..3)], "😀");
        assert_eq!(
            composition.range(99..100),
            composition.text.len()..composition.text.len()
        );
        assert!(text_key(&Keystroke::parse("a").unwrap()));
        assert!(!text_key(&Keystroke::parse("ctrl-c").unwrap()));
        assert!(!text_key(&Keystroke::parse("alt-b").unwrap()));
    }
}
