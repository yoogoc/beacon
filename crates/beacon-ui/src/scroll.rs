//! Scroll areas keep their identity, layout, and caller-provided scroll handles.
//! Desktop mouse users must be able to reach horizontal overflow without gestures.
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::*;
use std::panic::Location;

pub(crate) const DESKTOP_SCROLLBARS: bool = cfg!(any(target_os = "windows", target_os = "linux"));

pub(crate) trait OverflowScrollbars:
    Element + StatefulInteractiveElement + Styled + ParentElement + Sized
{
    #[track_caller]
    fn overflow_x_scrollbar(self) -> ScrollArea<Self> {
        ScrollArea::new(self.overflow_x_scroll())
    }

    #[track_caller]
    fn overflow_y_scrollbar(self) -> ScrollArea<Self> {
        let element = self.overflow_y_scroll();
        ScrollArea::new(if DESKTOP_SCROLLBARS {
            element.overflow_x_scroll()
        } else {
            element
        })
    }

    #[track_caller]
    fn overflow_scrollbar(self) -> ScrollArea<Self> {
        ScrollArea::new(self.overflow_scroll())
    }
}
impl<E> OverflowScrollbars for E where
    E: Element + StatefulInteractiveElement + Styled + ParentElement
{
}

pub(crate) struct ScrollArea<E> {
    element: Option<E>,
    key: ElementId,
    handle: Option<ScrollHandle>,
    installed: bool,
    desktop: bool,
}
impl<E: Element> ScrollArea<E> {
    #[track_caller]
    fn new(element: E) -> Self {
        let key = Element::id(&element).unwrap_or(ElementId::CodeLocation(*Location::caller()));
        Self {
            element: Some(element),
            key,
            handle: None,
            installed: false,
            desktop: DESKTOP_SCROLLBARS,
        }
    }
    fn element(&self) -> &E {
        self.element.as_ref().expect("scroll area element")
    }
    fn element_mut(&mut self) -> &mut E {
        self.element.as_mut().expect("scroll area element")
    }
}
impl<E: Element + Styled> Styled for ScrollArea<E> {
    fn style(&mut self) -> &mut StyleRefinement {
        self.element_mut().style()
    }
}
impl<E: Element + ParentElement> ParentElement for ScrollArea<E> {
    fn extend(&mut self, children: impl IntoIterator<Item = AnyElement>) {
        self.element_mut().extend(children);
    }
}
impl<E: Element + InteractiveElement> InteractiveElement for ScrollArea<E> {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.element_mut().interactivity()
    }
}
impl<E: Element + StatefulInteractiveElement> StatefulInteractiveElement for ScrollArea<E> {
    fn track_scroll(mut self, handle: &ScrollHandle) -> Self {
        self.handle = Some(handle.clone());
        self
    }
}
impl<E> IntoElement for ScrollArea<E>
where
    E: Element + StatefulInteractiveElement + Styled + ParentElement,
{
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl<E> Element for ScrollArea<E>
where
    E: Element + StatefulInteractiveElement + Styled + ParentElement,
{
    type RequestLayoutState = E::RequestLayoutState;
    type PrepaintState = E::PrepaintState;
    fn id(&self) -> Option<ElementId> {
        Some(self.key.clone())
    }
    fn source_location(&self) -> Option<&'static Location<'static>> {
        self.element().source_location()
    }
    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        if !self.installed {
            let mut element = self.element.take().expect("scroll area element");
            if self.desktop {
                let handle = self.handle.clone().unwrap_or_else(|| {
                    window
                        .use_keyed_state(
                            (self.key.clone(), "overflow-scroll-handle"),
                            cx,
                            |_, _| ScrollHandle::new(),
                        )
                        .read(cx)
                        .clone()
                });
                // Keep the bottom line and toolbar controls clear of the rail.
                let padding = element.style().padding.bottom.unwrap_or(px(0.).into());
                element.style().padding.bottom = Some(
                    (padding.to_pixels(px(0.).into(), window.rem_size()) + Scrollbar::width())
                        .into(),
                );
                element = element
                    .track_scroll(&handle)
                    .child(Scrollbar::horizontal(&handle).mode(ScrollbarMode::Always));
            } else if let Some(handle) = &self.handle {
                element = element.track_scroll(handle);
            }
            self.element = Some(element);
            self.installed = true;
        }
        self.element_mut().request_layout(id, inspector, window, cx)
    }
    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.element_mut()
            .prepaint(id, inspector, bounds, layout, window, cx)
    }
    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        paint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.element_mut()
            .paint(id, inspector, bounds, layout, paint, window, cx);
    }
    fn a11y_role(&self) -> Option<Role> {
        self.element().a11y_role()
    }
    fn write_a11y_info(&self, node: &mut accesskit::Node) {
        self.element().write_a11y_info(node);
    }
    fn a11y_synthetic_children(
        &mut self,
        paint: &mut Self::PrepaintState,
        builder: &mut A11ySubtreeBuilder,
    ) {
        self.element_mut().a11y_synthetic_children(paint, builder);
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests {
    use super::*;
    use gpui_kit::prelude::FluentBuilder as _;
    use gpui_kit::test::TestWindowExt as _;

    struct OverflowView {
        handle: ScrollHandle,
        supplied: bool,
    }
    impl Render for OverflowView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let mut area = div()
                .id("overflow-area")
                .test_support()
                .size_full()
                .overflow_scrollbar();
            // Exercise the Windows/Linux policy on every test host.
            area.desktop = true;
            area.when(self.supplied, |area| area.track_scroll(&self.handle))
                .child(
                    div().w(px(1200.)).h(px(800.)).flex_shrink_0().child(
                        div()
                            .id("overflow-end")
                            .test_support()
                            .ml(px(1100.))
                            .w(px(100.))
                            .h(px(30.))
                            .child("End"),
                    ),
                )
        }
    }

    #[::core::prelude::v1::test]
    fn scrollbar_drags_preserve_identity_and_position_across_renders() {
        for retained in [false, true] {
            for supplied in [false, true] {
                let cx = &mut crate::feature_test_support::context();
                let (window, view) = cx.update(|cx| {
                    gpui_kit::open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                Point::default(),
                                size(px(400.), px(250.)),
                            ))),
                            ..Default::default()
                        },
                        cx,
                        |window, cx| {
                            window.set_view_retention(retained);
                            cx.new(|_| OverflowView {
                                handle: ScrollHandle::new(),
                                supplied,
                            })
                        },
                    )
                    .unwrap()
                });
                cx.update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    window.render_frame(cx);
                    let bounds = window.find("overflow-area").bounds();
                    let y = bounds.bottom() - Scrollbar::width() / 2.;
                    window.drag(
                        point(bounds.left() + px(20.), y),
                        point(bounds.right() - px(20.), y),
                        cx,
                    );
                    let end = window.find("overflow-end").bounds();
                    assert!(
                        end.right() <= bounds.right() + px(1.),
                        "last content must be reachable: {end:?}"
                    );
                    if supplied {
                        assert!(view.read(cx).handle.offset().x < px(0.));
                    }
                    view.update(cx, |_, cx| cx.notify());
                    window.render_frame(cx);
                    assert_eq!(window.find("overflow-end").bounds(), end);
                })
                .unwrap();
                cx.simulate_window_resize(window, size(px(1400.), px(250.)));
                cx.update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    window.render_frame(cx);
                    let bounds = window.find("overflow-area").bounds();
                    let end = window.find("overflow-end").bounds();
                    assert!(end.right() <= bounds.right());
                    if supplied {
                        assert_eq!(view.read(cx).handle.offset().x, px(0.));
                    }
                })
                .unwrap();
            }
        }
    }
}
