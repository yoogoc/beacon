//! Compact selection controls shared by menus, tables and settings.
use gpui_kit::base::{Checkbox as BaseCheckbox, Radio as BaseRadio, StyledExt as _};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::rc::Rc;

type Change = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

#[derive(Clone, Copy)]
enum Mark {
    Check,
    Dot,
}

pub(crate) fn checkbox(id: impl Into<ElementId>) -> Selection {
    Selection::new(id.into(), Mark::Check)
}

pub(crate) fn radio(id: impl Into<ElementId>) -> Selection {
    Selection::new(id.into(), Mark::Dot)
}

/// Base controls retain keyboard activation and accessibility semantics; this
/// layer owns only their presentation and the application's existing callback.
#[derive(IntoElement)]
pub(crate) struct Selection {
    id: ElementId,
    mark: Mark,
    checked: bool,
    disabled: bool,
    label: Option<SharedString>,
    accessibility_label: Option<SharedString>,
    tooltip: Option<SharedString>,
    children: Vec<AnyElement>,
    style: StyleRefinement,
    on_change: Option<Change>,
}

impl Selection {
    fn new(id: ElementId, mark: Mark) -> Self {
        Self {
            id,
            mark,
            checked: false,
            disabled: false,
            label: None,
            accessibility_label: None,
            tooltip: None,
            children: Vec::new(),
            style: StyleRefinement::default(),
            on_change: None,
        }
    }

    pub(crate) fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub(crate) fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub(crate) fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    pub(crate) fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub(crate) fn on_click(
        mut self,
        change: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(change));
        self
    }
}

impl Styled for Selection {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for Selection {
    fn extend(&mut self, children: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(children);
    }
}

impl RenderOnce for Selection {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = window
            .use_keyed_state(self.id.clone(), cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let has_label = self.label.is_some() || !self.children.is_empty();
        let name = self.accessibility_label.or_else(|| self.label.clone());
        let indicator_size = cx.theme().font_size * 0.875;
        let focused = focus.is_focused(window);
        let indicator = div()
            .id((self.id.clone(), "indicator"))
            .test_support()
            .flex_shrink_0()
            .size(indicator_size)
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(match self.mark {
                Mark::Check => 4.,
                Mark::Dot => 99.,
            }))
            .border_1()
            .border_color(if self.checked {
                cx.theme().primary
            } else {
                cx.theme().foreground.opacity(0.3)
            })
            .bg(if self.checked {
                cx.theme().primary
            } else {
                cx.theme().background
            })
            .when(self.checked, |mark| match self.mark {
                Mark::Check => mark.child(
                    Icon::new(IconName::Check)
                        .size(indicator_size * 0.75)
                        .text_color(cx.theme().primary_foreground),
                ),
                Mark::Dot => mark.child(
                    div()
                        .size(indicator_size * 0.36)
                        .rounded_full()
                        .bg(cx.theme().primary_foreground),
                ),
            });
        // Focus belongs around the indicator, not around a whole menu or
        // settings row. Its reserved space also keeps labels from moving.
        let mark = h_flex()
            .size(indicator_size + px(4.))
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .rounded(px(match self.mark {
                Mark::Check => 5.,
                Mark::Dot => 99.,
            }))
            .border_1()
            .border_color(if focused {
                cx.theme().ring.opacity(0.65)
            } else {
                transparent_black()
            })
            .child(indicator);
        let content = h_flex()
            .gap(px(8.))
            .items_start()
            .child(mark)
            .when(has_label, |row| {
                row.w_full().child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .text_size(cx.theme().font_size * 0.8125)
                        .line_height(relative(1.4))
                        .children(self.label.map(|label| div().truncate().child(label)))
                        .children(self.children),
                )
            });
        let style = self.style;
        match self.mark {
            Mark::Check => present(
                BaseCheckbox::new(self.id)
                    .checked(self.checked)
                    .disabled(self.disabled)
                    .track_focus(&focus)
                    .when_some(name, |control, name| control.accessibility_label(name))
                    .when_some(self.on_change, |control, change| {
                        control.on_change(move |state, _, window, cx| {
                            window.prevent_default();
                            change(
                                &(state == gpui_kit::base::CheckboxState::Checked),
                                window,
                                cx,
                            );
                        })
                    }),
                content,
                &style,
                self.disabled,
                self.tooltip,
                cx,
            ),
            Mark::Dot => present(
                BaseRadio::new(self.id)
                    .checked(self.checked)
                    .disabled(self.disabled)
                    .track_focus(&focus)
                    .when_some(name, |control, name| control.accessibility_label(name))
                    .when_some(self.on_change, |control, change| {
                        control.on_change(move |checked, _, window, cx| {
                            window.prevent_default();
                            change(&checked, window, cx);
                        })
                    }),
                content,
                &style,
                self.disabled,
                self.tooltip,
                cx,
            ),
        }
    }
}

fn present<C>(
    control: C,
    content: impl IntoElement,
    style: &StyleRefinement,
    disabled: bool,
    tooltip: Option<SharedString>,
    cx: &App,
) -> AnyElement
where
    C: Styled + ParentElement + StatefulInteractiveElement + IntoElement + 'static,
{
    control
        .min_w(px(24.))
        .min_h(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .text_color(cx.theme().foreground)
        .when(disabled, |control| control.opacity(0.45))
        .when(!disabled, |control| {
            control
                .cursor_pointer()
                .hover(|style| style.bg(cx.theme().muted))
        })
        .refine_style(style)
        .when_some(tooltip, |control, tooltip| {
            control.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        })
        .child(content)
        .into_any_element()
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::component::{Theme, ThemeMode};
    use gpui_kit::test::TestWindowExt as _;

    struct Controls {
        checked: bool,
        choice: usize,
        changes: usize,
    }

    impl Render for Controls {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            v_flex()
                .p_4()
                .gap_2()
                .w(px(260.))
                .child(
                    checkbox("check")
                        .w_full()
                        .label("Enable option")
                        .checked(self.checked)
                        .on_click(cx.listener(|view, checked, _, cx| {
                            view.checked = *checked;
                            view.changes += 1;
                            cx.notify();
                        })),
                )
                .child(
                    checkbox("disabled-check")
                        .label("Unavailable option")
                        .disabled(true)
                        .on_click(cx.listener(|view, _, _, _| view.changes += 1)),
                )
                .children((0..2).map(|choice| {
                    radio(("choice", choice))
                        .w_full()
                        .label(format!("Option {choice}"))
                        .checked(self.choice == choice)
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.choice = choice;
                            view.changes += 1;
                            cx.notify();
                        }))
                }))
        }
    }

    #[::core::prelude::v1::test]
    fn pointer_keyboard_and_disabled_controls_keep_selection_semantics_in_both_themes() {
        for theme in [ThemeMode::Light, ThemeMode::Dark] {
            let cx = &mut support::context();
            let (window, controls) = cx.update(|cx| {
                Theme::change(theme, None, cx);
                gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                    window.set_view_retention(false);
                    cx.new(|_| Controls {
                        checked: false,
                        choice: 0,
                        changes: 0,
                    })
                })
                .unwrap()
            });
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(window.find("check").role(), Some(Role::CheckBox));
                assert_eq!(
                    window.find(("choice", 0usize)).role(),
                    Some(Role::RadioButton)
                );
                window.click("check", cx);
                assert_eq!(window.find("check").checked(), Some(true));
                window.press("space", cx);
                assert_eq!(window.find("check").checked(), Some(false));
                window.click("disabled-check", cx);
                assert_eq!(window.find("disabled-check").checked(), Some(false));
                window.click(("choice", 1usize), cx);
                assert_eq!(window.find(("choice", 1usize)).checked(), Some(true));
                assert_eq!(window.find(("choice", 0usize)).checked(), Some(false));
                window.press("space", cx);
                assert_eq!(window.find(("choice", 1usize)).checked(), Some(true));
            })
            .unwrap();
            assert_eq!(controls.read_with(cx, |controls, _| controls.changes), 3);
        }
    }
}
