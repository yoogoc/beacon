//! Local customization menus for the current resource list.
use super::*;
use crate::table_preferences::{ColumnLayout, FilterPreset, ResourcePreferences};
use gpui_kit::assets::IconName as Glyph;

#[derive(Clone)]
struct DraggedColumn {
    owner: EntityId,
    resource: String,
    id: String,
    name: String,
    position: usize,
}

impl Render for DraggedColumn {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .text_color(cx.theme().foreground)
            .shadow_md()
            .text_sm()
            .child(
                Icon::new(Glyph::GripVertical)
                    .size(px(14.))
                    .text_color(cx.theme().muted_foreground),
            )
            .child(crate::i18n::text(&self.name))
    }
}

pub(super) struct ListSettings {
    columns_open: bool,
    presets_open: bool,
    name: Entity<InputState>,
    search: Entity<InputState>,
    pub(super) error: Option<String>,
}

impl ListSettings {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ClusterView>) -> Self {
        Self {
            columns_open: false,
            presets_open: false,
            error: None,
            name: cx.new(|cx| crate::i18n::input("Filter name", window, cx)),
            search: cx.new(|cx| crate::i18n::input("Search saved filters", window, cx)),
        }
    }
    pub(super) fn close(&mut self) {
        self.columns_open = false;
        self.presets_open = false;
        self.error = None;
    }

    pub(super) fn is_open(&self) -> bool {
        self.columns_open || self.presets_open
    }
}

impl ClusterView {
    pub(super) fn resource_preferences(&self, cx: &App) -> ResourcePreferences {
        self.kind
            .as_ref()
            .and_then(|kind| {
                crate::settings::store(cx)
                    .read(cx)
                    .preferences
                    .resources
                    .get(&crate::table_preferences::key(self.session.id(), kind))
                    .cloned()
            })
            .unwrap_or_default()
    }

    fn save_resource_preferences(
        &mut self,
        preferences: ResourcePreferences,
        cx: &mut Context<Self>,
    ) {
        let Some(kind) = &self.kind else {
            return;
        };
        let key = crate::table_preferences::key(self.session.id(), kind);
        self.list_settings.error = crate::settings::save_resource(key, preferences, cx).err();
        cx.notify();
    }

    pub(super) fn persist_layout(&mut self, layout: ColumnLayout, cx: &mut Context<Self>) {
        let mut preferences = self.resource_preferences(cx);
        if preferences.columns == layout {
            return;
        }
        preferences.columns = layout;
        self.save_resource_preferences(preferences, cx);
    }

    fn toggle_list_column(&mut self, id: &str, cx: &mut Context<Self>) {
        let layout = self.table.update(cx, |table, cx| {
            table.delegate_mut().toggle_column(id);
            table.refresh(cx);
            table.delegate().layout_snapshot()
        });
        self.persist_layout(layout, cx);
    }

    fn reorder_list_column(
        &mut self,
        drag: &DraggedColumn,
        destination: &str,
        cx: &mut Context<Self>,
    ) {
        if drag.owner != cx.entity_id()
            || !self.list_settings.columns_open
            || self
                .kind
                .as_ref()
                .map(|kind| crate::table_preferences::key(self.session.id(), kind))
                .as_ref()
                != Some(&drag.resource)
        {
            return;
        }
        let layout = self.table.update(cx, |table, cx| {
            table
                .delegate_mut()
                .reorder_column_by_id(&drag.id, destination);
            table.refresh(cx);
            table.delegate().layout_snapshot()
        });
        self.persist_layout(layout, cx);
    }

    fn reset_list_columns(&mut self, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            table.delegate_mut().apply_layout(Default::default());
            table.refresh(cx);
        });
        self.persist_layout(Default::default(), cx);
    }

    fn save_filter_preset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.list_settings.name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return;
        }
        let preset = self
            .table
            .read(cx)
            .delegate()
            .filter_preset(self.scoped_to.clone());
        let mut preferences = self.resource_preferences(cx);
        preferences.filters.insert(name, preset);
        self.save_resource_preferences(preferences, cx);
        if self.list_settings.error.is_none() {
            self.list_settings
                .name
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.list_settings
                .search
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    fn apply_filter_preset(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preset) = self.resource_preferences(cx).filters.get(name).cloned() else {
            return;
        };
        self.rescope(preset.namespaces.clone(), window, cx);
        let result = self.table.update(cx, |table, cx| {
            table.delegate_mut().apply_preset(&preset)?;
            table.refresh(cx);
            table.clear_selection(cx);
            table.scroll_to_row(0, cx);
            Ok::<_, String>(())
        });
        if let Err(error) = result {
            self.list_settings.error = Some(error);
        } else {
            self.row_search
                .update(cx, |input, cx| input.set_value(preset.search, window, cx));
            self.label_input
                .update(cx, |input, cx| input.set_value(preset.labels, window, cx));
            self.label_error = None;
            self.detail = None;
            self.list_settings.presets_open = false;
            let layout = self.table.read(cx).delegate().layout_snapshot();
            self.persist_layout(layout, cx);
        }
        cx.notify();
    }

    fn remove_filter_preset(&mut self, name: &str, cx: &mut Context<Self>) {
        let mut preferences = self.resource_preferences(cx);
        preferences.filters.remove(name);
        self.save_resource_preferences(preferences, cx);
    }

    fn open_list_menu(
        &mut self,
        columns: bool,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if columns {
            self.list_settings.columns_open = open;
        } else {
            self.list_settings.presets_open = open;
        }
        if open {
            if columns {
                self.list_settings.presets_open = false;
            } else {
                self.list_settings.columns_open = false;
            }
            self.namespace_menu_open = false;
            self.filter_menu_open = None;
            self.label_menu_open = false;
            self.picker_search
                .update(cx, |input, cx| input.set_value("", window, cx));
            if !columns {
                self.list_settings
                    .search
                    .update(cx, |input, cx| input.set_value("", window, cx));
            }
        }
        cx.notify();
    }

    pub(super) fn listen_list_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let subscription = cx.subscribe_in(
            &self.list_settings.name,
            window,
            |view, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => view.save_filter_preset(window, cx),
                InputEvent::Change => {
                    view.list_settings.error = None;
                    cx.notify();
                }
                _ => {}
            },
        );
        self._subscriptions.push(subscription);
        self._subscriptions.push(cx.subscribe_in(
            &self.list_settings.search,
            window,
            |_, _, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        ));
    }

    pub(super) fn render_column_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let choices = self.table.read(cx).delegate().column_choices();
        let search = OptionSearch::new(self.picker_search.read(cx).value().as_ref());
        let input = self.picker_search.clone();
        let weak = cx.entity().downgrade();
        let opening = weak.clone();
        let owner = cx.entity_id();
        let resource = self
            .kind
            .as_ref()
            .map(|kind| crate::table_preferences::key(self.session.id(), kind))
            .unwrap_or_default();
        Popover::new("resource-columns")
            .p_1p5()
            .open(self.list_settings.columns_open)
            .on_open_change(move |open, window, cx| {
                let _ = opening.update(cx, |view, cx| view.open_list_menu(true, *open, window, cx));
            })
            .trigger(
                Button::new("resource-columns-trigger")
                    .small()
                    .ghost()
                    .icon(Icon::new(gpui_kit::assets::IconName::Columns3))
                    .accessibility_label(crate::i18n::text("Columns"))
                    .tooltip(crate::i18n::text("Choose visible columns")),
            )
            .content(move |_, _, cx| {
                let reset = weak.clone();
                let rows: Vec<_> = choices
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, name, _, _))| {
                        search.matches(name) || search.matches(&crate::i18n::text(name))
                    })
                    .map(|(position, (id, name, visible, required))| {
                        let toggling = weak.clone();
                        let dropping = weak.clone();
                        let toggled = id.clone();
                        let destination = id.clone();
                        let accepted = id.clone();
                        let resource = resource.clone();
                        let drag = DraggedColumn {
                            owner,
                            resource: resource.clone(),
                            id: id.clone(),
                            name: name.clone(),
                            position,
                        };
                        h_flex()
                            .id(SharedString::from(format!("column-picker-row-{id}")))
                            .test_support()
                            .w_full()
                            .flex_shrink_0()
                            .h(toolbar::picker_row_height(cx))
                            .items_center()
                            .border_y_2()
                            .border_color(transparent_black())
                            .can_drop(move |value, _, _| {
                                value.downcast_ref::<DraggedColumn>().is_some_and(|drag| {
                                    drag.owner == owner
                                        && drag.resource == resource
                                        && drag.id != accepted
                                })
                            })
                            .drag_over::<DraggedColumn>(move |row, drag, _, cx| {
                                let row = row.border_color(cx.theme().primary);
                                if drag.position < position {
                                    row.border_t_0()
                                } else {
                                    row.border_b_0()
                                }
                            })
                            .on_drop(move |drag: &DraggedColumn, _, cx| {
                                let _ = dropping.update(cx, |view, cx| {
                                    view.reorder_list_column(drag, &destination, cx)
                                });
                            })
                            .child(
                                checkbox(SharedString::from(format!("visible-column-{id}")))
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .px_2()
                                    .label(crate::i18n::text(name))
                                    .checked(*visible)
                                    .disabled(*required)
                                    .on_click(move |_, _, cx| {
                                        let _ = toggling.update(cx, |view, cx| {
                                            view.toggle_list_column(&toggled, cx)
                                        });
                                    }),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("drag-column-{id}")))
                                    .test_support()
                                    .w(px(28.))
                                    .h_full()
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor(CursorStyle::OpenHand)
                                    .rounded_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .hover(|handle| {
                                        handle
                                            .bg(cx.theme().muted)
                                            .text_color(cx.theme().foreground)
                                    })
                                    .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                                    .child(Icon::new(Glyph::GripVertical).size(px(14.))),
                            )
                    })
                    .collect();
                v_flex()
                    .w(px(260.))
                    .max_h(px(420.))
                    .min_h_0()
                    .gap_2()
                    .child(
                        div()
                            .px_2()
                            .pt_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(crate::i18n::text("Visible columns")),
                    )
                    .child(toolbar::picker_search("resource-column-search", &input, cx))
                    .child(
                        div()
                            .id("resource-column-options")
                            .min_h_0()
                            .overflow_y_scroll()
                            .child(v_flex().gap_0p5().children(rows)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(crate::i18n::text(
                                "Drag handles to reorder. Changes save automatically.",
                            )),
                    )
                    .child(
                        h_flex().child(
                            Button::new("reset-resource-columns")
                                .small()
                                .ghost()
                                .label(crate::i18n::text("Restore defaults"))
                                .on_click(move |_, _, cx| {
                                    let _ =
                                        reset.update(cx, |view, cx| view.reset_list_columns(cx));
                                }),
                        ),
                    )
            })
    }

    pub(super) fn render_saved_filters(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let preferences = self.resource_preferences(cx);
        let search = OptionSearch::new(self.list_settings.search.read(cx).value().as_ref());
        let input = self.list_settings.search.clone();
        let name = self.list_settings.name.clone();
        let draft = name.read(cx).value();
        let replacing = preferences.filters.contains_key(draft.trim());
        let can_save = !draft.trim().is_empty();
        let current = self
            .table
            .read(cx)
            .delegate()
            .filter_preset(self.scoped_to.clone());
        let namespaced = self.kind.as_ref().is_some_and(|kind| kind.namespaced);
        let summary = filter_summary(&current, namespaced);
        let error = self.list_settings.error.clone();
        let weak = cx.entity().downgrade();
        let opening = weak.clone();
        Popover::new("resource-saved-filters")
            .p_0()
            .open(self.list_settings.presets_open)
            .on_open_change(move |open, window, cx| {
                let _ = opening.update(cx, |view, cx| view.open_list_menu(false, *open, window, cx));
            })
            .trigger(Button::new("resource-saved-filters-trigger").small().ghost()
                .icon(Icon::new(gpui_kit::assets::IconName::Bookmark))
                .accessibility_label(crate::i18n::text("Saved filters")).tooltip(crate::i18n::text("Save or apply resource filters")))
            .content(move |_, _, cx| {
                let saving = weak.clone();
                let empty = preferences.filters.is_empty();
                let rows: Vec<_> = preferences.filters.iter()
                    .filter(|(name, _)| search.matches(name) || search.matches(&crate::i18n::text(name)))
                    .map(|(name, preset)| {
                        let applying = weak.clone();
                        let removing = weak.clone();
                        let applied = name.clone();
                        let removed = name.clone();
                        let active = preset == &current;
                        let summary = filter_summary(preset, namespaced);
                        h_flex()
                            .id(SharedString::from(format!("saved-filter-row-{name}")))
                            .test_support()
                            .gap_1().items_center().rounded_md()
                            .when(active, |row| row.bg(cx.theme().muted.opacity(0.6)))
                            .child(Button::new(SharedString::from(format!("apply-filter-{name}")))
                                .small().ghost().flex_1().min_w_0().h_auto().px_2().py_2()
                                .accessibility_label(format!("Apply filter {name}"))
                                .tooltip(summary.clone())
                                .child(h_flex().w_full().min_w_0().items_center().gap_2()
                                    .child(Icon::new(Glyph::Bookmark).size(px(14.))
                                        .text_color(cx.theme().muted_foreground))
                                    .child(v_flex().flex_1().min_w_0().items_start().gap_0p5()
                                        .child(div().w_full().truncate().text_sm()
                                            .font_weight(FontWeight::MEDIUM).child(name.clone()))
                                        .child(div().w_full().truncate().text_xs()
                                            .text_color(cx.theme().muted_foreground).child(summary)))
                                    .when(active, |row| row.child(div()
                                        .id(SharedString::from(format!("saved-filter-current-{name}")))
                                        .test_support().flex_shrink_0()
                                        .child(Icon::new(Glyph::Check).size(px(14.))
                                            .text_color(cx.theme().foreground)))))
                                .on_click(move |_, window, cx| {
                                    let _ = applying.update(cx, |view, cx| view.apply_filter_preset(&applied, window, cx));
                                }))
                            .child(Button::new(SharedString::from(format!("remove-filter-{name}")))
                                .xsmall().ghost().mr_1()
                                .icon(Icon::new(Glyph::Trash).size(px(14.)))
                                .text_color(cx.theme().muted_foreground)
                                .accessibility_label(format!("Delete filter {name}"))
                                .tooltip(crate::i18n::text("Delete saved filter"))
                                .on_click(move |_, _, cx| {
                                    let _ = removing.update(cx, |view, cx| view.remove_filter_preset(&removed, cx));
                                }))
                    }).collect();
                v_flex()
                    .id("saved-filter-menu").test_support()
                    // Inputs emit PressEnter, then propagate Enter. Keep that
                    // action from toggling the surrounding popover trigger.
                    .on_action(|_: &gpui_kit::component::input::Enter, _, cx| cx.stop_propagation())
                    .w(px(344.)).max_h(px(460.)).min_h_0()
                    .text_color(cx.theme().foreground)
                    .child(h_flex().flex_shrink_0().items_center().gap_2().px_3().py_2()
                        .child(div().flex_1().text_sm().font_weight(FontWeight::SEMIBOLD)
                            .child(crate::i18n::text("Saved filters")))
                        .when(!empty, |header| header.child(div()
                            .rounded_md().px_1p5().py_0p5().text_xs()
                            .bg(cx.theme().muted).text_color(cx.theme().muted_foreground)
                            .child(preferences.filters.len().to_string())))
                        .child(Button::new("saved-filter-help").xsmall().ghost()
                            .icon(Icon::new(Glyph::Info).size(px(14.)))
                            .text_color(cx.theme().muted_foreground)
                            .accessibility_label(crate::i18n::text("About saved filters"))
                            .tooltip(crate::i18n::text("Save namespaces, search, labels, resource filters and sorting for this cluster and resource type."))))
                    .when(!empty, |menu| menu.child(div().px_3().pb_1()
                        .child(toolbar::picker_search("saved-filter-search", &input, cx))))
                    .when(rows.is_empty(), |menu| menu.child(v_flex()
                        .id(if empty { "saved-filter-empty" } else { "saved-filter-no-results" })
                        .test_support().flex_shrink_0().items_center().justify_center().gap_2()
                        .px_3().py_4()
                        .child(div().size(px(32.)).flex().items_center().justify_center()
                            .rounded_lg().bg(cx.theme().muted)
                            .child(Icon::new(if empty { Glyph::Bookmark } else { Glyph::Search })
                                .size(px(17.)).text_color(cx.theme().muted_foreground)))
                        .child(v_flex().items_center().gap_1()
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM)
                                .child(crate::i18n::text(if empty { "No saved filters yet" } else { "No filters found" })))
                            .child(div().text_xs().text_color(cx.theme().muted_foreground)
                                .child(crate::i18n::text(if empty { "Save the current view below." } else { "Try a different name." }))))))
                    .when(!rows.is_empty(), |menu| menu.child(div()
                        .id("saved-filter-options").min_h_0().max_h(px(232.))
                        .px_2().pb_2().overflow_y_scroll()
                        .child(v_flex().gap_0p5().children(rows))))
                    .child(v_flex().flex_shrink_0().gap_2().p_3()
                        .border_t_1().border_color(cx.theme().border)
                        .child(v_flex().gap_0p5()
                            .child(div().text_xs().font_weight(FontWeight::MEDIUM)
                                .child(crate::i18n::text("Save current view")))
                            .child(div().id("saved-filter-view-summary").test_support()
                                .w_full().truncate().text_xs()
                                .text_color(cx.theme().muted_foreground).child(summary.clone())))
                        .children(error.clone().map(|error| div()
                            .id("saved-filter-error").test_support().text_xs()
                            .text_color(cx.theme().tone(Tone::Critical))
                            .child(crate::copyable_text::copyable_text("saved-filter-error-text", error))))
                        .child(h_flex().gap_2().items_center()
                            .child(div().flex_1().min_w_0()
                                .child(Input::new(&name).id("saved-filter-name").small()))
                            .child(Button::new("save-resource-filter").small().primary()
                                .label(crate::i18n::text(if replacing { "Replace" } else { "Save" }))
                                .accessibility_label(crate::i18n::text(if replacing { "Replace saved filter" } else { "Save current view" }))
                                .disabled(!can_save)
                                .on_click(move |_, window, cx| {
                                    let _ = saving.update(cx, |view, cx| view.save_filter_preset(window, cx));
                                }))))
            })
    }
}

fn filter_summary(preset: &FilterPreset, namespaced: bool) -> String {
    let scope = if !namespaced {
        "Cluster-wide".to_owned()
    } else {
        match preset.namespaces.len() {
            0 => ALL_NAMESPACES.to_owned(),
            1 => preset.namespaces.iter().next().cloned().unwrap_or_default(),
            count => format!("{count} namespaces"),
        }
    };
    let mut parts = vec![scope];
    if !preset.search.is_empty() {
        parts.push(format!("Search: {}", preset.search));
    }
    if !preset.labels.is_empty() {
        parts.push(format!("Labels: {}", preset.labels));
    }
    parts.extend(
        preset
            .fields
            .iter()
            .map(|(field, value)| format!("{}: {value}", field.label())),
    );
    if let Some(sort) = &preset.sort {
        parts.push(
            if sort.descending {
                "Sorted descending"
            } else {
                "Sorted ascending"
            }
            .into(),
        );
    }
    parts.join(" · ")
}

#[cfg(all(test, feature = "ui-tests"))]
mod integration_tests;
