//! Local customization menus for the current resource list.
use super::*;
use crate::table_preferences::{ColumnLayout, ResourcePreferences};

pub(super) struct ListSettings {
    columns_open: bool,
    presets_open: bool,
    name: Entity<InputState>,
    pub(super) error: Option<String>,
}

impl ListSettings {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ClusterView>) -> Self {
        Self {
            columns_open: false,
            presets_open: false,
            error: None,
            name: cx.new(|cx| InputState::new(window, cx).placeholder("Name this filter")),
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

    fn reset_list_columns(&mut self, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            table.delegate_mut().apply_layout(Default::default());
            table.refresh(cx);
        });
        self.persist_layout(Default::default(), cx);
    }

    fn save_filter_preset(&mut self, cx: &mut Context<Self>) {
        let name = self.list_settings.name.read(cx).value().trim().to_owned();
        let preset = self
            .table
            .read(cx)
            .delegate()
            .filter_preset(self.scoped_to.clone());
        let mut preferences = self.resource_preferences(cx);
        preferences.filters.insert(name, preset);
        self.save_resource_preferences(preferences, cx);
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
        }
        cx.notify();
    }

    pub(super) fn listen_list_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let subscription = cx.subscribe_in(
            &self.list_settings.name,
            window,
            |view, _, event, _, cx| match event {
                InputEvent::PressEnter { .. } => view.save_filter_preset(cx),
                InputEvent::Change => {
                    view.list_settings.error = None;
                    cx.notify();
                }
                _ => {}
            },
        );
        self._subscriptions.push(subscription);
    }

    pub(super) fn render_column_picker(
        &self,
        layout: toolbar::Layout,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let choices = self.table.read(cx).delegate().column_choices();
        let search = OptionSearch::new(self.picker_search.read(cx).value().as_ref());
        let input = self.picker_search.clone();
        let weak = cx.entity().downgrade();
        let opening = weak.clone();
        Popover::new("resource-columns").p_1p5().open(self.list_settings.columns_open)
            .on_open_change(move |open, window, cx| {
                let _ = opening.update(cx, |view, cx| view.open_list_menu(true, *open, window, cx));
            })
            .trigger(Button::new("resource-columns-trigger").small().ghost()
                .icon(Icon::new(gpui_kit::assets::IconName::Columns3))
                .when(layout.text_actions(), |button| button.label("Columns"))
                .accessibility_label("Columns").tooltip("Choose visible columns"))
            .content(move |_, _, cx| {
                let reset = weak.clone();
                let rows: Vec<_> = choices.iter().filter(|(_, name, _, _)| search.matches(name)).map(|(id, name, visible, required)| {
                    let weak = weak.clone();
                    let id = id.clone();
                    checkbox(SharedString::from(format!("visible-column-{id}")))
                        .w_full().h(toolbar::picker_row_height(cx)).px_2()
                        .label(name.clone()).checked(*visible).disabled(*required)
                        .on_click(move |_, _, cx| { let _ = weak.update(cx, |view, cx| view.toggle_list_column(&id, cx)); })
                }).collect();
                v_flex().w(px(260.)).max_h(px(420.)).min_h_0().gap_2()
                    .child(div().px_2().pt_1().text_xs().text_color(cx.theme().muted_foreground).child("Visible columns"))
                    .child(toolbar::picker_search("resource-column-search", &input, cx))
                    .child(div().id("resource-column-options").min_h_0().overflow_y_scroll()
                        .child(v_flex().gap_0p5().children(rows)))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground)
                        .child("Drag headers to reorder. Resize column edges. Changes save automatically."))
                    .child(h_flex().child(Button::new("reset-resource-columns").small().ghost().label("Restore defaults")
                        .on_click(move |_, _, cx| { let _ = reset.update(cx, |view, cx| view.reset_list_columns(cx)); })))
            })
    }

    pub(super) fn render_saved_filters(
        &self,
        layout: toolbar::Layout,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let preferences = self.resource_preferences(cx);
        let search = OptionSearch::new(self.picker_search.read(cx).value().as_ref());
        let input = self.picker_search.clone();
        let name = self.list_settings.name.clone();
        let replacing = preferences
            .filters
            .contains_key(name.read(cx).value().trim());
        let weak = cx.entity().downgrade();
        let opening = weak.clone();
        Popover::new("resource-saved-filters").open(self.list_settings.presets_open)
            .on_open_change(move |open, window, cx| {
                let _ = opening.update(cx, |view, cx| view.open_list_menu(false, *open, window, cx));
            })
            .trigger(Button::new("resource-saved-filters-trigger").small().ghost()
                .icon(Icon::new(gpui_kit::assets::IconName::Bookmark))
                .when(layout.text_actions(), |button| button.label("Saved filters").dropdown_caret(true))
                .accessibility_label("Saved filters").tooltip("Save or apply resource filters"))
            .content(move |_, _, cx| {
                let saving = weak.clone();
                let rows: Vec<_> = preferences.filters.iter().filter(|(name, _)| search.matches(name)).map(|(name, preset)| {
                    let applying = weak.clone();
                    let removing = weak.clone();
                    let applied = name.clone();
                    let removed = name.clone();
                    h_flex().gap_2().items_center()
                        .child(Button::new(SharedString::from(format!("apply-filter-{name}"))).small().ghost()
                            .flex_1().label(name.clone()).tooltip(format!("Namespaces: {} · Search: {} · Labels: {}",
                                if preset.namespaces.is_empty() { ALL_NAMESPACES.to_owned() } else { preset.namespaces.iter().cloned().collect::<Vec<_>>().join(", ") },
                                preset.search, preset.labels))
                            .on_click(move |_, window, cx| { let _ = applying.update(cx, |view, cx| view.apply_filter_preset(&applied, window, cx)); }))
                        .child(Button::new(SharedString::from(format!("remove-filter-{name}"))).small().ghost()
                            .label("Remove").on_click(move |_, _, cx| { let _ = removing.update(cx, |view, cx| view.remove_filter_preset(&removed, cx)); }))
                }).collect();
                v_flex().w(px(360.)).max_h(px(460.)).min_h_0().gap_3()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Saved filters"))
                    .child(toolbar::picker_search("saved-filter-search", &input, cx))
                    .when(rows.is_empty(), |menu| menu.child(div().text_sm().text_color(cx.theme().muted_foreground).child("No matching saved filters")))
                    .child(div().id("saved-filter-options").min_h_0().overflow_y_scroll().child(v_flex().gap_1().children(rows)))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground)
                        .child("Save the current namespace, search, labels, resource filters and sorting for this cluster and resource type."))
                    .child(h_flex().gap_2().items_center()
                        .child(div().flex_1().min_w_0().child(Input::new(&name).small()))
                        .child(Button::new("save-resource-filter").small().primary()
                            .label(if replacing { "Replace" } else { "Save" })
                            .on_click(move |_, _, cx| { let _ = saving.update(cx, |view, cx| view.save_filter_preset(cx)); })))
            })
    }
}
