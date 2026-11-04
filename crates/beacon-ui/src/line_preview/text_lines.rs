use super::*;
use crate::copyable_text::{copy_item, copyable_text};
use gpui_kit::component::menu::ContextMenuExt as _;
use std::ops::Range;

pub(crate) struct TextLines {
    text: SharedString,
    lines: Arc<Vec<Range<usize>>>,
    widest: usize,
    scroll: UniformListScrollHandle,
    preview: Entity<LinePreview>,
}
impl TextLines {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self {
            text: "".into(),
            lines: Arc::new(std::iter::once(0..0).collect()),
            widest: 0,
            scroll: UniformListScrollHandle::new(),
            preview: LinePreview::new(cx),
        })
    }
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.text.as_ref() == text {
            return;
        }
        self.text = text.to_owned().into();
        let mut offset = 0;
        self.lines = Arc::new(
            text.split_inclusive('\n')
                .map(|line| {
                    let end = offset + line.trim_end_matches(['\r', '\n']).len();
                    let range = offset..end;
                    offset += line.len();
                    range
                })
                .collect(),
        );
        self.widest = self
            .lines
            .iter()
            .enumerate()
            .max_by_key(|(_, range)| range.len())
            .map_or(0, |(index, _)| index);
        self.scroll = UniformListScrollHandle::new();
        self.preview.update(cx, |view, cx| view.close(cx));
        cx.notify();
    }
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.preview.update(cx, |view, cx| view.close(cx));
    }
}
impl Render for TextLines {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = self.text.clone();
        let all = text.clone();
        let lines = self.lines.clone();
        let owner = self.preview.clone();
        div()
            .id("file-preview-scroll")
            .test_support()
            .relative()
            .flex_1()
            .min_size_0()
            .size_full()
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(cx.theme().mono_font_size)
            .child(
                uniform_list("file-content", lines.len(), move |range, _, cx| {
                    range
                        .map(|index| {
                            let raw: SharedString = text[lines[index].clone()].to_owned().into();
                            let row = h_flex()
                                .px_3()
                                .gap_3()
                                .min_h(px(24.))
                                .whitespace_nowrap()
                                .child(
                                    div()
                                        .w(px(32.))
                                        .flex_shrink_0()
                                        .text_right()
                                        .text_color(cx.theme().muted_foreground)
                                        .child((index + 1).to_string()),
                                )
                                .child(copyable_text("file-line", raw.clone()));
                            super::trigger(&owner, index, raw, row, cx)
                        })
                        .collect()
                })
                .with_width_from_item(Some(self.widest))
                .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                .track_scroll(&self.scroll)
                .size_full()
                .py_1()
                .pb_3(),
            )
            .child(gpui_kit::component::scroll::Scrollbar::vertical(
                &self.scroll,
            ))
            .child(gpui_kit::component::scroll::Scrollbar::horizontal(
                &self.scroll,
            ))
            .child(self.preview.clone())
            .context_menu(move |menu, _, _| menu.item(copy_item("Copy file contents", all.clone())))
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;

    #[::core::prelude::v1::test]
    fn file_rows_are_virtual_and_pinned_snapshots_survive_scrolling_but_not_file_changes() {
        let cx = &mut support::context();
        let dir = tempfile::tempdir().unwrap();
        support::workspace(cx, dir.path());
        let text = (0..5000)
            .map(|id| format!("{{\"id\":{id},\"ready\":true}}\n"))
            .collect::<String>();
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(760.), px(680.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_view_retention(false);
                    TextLines::new(cx)
                },
            )
            .unwrap()
        });
        view.update(cx, |view, cx| view.set_text(&text, cx));
        let preview = view.read_with(cx, |view, _| view.preview.clone());
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window
                    .try_find(("line-preview-trigger", 4999usize))
                    .is_none()
            );
            window.hover(("line-preview-trigger", 0usize), cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.executor().advance_clock(OPEN_DELAY);
        support::settle(cx, |cx| {
            preview.read_with(cx, |view, _| view.active.is_some())
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("line-preview-pin", cx);
        })
        .unwrap();
        view.update(cx, |view, cx| {
            view.scroll.scroll_to_item(4900, ScrollStrategy::Top);
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(("line-preview-trigger", 0usize)).is_none());
            assert!(window.find("line-preview").visible());
            window.click("line-preview-raw", cx);
            window.click("line-preview-copy", cx);
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("{\"id\":0,\"ready\":true}")
            );
        })
        .unwrap();
        view.update(cx, |view, cx| view.set_text("different file", cx));
        assert!(preview.read_with(cx, |view, _| view.active.is_none()));
    }
}
