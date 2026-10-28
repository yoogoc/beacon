//! Enter in a menu's search field confirms the first matching data choice.
use super::*;

impl ClusterView {
    pub(super) fn confirm_picker_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.picker_search.read(cx).value();
        if query.trim().is_empty() {
            return;
        }
        let search = OptionSearch::new(&query);
        if self.namespace_menu_open {
            // The fixed "All namespaces" entry is not a search result. Use
            // the same single-selection behavior as clicking a namespace name.
            let first = self
                .namespace_names
                .iter()
                .find(|name| search.matches(name))
                .cloned();
            if let Some(name) = first {
                self.set_namespace(Some(name.to_string()), window, cx);
                self.namespace_menu_open = false;
                self.row_search.read(cx).focus_handle(cx).focus(window, cx);
            }
        } else if let Some(field) = self.filter_menu_open {
            let first = self
                .field_picker_values(field, cx)
                .into_iter()
                .find(|value| search.matches(value));
            if let Some(value) = first {
                self.table.update(cx, |state, cx| {
                    if state.delegate_mut().set_field_filter(field, Some(value)) {
                        state.scroll_to_row(0, cx);
                        cx.notify();
                    }
                });
                self.filter_menu_open = None;
                self.row_search.read(cx).focus_handle(cx).focus(window, cx);
            }
        } else if self.label_menu_open {
            let first = self
                .table
                .read(cx)
                .delegate()
                .label_values()
                .into_iter()
                .find(|(key, value)| search.matches(&format!("{key}={value}")));
            if let Some((key, value)) = first {
                let selected = self
                    .table
                    .read(cx)
                    .delegate()
                    .label_selector()
                    .has_equality(&key, &value);
                if !selected {
                    self.toggle_label(&key, &value, window, cx);
                }
            }
        } else {
            self.confirm_column_search(&search, cx);
        }
        cx.notify();
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests;
