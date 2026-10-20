//! Shared resource actions for the row menu and detail-header toolbar.
use std::rc::Rc;

use beacon_kube::Operation;
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    Icon,
    menu::{PopupMenu, PopupMenuItem},
};
use gpui_kit::*;

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(Default)]
pub(crate) struct ResourceMenu {
    items: Vec<MenuEntry>,
}

struct MenuEntry {
    item: PopupMenuItem,
    destructive: bool,
    show_in_header: bool,
}

pub(crate) enum HeaderItem {
    Separator,
    Action {
        label: SharedString,
        icon: Box<Icon>,
        disabled: bool,
        destructive: bool,
        handler: Handler,
    },
}

impl ResourceMenu {
    pub fn label(self, label: impl Into<SharedString>) -> Self {
        self.push(PopupMenuItem::label(label), false, false)
    }

    pub fn item(self, item: PopupMenuItem) -> Self {
        self.push(item, false, true)
    }

    pub fn popup_only_item(self, item: PopupMenuItem) -> Self {
        self.push(item, false, false)
    }

    fn push(mut self, item: PopupMenuItem, destructive: bool, show_in_header: bool) -> Self {
        self.items.push(MenuEntry {
            item,
            destructive,
            show_in_header,
        });
        self
    }

    pub fn operation(self, item: PopupMenuItem, operation: &Operation) -> Self {
        use beacon_kube::workflow_ops::Action;
        let (icon, destructive) = match operation {
            Operation::Delete => (IconName::Trash, true),
            Operation::Restart => (IconName::RefreshCw, false),
            Operation::Scale { .. } => (IconName::SlidersHorizontal, false),
            Operation::Apply => (IconName::FileCode, false),
            Operation::RunJob => (IconName::Play, false),
            Operation::CronJobSuspend { suspend } => (
                if *suspend {
                    IconName::Pause
                } else {
                    IconName::Play
                },
                false,
            ),
            Operation::NodeCordon { unschedulable } => (
                if *unschedulable {
                    IconName::Lock
                } else {
                    IconName::Play
                },
                false,
            ),
            Operation::NodeDrain => (IconName::Unplug, false),
            Operation::Workflow { action } => match action {
                Action::Suspend => (IconName::Pause, false),
                Action::Stop => (IconName::CircleX, true),
                Action::Terminate => (IconName::X, true),
                Action::Retry | Action::Resubmit => (IconName::RefreshCw, false),
                _ => (IconName::Play, false),
            },
        };
        self.push(item.icon(icon), destructive, true)
    }

    pub fn separator(self) -> Self {
        self.push(PopupMenuItem::Separator, false, true)
    }

    pub fn popup(self, menu: PopupMenu) -> PopupMenu {
        self.items
            .into_iter()
            .fold(menu, |menu, entry| menu.item(entry.item))
    }

    pub fn header(self) -> Vec<HeaderItem> {
        self.items
            .into_iter()
            .filter(|entry| entry.show_in_header)
            .filter_map(
                |MenuEntry {
                     item, destructive, ..
                 }| match item {
                    PopupMenuItem::Separator => Some(HeaderItem::Separator),
                    PopupMenuItem::Item {
                        label,
                        icon,
                        disabled,
                        handler,
                        action,
                        ..
                    } => {
                        let handler = handler.or_else(|| {
                            action.map(|action| {
                                Rc::new(move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                    window.dispatch_action(action.boxed_clone(), cx)
                                }) as Handler
                            })
                        })?;
                        Some(HeaderItem::Action {
                            label,
                            icon: Box::new(icon.unwrap_or_else(|| Icon::new(IconName::Info))),
                            disabled,
                            destructive,
                            handler,
                        })
                    }
                    // Resource menus have no custom elements or submenus. The label is
                    // the identity already displayed by the detail header.
                    _ => None,
                },
            )
            .collect()
    }
}
