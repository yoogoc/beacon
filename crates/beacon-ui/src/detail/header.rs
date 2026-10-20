//! Hovering the resource icon reveals the same actions as the row menu.
use super::*;
use crate::{resource_menu::HeaderItem, table::ResourceTable};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{menu::ContextMenuExt as _, table::TableState};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct HeaderState {
    source: Option<Entity<TableState<ResourceTable>>>,
    items: Vec<HeaderItem>,
    open: bool,
    transition: u64,
    from: f32,
    started: Option<Instant>,
    scroll: ScrollHandle,
}

impl DetailView {
    pub(crate) fn attach_resource_table(&mut self, table: Entity<TableState<ResourceTable>>) {
        self.header.source = Some(table);
    }

    fn refresh_header_actions(&mut self, cx: &mut Context<Self>) {
        if let Some(table) = &self.header.source {
            self.header.items = table
                .read(cx)
                .delegate()
                .resource_menu(self.target.clone(), cx)
                .header();
        }
    }

    fn set_header_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.header.open == open {
            return;
        }
        if open {
            self.refresh_header_actions(cx);
            if self.header.items.is_empty() {
                return;
            }
        }
        let previous_target = if self.header.open { 1. } else { 0. };
        let elapsed = self.header.started.map_or(1., |started| {
            (started.elapsed().as_secs_f32() / 0.15).clamp(0., 1.)
        });
        self.header.from =
            self.header.from + (previous_target - self.header.from) * ease_in_out(elapsed);
        self.header.started = Some(Instant::now());
        self.header.open = open;
        if open {
            self.header.scroll.set_offset(Point::default());
        }
        self.header.transition = self.header.transition.wrapping_add(1);
        cx.notify();
    }

    pub(super) fn defer_header_refresh(&self, cx: &mut Context<Self>) {
        if !self.header.open {
            return;
        }
        // Refreshes can arrive from a borrowed parent ClusterView. Read the
        // shared menu after that update completes, never re-enter the parent.
        let view = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = view.update(cx, |view, cx| {
                if view.header.open {
                    view.refresh_header_actions(cx);
                    cx.notify();
                }
            });
        });
    }

    fn header_pointer_listener(&self, cx: &Context<Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                // Hover can turn false while a child captures the pointer or
                // a scrollbar/tooltip changes hit testing. Only the stationary
                // header's geometry determines whether the pointer left it.
                let moving = view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase.capture()
                        && event.pressed_button != Some(MouseButton::Left)
                        && !bounds.contains(&event.position)
                    {
                        let _ = moving.update(cx, |view, cx| view.set_header_open(false, cx));
                    }
                });
                let released = view.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase.capture() && !bounds.contains(&event.position) {
                        let _ = released.update(cx, |view, cx| view.set_header_open(false, cx));
                    }
                });
                let pressed = view.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                    if phase.capture() && !bounds.contains(&event.position) {
                        let _ = pressed.update(cx, |view, cx| view.set_header_open(false, cx));
                    }
                });
                let exited = view.clone();
                window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
                    if phase.capture() {
                        let _ = exited.update(cx, |view, cx| view.set_header_open(false, cx));
                    }
                });
            },
        )
        .absolute()
        .inset_0()
    }

    fn header_toolbar(
        &self,
        cx: &mut Context<Self>,
    ) -> gpui_kit::base::ObservedElement<Stateful<Div>> {
        h_flex()
            .id("detail-header-actions")
            .test_support()
            .absolute()
            .inset_0()
            .items_center()
            .gap_1()
            .overflow_x_scroll()
            .track_scroll(&self.header.scroll)
            .whitespace_nowrap()
            .children(self.header.items.iter().enumerate().map(|(index, item)| {
                match item {
                    HeaderItem::Separator => div()
                        .flex_shrink_0()
                        .mx_1()
                        .w(px(1.))
                        .h(px(18.))
                        .bg(cx.theme().border)
                        .into_any_element(),
                    HeaderItem::Action {
                        label,
                        icon,
                        disabled,
                        destructive,
                        handler,
                    } => {
                        let handler = handler.clone();
                        let view = cx.weak_entity();
                        Button::new(("detail-resource-action", index))
                            .small()
                            .ghost()
                            .size(px(30.))
                            .flex_shrink_0()
                            .icon(icon.as_ref().clone().size_4())
                            .accessibility_label(label.clone())
                            .tooltip(label.clone())
                            .disabled(*disabled)
                            .when(*destructive, |button| button.text_color(cx.theme().danger))
                            .on_click(move |event, window, cx| {
                                let handler = handler.clone();
                                let event = event.clone();
                                let view = view.clone();
                                // An action can select another detail tab or close
                                // this panel. Dispatch outside its current borrow.
                                window.defer(cx, move |window, cx| {
                                    handler(&event, window, cx);
                                    let _ = view.update(cx, |view, cx| {
                                        if view.header.open {
                                            view.refresh_header_actions(cx);
                                            cx.notify();
                                        }
                                    });
                                });
                            })
                            .into_any_element()
                    }
                }
            }))
            .horizontal_scrollbar(&self.header.scroll)
    }

    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let subtitle = match &self.target.namespace {
            Some(namespace) => format!("{} · {namespace}", self.kind.display_name()),
            None => self.kind.display_name(),
        };
        let open = self.header.open;
        let from = self.header.from;
        let target_opacity = if open { 1. } else { 0. };
        let identity = v_flex()
            .id("detail-header-identity")
            .test_support()
            .absolute()
            .inset_0()
            .justify_center()
            .gap_1()
            .min_w_0()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(SelectableText::new("detail-name", self.target.name.clone())),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SelectableText::new("detail-subtitle", subtitle)),
            );
        let toolbar = self.header_toolbar(cx);
        let (identity, toolbar) = if self.header.transition == 0 {
            (
                identity.into_any_element(),
                toolbar.invisible().into_any_element(),
            )
        } else {
            let animation = || Animation::new(Duration::from_millis(150)).with_easing(ease_in_out);
            (
                identity
                    .with_animation(
                        ("detail-identity", self.header.transition),
                        animation(),
                        move |identity, delta| {
                            let opacity = 1. - (from + (target_opacity - from) * delta);
                            identity
                                .opacity(opacity)
                                .left(px(-4. * (1. - opacity)))
                                .when(open && delta >= 1., |identity| identity.invisible())
                        },
                    )
                    .into_any_element(),
                toolbar
                    .with_animation(
                        ("detail-toolbar", self.header.transition),
                        animation(),
                        move |toolbar, delta| {
                            let opacity = from + (target_opacity - from) * delta;
                            toolbar
                                .opacity(opacity)
                                .left(px(6. * (1. - opacity)))
                                .when(!open && delta >= 1., |toolbar| toolbar.invisible())
                        },
                    )
                    .into_any_element(),
            )
        };
        let source = self.header.source.clone();
        let target = self.target.clone();
        let row = h_flex()
            .id("detail-header-row")
            .test_support()
            .relative()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .when(open, |row| row.child(self.header_pointer_listener(cx)))
            .child(
                Button::new("detail-resource-icon")
                    .ghost()
                    .size(px(36.))
                    .flex_shrink_0()
                    .rounded_md()
                    .bg(cx.theme().muted.opacity(0.5))
                    .when(open, |button| button.bg(cx.theme().primary.opacity(0.10)))
                    .icon(
                        crate::icons::resource(&self.kind.resource.group, &self.kind.resource.kind)
                            .size_4()
                            .text_color(if open {
                                cx.theme().primary
                            } else {
                                cx.theme().muted_foreground
                            }),
                    )
                    .accessibility_label(crate::i18n::text("Resource actions"))
                    .on_mouse_move(cx.listener(|view, _, _, cx| {
                        view.set_header_open(true, cx);
                    }))
                    .on_click(cx.listener(|view, _, _, cx| view.set_header_open(true, cx))),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h(px(40.))
                    .overflow_hidden()
                    .child(identity)
                    .child(toolbar),
            )
            .child(
                Button::new("close-detail")
                    .xsmall()
                    .ghost()
                    .label("×")
                    .tooltip(crate::i18n::text("Close the details panel"))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DetailClosed))),
            )
            .context_menu(move |menu, _, cx| match &source {
                Some(table) => table
                    .read(cx)
                    .delegate()
                    .resource_menu(target.clone(), cx)
                    .popup(menu),
                None => menu,
            });
        v_flex()
            .w_full()
            .flex_shrink_0()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(row)
            .child(
                TabBar::new("detail-tabs")
                    .segmented()
                    .w_full()
                    .small()
                    .selected_index(
                        self.tabs
                            .iter()
                            .position(|tab| *tab == self.tab)
                            .unwrap_or(0),
                    )
                    .children(
                        self.tabs
                            .iter()
                            .map(|tab| Tab::new().child(crate::i18n::text(tab.label())))
                            .collect::<Vec<_>>(),
                    )
                    .on_click(cx.listener(|view, index: &usize, window, cx| {
                        if let Some(tab) = view.tabs.get(*index).copied() {
                            view.select(tab, window, cx);
                        }
                    })),
            )
    }
}
