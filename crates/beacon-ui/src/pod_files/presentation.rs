//! Presentation for the container file manager; operations stay in FilesView.
use super::*;
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::Icon;
use gpui_kit::component::input::Textarea;
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::select::Select;
use gpui_kit::component::tooltip::Tooltip;

fn action(id: &'static str, icon: IconName, label: &'static str) -> Button {
    Button::new(id)
        .small()
        .ghost()
        .icon(Icon::new(icon).size(px(15.)))
        .accessibility_label(label)
        .tooltip(crate::i18n::text(label))
}

fn separator(cx: &App) -> Div {
    div().w(px(1.)).h(px(18.)).mx_1().bg(cx.theme().border)
}

fn file_icon(entry: &Entry) -> IconName {
    if entry.link.is_some() {
        IconName::Link
    } else if entry.directory() {
        IconName::Folder
    } else if !entry.regular() {
        IconName::File
    } else {
        match entry.name().rsplit('.').next().unwrap_or("") {
            "json" | "yaml" | "yml" | "toml" | "xml" | "sh" | "py" | "js" | "rs" => {
                IconName::FileCode
            }
            "gz" | "zip" | "tar" | "tgz" | "xz" => IconName::FileArchive,
            "txt" | "log" | "md" | "conf" | "ini" => IconName::FileText,
            _ => IconName::File,
        }
    }
}

fn empty_state(icon: IconName, title: &str, hint: &str, cx: &App) -> AnyElement {
    v_flex()
        .id("files-empty-state")
        .test_support()
        .size_full()
        .justify_center()
        .items_center()
        .gap_2()
        .p_6()
        .text_center()
        .child(
            div()
                .p_3()
                .rounded(px(12.))
                .bg(cx.theme().tokens.muted)
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(icon).size(px(24.))),
        )
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .child(crate::i18n::text(title)),
        )
        .child(
            div()
                .max_w(px(300.))
                .text_color(cx.theme().muted_foreground)
                .child(hint.to_owned()),
        )
        .into_any_element()
}

impl FilesView {
    fn close_preview(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.dirty(cx) {
            return;
        }
        self.preview = None;
        self.editor = None;
        self._editor_changes = None;
        cx.notify();
    }

    fn render_operations(&self, cx: &mut Context<Self>) -> AnyElement {
        let targets = self.targets();
        let writable =
            self.listing.as_ref().is_some_and(|l| l.writable) && !self.busy && !self.dirty(cx);
        let may_modify = writable && !targets.is_empty() && targets.iter().all(|e| e.writable);
        let may_rename = may_modify && targets.len() == 1;
        let may_download = !self.busy && !targets.is_empty();
        if self.width.get() < px(520.) {
            let upload = cx.entity().downgrade();
            let mkdir = upload.clone();
            let download = upload.clone();
            let rename = upload.clone();
            let delete = upload.clone();
            return action("files-more", IconName::Ellipsis, "File operations")
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                    menu.item(
                        PopupMenuItem::new(crate::i18n::text("Upload file…"))
                            .icon(Icon::new(IconName::Upload))
                            .disabled(!writable)
                            .on_click({
                                let upload = upload.clone();
                                move |_, w, cx| {
                                    let _ = upload.update(cx, |v, cx| v.upload(w, cx));
                                }
                            }),
                    )
                    .item(
                        PopupMenuItem::new(crate::i18n::text("New folder"))
                            .icon(Icon::new(IconName::FolderPlus))
                            .disabled(!writable)
                            .on_click({
                                let mkdir = mkdir.clone();
                                move |_, w, cx| {
                                    let _ = mkdir.update(cx, |v, cx| v.mkdir(w, cx));
                                }
                            }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(crate::i18n::text("Download selection…"))
                            .icon(Icon::new(IconName::Download))
                            .disabled(!may_download)
                            .on_click({
                                let download = download.clone();
                                move |_, w, cx| {
                                    let _ = download.update(cx, |v, cx| v.download(w, cx));
                                }
                            }),
                    )
                    .item(
                        PopupMenuItem::new(crate::i18n::text("Rename selection"))
                            .icon(Icon::new(IconName::Pencil))
                            .disabled(!may_rename)
                            .on_click({
                                let rename = rename.clone();
                                move |_, w, cx| {
                                    let _ = rename.update(cx, |v, cx| v.rename(w, cx));
                                }
                            }),
                    )
                    .item(
                        PopupMenuItem::new(crate::i18n::text("Delete selection…"))
                            .icon(Icon::new(IconName::Trash))
                            .disabled(!may_modify)
                            .on_click({
                                let delete = delete.clone();
                                move |_, w, cx| {
                                    let _ = delete.update(cx, |v, cx| v.delete(w, cx));
                                }
                            }),
                    )
                })
                .into_any_element();
        }
        let text_actions = self.width.get() >= px(900.);
        h_flex()
            .gap_1()
            .flex_shrink_0()
            .child(separator(cx))
            .child(
                action("files-upload", IconName::Upload, "Upload file…")
                    .outline()
                    .when(text_actions, |b| b.label(crate::i18n::text("Upload")))
                    .disabled(!writable)
                    .on_click(cx.listener(|v, _, w, cx| v.upload(w, cx))),
            )
            .child(
                action("files-mkdir", IconName::FolderPlus, "New folder")
                    .outline()
                    .when(text_actions, |b| b.label(crate::i18n::text("New folder")))
                    .disabled(!writable)
                    .on_click(cx.listener(|v, _, w, cx| v.mkdir(w, cx))),
            )
            .child(separator(cx))
            .child(
                action("files-download", IconName::Download, "Download selection…")
                    .disabled(!may_download)
                    .on_click(cx.listener(|v, _, w, cx| v.download(w, cx))),
            )
            .child(
                action("files-rename", IconName::Pencil, "Rename selection")
                    .disabled(!may_rename)
                    .on_click(cx.listener(|v, _, w, cx| v.rename(w, cx))),
            )
            .child(
                action("files-delete", IconName::Trash, "Delete selection…")
                    .when(may_modify, |b| {
                        b.icon(
                            Icon::new(IconName::Trash)
                                .size(px(15.))
                                .text_color(cx.theme().danger),
                        )
                    })
                    .disabled(!may_modify)
                    .on_click(cx.listener(|v, _, w, cx| v.delete(w, cx))),
            )
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let listing = self.listing.clone();
        let visible = self.visible.clone();
        let count = visible.len();
        let total = listing.as_ref().map_or(0, |l| l.entries.len());
        let show_mode = self.list_width.get() >= px(470.);
        let show_modified = self.list_width.get() >= px(650.);
        let this = cx.entity().downgrade();
        let selected = self.selected.clone();
        let busy = self.busy;
        let focused = self.preview.as_ref().map(|p| p.entry.path.clone());
        let all_checked = count > 0
            && listing.as_ref().is_some_and(|listing| {
                visible
                    .iter()
                    .all(|i| self.selected.contains(&listing.entries[*i].path))
            });
        let list = uniform_list("pod-file-list", count, move |range, _, cx| {
            range
                .map(|index| {
                    let entry = &listing.as_ref().unwrap().entries[visible[index]];
                    let key = entry.path.clone();
                    let checked = selected.contains(&key);
                    let active = focused.as_ref() == Some(&key);
                    let open = this.clone();
                    let pick = this.clone();
                    let target = entry.clone();
                    let menu = this.clone();
                    let menu_entry = entry.clone();
                    let hover = format!(
                        "{}\n{} · {} · mode {} · owner {}\nModified {}",
                        entry.path,
                        entry.kind,
                        size(entry.size),
                        entry.permissions,
                        entry.owner,
                        modified(entry.modified)
                    );
                    h_flex()
                        .id(SharedString::from(format!("file-row-{index}")))
                        .test_support()
                        .h(px(38.))
                        .w_full()
                        .gap_3()
                        .px_3()
                        .border_b_1()
                        .border_color(cx.theme().table_row_border)
                        .cursor_pointer()
                        .tooltip(move |w, cx| Tooltip::new(hover.clone()).build(w, cx))
                        .when(checked || active, |row| {
                            row.bg(cx.theme().tokens.table_active)
                        })
                        .hover(|row| {
                            if checked || active {
                                row
                            } else {
                                row.bg(cx.theme().tokens.table_hover)
                            }
                        })
                        .child(
                            checkbox(SharedString::from(format!("select-file-{index}")))
                                .checked(checked)
                                .disabled(busy)
                                .accessibility_label(format!("Select {}", entry.name()))
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    let _ = pick.update(cx, |view, cx| {
                                        if !view.selected.insert(key.clone()) {
                                            view.selected.remove(&key);
                                        }
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            h_flex()
                                .id(SharedString::from(format!("open-file-{index}")))
                                .role(Role::Button)
                                .aria_label(format!("Open {}", entry.name()))
                                .test_support()
                                .focusable()
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .gap_2()
                                .child(Icon::new(file_icon(entry)).size(px(16.)).text_color(
                                    if entry.directory() {
                                        cx.theme().foreground
                                    } else {
                                        cx.theme().muted_foreground
                                    },
                                ))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_ellipsis()
                                        .child(entry.name().to_owned()),
                                )
                                .when(!entry.writable, |name| {
                                    name.child(
                                        Icon::new(IconName::Lock)
                                            .size(px(12.))
                                            .text_color(cx.theme().muted_foreground),
                                    )
                                })
                                .on_key_down({
                                    let open = open.clone();
                                    let target = target.clone();
                                    move |event, w, cx| {
                                        if matches!(event.keystroke.key.as_str(), "enter" | "space")
                                        {
                                            cx.stop_propagation();
                                            let _ = open.update(cx, |v, cx| {
                                                v.open_entry(target.clone(), w, cx)
                                            });
                                        }
                                    }
                                }),
                        )
                        .child(
                            div()
                                .w(px(76.))
                                .flex_shrink_0()
                                .text_right()
                                .text_color(cx.theme().muted_foreground)
                                .child(if entry.directory() {
                                    "—".into()
                                } else {
                                    size(entry.size)
                                }),
                        )
                        .when(show_modified, |row| {
                            row.child(
                                div()
                                    .w(px(138.))
                                    .flex_shrink_0()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        beacon_columns::Timestamp::from_second(entry.modified)
                                            .map(|t| t.strftime("%Y-%m-%d %H:%M").to_string())
                                            .unwrap_or_else(|_| "—".into()),
                                    ),
                            )
                        })
                        .when(show_mode, |row| {
                            row.child(
                                div()
                                    .w(px(68.))
                                    .flex_shrink_0()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_size(cx.theme().mono_font_size)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(entry.permissions.clone()),
                            )
                        })
                        .on_click(move |_, window, cx| {
                            let _ = open
                                .update(cx, |view, cx| view.open_entry(target.clone(), window, cx));
                        })
                        .context_menu(move |popup, _, cx| {
                            let open = menu.clone();
                            let target = menu_entry.clone();
                            let copy = menu_entry.path.clone();
                            let download = menu.clone();
                            let delete = menu.clone();
                            let for_download = menu_entry.clone();
                            let for_delete = menu_entry.clone();
                            let writable = menu.upgrade().is_some_and(|v| {
                                v.read(cx).listing.as_ref().is_some_and(|l| l.writable)
                            }) && menu_entry.writable;
                            popup
                                .item(PopupMenuItem::new(crate::i18n::text("Open")).on_click(
                                    move |_, window, cx| {
                                        let _ = open.update(cx, |v, cx| {
                                            v.open_entry(target.clone(), window, cx)
                                        });
                                    },
                                ))
                                .item(PopupMenuItem::new(crate::i18n::text("Download…")).on_click(
                                    move |_, window, cx| {
                                        let _ = download.update(cx, |v, cx| {
                                            v.selected =
                                                BTreeSet::from([for_download.path.clone()]);
                                            v.download(window, cx);
                                        });
                                    },
                                ))
                                .item(crate::copyable_text::copy_item("Copy path", copy))
                                .separator()
                                .item(
                                    PopupMenuItem::new(crate::i18n::text("Delete…"))
                                        .disabled(!writable)
                                        .on_click(move |_, window, cx| {
                                            let _ = delete.update(cx, |v, cx| {
                                                if !v.selected.contains(&for_delete.path) {
                                                    v.selected =
                                                        BTreeSet::from([for_delete.path.clone()]);
                                                }
                                                v.delete(window, cx);
                                            });
                                        }),
                                )
                        })
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        })
        .size_full();
        let width = self.list_width.clone();
        let entity = cx.entity_id();
        v_flex()
            .id("files-list-pane")
            .test_support()
            .relative()
            .size_full()
            .min_size_0()
            .on_prepaint(move |bounds, _, cx| {
                if width.replace(bounds.size.width) != bounds.size.width {
                    cx.notify(entity);
                }
            })
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .flex_shrink_0()
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.search)
                                .id("files-search")
                                .small()
                                .cleanable(true)
                                .prefix(Icon::new(IconName::Search).size(px(14.)))
                                .text_size(cx.theme().font_size * 0.8125),
                        ),
                    )
                    .when(!self.selected.is_empty(), |bar| {
                        bar.child(
                            action("files-clear-selection", IconName::X, "Clear selection")
                                .on_click(cx.listener(|v, _, _, cx| {
                                    v.selected.clear();
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(
                h_flex()
                    .id("files-column-header")
                    .test_support()
                    .w_full()
                    .h(px(32.))
                    .px_3()
                    .gap_3()
                    .flex_shrink_0()
                    .bg(cx.theme().tokens.table_head)
                    .border_y_1()
                    .border_color(cx.theme().table_row_border)
                    .text_color(cx.theme().table_head_foreground)
                    .child(
                        checkbox("files-select-all")
                            .checked(all_checked)
                            .disabled(self.busy || count == 0)
                            .accessibility_label(crate::i18n::text("Select all filtered files"))
                            .on_click(cx.listener(|v, checked, _, cx| {
                                if let Some(listing) = &v.listing {
                                    for index in v.visible.iter() {
                                        let path = &listing.entries[*index].path;
                                        if *checked {
                                            v.selected.insert(path.clone());
                                        } else {
                                            v.selected.remove(path);
                                        }
                                    }
                                }
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1().min_w_0().child(crate::i18n::text("Name")))
                    .child(
                        div()
                            .w(px(76.))
                            .flex_shrink_0()
                            .text_right()
                            .child(crate::i18n::text("Size")),
                    )
                    .when(show_modified, |row| {
                        row.child(
                            div()
                                .w(px(138.))
                                .flex_shrink_0()
                                .child(crate::i18n::text("Modified (UTC)")),
                        )
                    })
                    .when(show_mode, |row| {
                        row.child(
                            div()
                                .w(px(68.))
                                .flex_shrink_0()
                                .child(crate::i18n::text("Mode")),
                        )
                    }),
            )
            .child(div().flex_1().min_size_0().child(if count > 0 {
                list.into_any_element()
            } else if self.busy {
                empty_state(
                    IconName::LoaderCircle,
                    "Loading files",
                    "Reading the container filesystem…",
                    cx,
                )
            } else if self.listing.is_none() {
                empty_state(
                    IconName::Folder,
                    "Container filesystem",
                    "Choose a running container to browse its files.",
                    cx,
                )
            } else if total == 0 {
                empty_state(
                    IconName::FolderOpen,
                    "This folder is empty",
                    "There are no files in this directory.",
                    cx,
                )
            } else {
                empty_state(
                    IconName::Search,
                    "No matching files",
                    "Try a different name or clear the search.",
                    cx,
                )
            }))
            .child(
                h_flex()
                    .w_full()
                    .h(px(30.))
                    .px_3()
                    .gap_2()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(cx.theme().table_row_border)
                    .text_size(cx.theme().font_size * 0.75)
                    .text_color(cx.theme().table_foot_foreground)
                    .child(if count == total {
                        format!("{total} items")
                    } else {
                        format!("{count} of {total} items")
                    })
                    .when(!self.selected.is_empty(), |bar| {
                        bar.child(format!("· {} selected", self.selected.len()))
                    })
                    .child(div().flex_1())
                    .when(self.listing.as_ref().is_some_and(|l| !l.writable), |bar| {
                        bar.child(Icon::new(IconName::Lock).size(px(12.)))
                            .child(crate::i18n::text("Read-only"))
                    }),
            )
            .into_any_element()
    }

    fn render_preview(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(preview) = &self.preview else {
            return div().into_any_element();
        };
        let entry = &preview.entry;
        let editing = self.editor.is_some();
        let dirty = self.dirty(cx);
        let mut body = v_flex()
            .id("files-preview-pane")
            .test_support()
            .size_full()
            .min_size_0()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_3()
                    .gap_2()
                    .flex_shrink_0()
                    .child(
                        Icon::new(file_icon(entry))
                            .size(px(20.))
                            .text_color(cx.theme().foreground),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(copyable_text("file-name", entry.name().to_owned())),
                            )
                            .child(
                                div()
                                    .text_size(cx.theme().font_size * 0.75)
                                    .text_color(cx.theme().muted_foreground)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(copyable_text("file-path", entry.path.clone())),
                            ),
                    )
                    .child(
                        action("files-close-preview", IconName::X, "Close preview")
                            .disabled(self.busy || dirty)
                            .on_click(cx.listener(|v, _, _, cx| v.close_preview(cx))),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .pb_2()
                    .gap_2()
                    .flex_shrink_0()
                    .when(!editing, |bar| {
                        bar.child(
                            action("edit-file", IconName::Pencil, "Edit file")
                                .outline()
                                .label(crate::i18n::text("Edit file"))
                                .disabled(!preview.editable() || self.busy)
                                .on_click(cx.listener(|v, _, w, cx| v.edit(w, cx))),
                        )
                    })
                    .when(editing, |bar| {
                        bar.child(
                            action("save-file", IconName::Check, "Review and save changes")
                                .primary()
                                .label(crate::i18n::text("Review & save"))
                                .disabled(self.busy || !dirty)
                                .on_click(cx.listener(|v, _, w, cx| v.save(w, cx))),
                        )
                        .child(
                            action("discard-file", IconName::Undo2, "Discard changes")
                                .disabled(self.busy)
                                .on_click(cx.listener(|v, _, _, cx| {
                                    v.editor = None;
                                    v._editor_changes = None;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        action("download-preview", IconName::Download, "Download file…")
                            .when(!editing, |b| b.label(crate::i18n::text("Download")))
                            .disabled(self.busy)
                            .on_click(cx.listener(|v, _, w, cx| {
                                v.selected.clear();
                                v.download(w, cx);
                            })),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .flex_shrink_0()
                    .border_y_1()
                    .border_color(cx.theme().border)
                    .text_size(cx.theme().font_size * 0.75)
                    .text_color(cx.theme().muted_foreground)
                    .child(if preview.text.is_some() {
                        "Text"
                    } else {
                        "Binary / special"
                    })
                    .child("·")
                    .child(size(entry.size))
                    .when(dirty, |bar| {
                        bar.child(
                            div()
                                .text_color(cx.theme().foreground)
                                .child(crate::i18n::text("· Unsaved")),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("files-details-toggle")
                            .xsmall()
                            .ghost()
                            .label(crate::i18n::text("Details"))
                            .icon(if self.details {
                                IconName::ChevronUp
                            } else {
                                IconName::ChevronDown
                            })
                            .on_click(cx.listener(|v, _, _, cx| {
                                v.details = !v.details;
                                cx.notify();
                            })),
                    ),
            )
            .when(self.details, |body| {
                body.child(
                    v_flex()
                        .id("file-metadata")
                        .test_support()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .flex_shrink_0()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .children(
                            [
                                ("Type", entry.kind.clone()),
                                ("Permissions", entry.permissions.clone()),
                                ("Owner (UID:GID)", entry.owner.clone()),
                                ("Modified", modified(entry.modified)),
                            ]
                            .into_iter()
                            .enumerate()
                            .map(|(i, (label, value))| {
                                h_flex()
                                    .items_start()
                                    .w_full()
                                    .gap_3()
                                    .child(
                                        div()
                                            .w(px(112.))
                                            .flex_shrink_0()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(label),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .child(copyable_text(("file-property", i), value)),
                                    )
                            }),
                        ),
                )
            });
        for (id, message) in [
            (!entry.writable).then_some(("file-readonly", "Read-only file. Editing is disabled.")),
            preview.truncated.then_some((
                "file-truncated",
                "Preview limited to 256 KiB. Download to view the complete file.",
            )),
        ]
        .into_iter()
        .flatten()
        {
            body = body.child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .items_start()
                    .flex_shrink_0()
                    .bg(cx.theme().tokens.muted)
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(IconName::Info).size(px(14.)))
                    .child(div().flex_1().min_w_0().child(copyable_text(id, message))),
            );
        }
        if let Some(link) = &entry.link {
            let target = if link.starts_with('/') {
                files::path(link)
            } else {
                files::path(&format!(
                    "{}/{}",
                    entry.path.rsplit_once('/').map_or("/", |p| p.0),
                    link
                ))
            };
            body = body.child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .flex_shrink_0()
                    .child(Icon::new(IconName::Link).size(px(14.)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(copyable_text("symlink-target", format!("Target: {link}"))),
                    )
                    .child(
                        action(
                            "open-link-target",
                            IconName::ArrowRight,
                            "Open symlink target",
                        )
                        .disabled(self.busy || target.is_err())
                        .on_click(cx.listener(move |v, _, w, cx| {
                            if let Ok(path) = &target {
                                v.start(Work::Preview(path.clone()), w, cx);
                            }
                        })),
                    ),
            );
        }
        body.child(if let Some(editor) = &self.editor {
            div().flex_1().min_size_0().p_2().child(
                Textarea::new(editor).h(relative(1.)).readonly(self.busy)
                    .font_family(cx.theme().mono_font_family.clone()).text_size(cx.theme().mono_font_size),
            ).into_any_element()
        } else if let Some(text) = &preview.text {
            v_flex().flex_1().min_size_0()
                .bg(cx.theme().background)
                .child(h_flex().w_full().h(px(28.)).px_3().flex_shrink_0()
                    .text_size(cx.theme().font_size * 0.6875).text_color(cx.theme().muted_foreground)
                    .child(crate::i18n::text("Text preview")).child(div().flex_1()).child("UTF-8"))
                .child(div().id("file-preview-scroll").test_support().flex_1().min_size_0()
                    .overflow_scroll().p_3().pt_1().font_family(cx.theme().mono_font_family.clone()).text_size(cx.theme().mono_font_size)
                    .line_height(relative(1.6)).whitespace_nowrap()
                    .child(copyable_text("file-content", text.clone())))
                .into_any_element()
        } else {
            v_flex().flex_1().min_size_0().justify_center().items_center().gap_3().p_6().text_center()
                .child(Icon::new(file_icon(entry)).size(px(32.)).text_color(cx.theme().muted_foreground))
                .child(copyable_text("file-binary", "Text preview unavailable"))
                .child(div().max_w(px(280.)).text_color(cx.theme().muted_foreground)
                    .child(crate::i18n::text("Download this file to open it locally. Its original bytes are preserved.")))
                .into_any_element()
        }).into_any_element()
    }
}

impl Render for FilesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dirty = self.dirty(cx);
        let directory = self
            .listing
            .as_ref()
            .map_or_else(|| "/".into(), |l| l.path.clone());
        let parent = files::path(&format!("{directory}/.."));
        let breadcrumbs = directory
            .split('/')
            .filter(|s| !s.is_empty())
            .scan(String::new(), |path, part| {
                path.push('/');
                path.push_str(part);
                Some((part.to_string(), path.clone()))
            })
            .collect::<Vec<_>>();
        let width = self.width.clone();
        let entity = cx.entity_id();
        let content = if self.preview.is_none() {
            self.render_list(cx)
        } else if self.width.get() < px(680.) {
            // A split workspace pane still gives the file content its full width.
            self.render_preview(cx)
        } else {
            h_resizable("files-split")
                .with_state(&self.split)
                .child(
                    resizable_panel()
                        .size(self.width.get() * 0.55)
                        .size_range(px(300.)..px(10000.))
                        .child(self.render_list(cx)),
                )
                .child(
                    resizable_panel().size_range(px(300.)..px(10000.)).child(
                        div()
                            .size_full()
                            .border_l_1()
                            .border_color(cx.theme().border)
                            .child(self.render_preview(cx)),
                    ),
                )
                .into_any_element()
        };
        v_flex()
            .id("pod-files-view")
            .test_support()
            .relative()
            .size_full()
            .min_size_0()
            .bg(cx.theme().background)
            .text_sm()
            .font_family(cx.theme().font_family.clone())
            .text_color(cx.theme().foreground)
            .on_prepaint(move |bounds, _, cx| {
                if width.replace(bounds.size.width) != bounds.size.width {
                    cx.notify(entity);
                }
            })
            .child(
                h_flex()
                    .w_full()
                    .px_4()
                    .py_3()
                    .gap_3()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .when(self.width.get() >= px(520.), |bar| {
                        bar.child(
                            div()
                                .p_2()
                                .rounded(px(8.))
                                .bg(cx.theme().muted)
                                .text_color(cx.theme().foreground)
                                .child(Icon::new(IconName::FolderOpen).size(px(20.))),
                        )
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(crate::i18n::text("Pod files")),
                            )
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_color(cx.theme().muted_foreground)
                                    .text_size(cx.theme().font_size * 0.75)
                                    .child(copyable_text(
                                        "files-pod",
                                        format!(
                                            "{} / {}",
                                            self.object.metadata.namespace.as_deref().unwrap_or(""),
                                            self.object.metadata.name.as_deref().unwrap_or("")
                                        ),
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .w(px(if self.width.get() < px(520.) {
                                160.
                            } else {
                                220.
                            }))
                            .flex_shrink_0()
                            .child(
                                Select::new(&self.picker)
                                    .small()
                                    .search_placeholder("Search running containers")
                                    .title_prefix("Container: ")
                                    .disabled(self.containers.is_empty() || self.busy || dirty),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .id("files-toolbar")
                    .role(Role::Toolbar)
                    .aria_label("File operations")
                    .test_support()
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .flex_shrink_0()
                    .child(
                        action("files-up", IconName::ArrowUp, "Parent folder")
                            .disabled(self.busy || dirty || directory == "/")
                            .on_click(cx.listener(move |v, _, w, cx| {
                                if let Ok(path) = &parent {
                                    v.load(path.clone(), w, cx);
                                }
                            })),
                    )
                    .child(
                        div().flex_1().min_w(px(80.)).child(
                            Input::new(&self.path)
                                .id("files-path-input")
                                .small()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_size(cx.theme().mono_font_size)
                                .disabled(self.busy || dirty),
                        ),
                    )
                    .when(self.width.get() >= px(520.), |bar| {
                        bar.child(
                            action("files-go", IconName::ArrowRight, "Open path (Enter)")
                                .disabled(self.busy || dirty)
                                .on_click(cx.listener(|v, _, w, cx| {
                                    v.load(v.path.read(cx).value().to_string(), w, cx);
                                })),
                        )
                    })
                    .child(
                        action("files-refresh", IconName::RefreshCw, "Refresh folder")
                            .disabled(self.busy || dirty)
                            .on_click(cx.listener(|v, _, w, cx| {
                                v.load(
                                    v.listing
                                        .as_ref()
                                        .map_or_else(|| "/".into(), |l| l.path.clone()),
                                    w,
                                    cx,
                                );
                            })),
                    )
                    .child(self.render_operations(cx)),
            )
            .child(
                h_flex()
                    .id("files-breadcrumbs")
                    .w_full()
                    .min_w_0()
                    .px_3()
                    .pb_2()
                    .gap_1()
                    .flex_shrink_0()
                    .child(
                        div()
                            .id("files-breadcrumb-scroll")
                            .flex_1()
                            .min_w_0()
                            .overflow_x_scroll()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Button::new("files-root")
                                            .xsmall()
                                            .ghost()
                                            .icon(IconName::HardDrive)
                                            .label("/")
                                            .disabled(self.busy || dirty)
                                            .on_click(
                                                cx.listener(|v, _, w, cx| {
                                                    v.load("/".into(), w, cx)
                                                }),
                                            ),
                                    )
                                    .children(breadcrumbs.into_iter().enumerate().map(
                                        |(i, (name, path))| {
                                            h_flex()
                                                .gap_1()
                                                .flex_shrink_0()
                                                .child(
                                                    Icon::new(IconName::ChevronRight)
                                                        .size(px(12.))
                                                        .text_color(cx.theme().muted_foreground),
                                                )
                                                .child(
                                                    Button::new(("breadcrumb", i))
                                                        .xsmall()
                                                        .ghost()
                                                        .label(name)
                                                        .disabled(self.busy || dirty)
                                                        .on_click(cx.listener(
                                                            move |v, _, w, cx| {
                                                                v.load(path.clone(), w, cx)
                                                            },
                                                        )),
                                                )
                                        },
                                    )),
                            ),
                    )
                    .when(!self.selected.is_empty(), |bar| {
                        bar.child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded(px(4.))
                                .bg(cx.theme().tokens.muted)
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("{} selected", self.selected.len())),
                        )
                    }),
            )
            .when(self.busy, |body| {
                body.child(
                    h_flex()
                        .w_full()
                        .px_3()
                        .py_1()
                        .gap_2()
                        .flex_shrink_0()
                        .bg(cx.theme().muted)
                        .child(
                            Icon::new(IconName::LoaderCircle)
                                .size(px(14.))
                                .text_color(cx.theme().foreground),
                        )
                        .child(copyable_text(
                            "file-progress",
                            if self.transfer {
                                format!(
                                    "Transferring · {}",
                                    size(self.progress.load(Ordering::Relaxed))
                                )
                            } else {
                                "Working…".into()
                            },
                        ))
                        .child(div().flex_1())
                        .child(
                            Button::new("files-cancel")
                                .xsmall()
                                .ghost()
                                .label(crate::i18n::text("Cancel"))
                                .on_click(cx.listener(|v, _, _, cx| v.cancel(cx))),
                        ),
                )
            })
            .when_some(self.message.clone(), |body, message| {
                body.child(
                    h_flex()
                        .w_full()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .items_start()
                        .flex_shrink_0()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().tokens.muted)
                        .child(
                            Icon::new(IconName::Info)
                                .size(px(14.))
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            div()
                                .id("files-message-scroll")
                                .flex_1()
                                .min_w_0()
                                .max_h(px(120.))
                                .overflow_y_scroll()
                                .child(copyable_text("files-message", message)),
                        )
                        .child(
                            action("files-dismiss-message", IconName::X, "Dismiss message")
                                .on_click(cx.listener(|v, _, _, cx| {
                                    v.message = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_size_0()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(content),
            )
    }
}
