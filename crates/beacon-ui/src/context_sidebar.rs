//! Source headers have their own expansion state, independent of cluster rows.
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Collapsible, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    sidebar::{SidebarItem, SidebarMenu},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::rc::Rc;

type Toggle = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(Clone)]
pub(crate) struct SourceGroup {
    pub id: String,
    pub name: String,
    pub icon: IconName,
    pub count: usize,
    pub closed: bool,
    pub menu: SidebarMenu,
    pub toggle: Toggle,
    collapsed: bool,
}
impl SourceGroup {
    pub fn new(
        id: String,
        name: String,
        icon: IconName,
        closed: bool,
        rows: Vec<gpui_kit::component::sidebar::SidebarMenuItem>,
        toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id,
            name,
            icon,
            count: rows.len(),
            closed,
            menu: SidebarMenu::new().children(rows),
            toggle: Rc::new(toggle),
            collapsed: false,
        }
    }
}
impl Collapsible for SourceGroup {
    fn is_collapsed(&self) -> bool {
        self.collapsed
    }
    fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }
}
impl SidebarItem for SourceGroup {
    fn render(
        self,
        _: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let toggle = self.toggle;
        let header = Button::new(format!("toggle-source-{}", self.id))
            .ghost()
            .small()
            .w_full()
            .h_auto()
            .px_2()
            .py_1()
            .accessibility_label(format!(
                "{} {} · {} contexts",
                if self.closed { "Expand" } else { "Collapse" },
                self.name,
                self.count
            ))
            .text_color(cx.theme().muted_foreground)
            .tooltip(self.name.clone())
            .child(
                h_flex()
                    .w_full()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Icon::new(if self.closed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .size_3(),
                    )
                    .child(Icon::new(self.icon).size_3())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .truncate()
                            .child(self.name),
                    )
                    .child(div().text_xs().child(self.count.to_string())),
            )
            .on_click(move |event, window, cx| toggle(event, window, cx));
        v_flex().gap_1().child(header).when(!self.closed, |group| {
            group.child(self.menu.collapsed(self.collapsed).render(
                format!("source-rows-{}", self.id),
                window,
                cx,
            ))
        })
    }
}
