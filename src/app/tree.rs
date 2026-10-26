//! Left sidebar: the menu bar, the connection tree, drag-and-drop and the collapsed-sidebar overlay.
//! Split out of `app.rs`; these are `App` methods.

use super::*;

impl App {
    pub(super) fn ui_menu(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Import Windows RDP-Manager Backup or Database …").clicked() {
                        self.pick(Pick::Database);
                        ui.close_menu();
                    }
                    if ui.button("Import .rdp Files …").clicked() {
                        self.rdp_import = Some(RdpImportDialog { files: Vec::new(), folder: self.current_folder() });
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Export Database …").on_hover_text("Save all connections, folders and credentials to a file").clicked() {
                        self.export_confirm = true;
                        ui.close_menu();
                    }
                    if ui.button("Import Database (RC Manager export) …").clicked() {
                        self.pick(Pick::ImportDb);
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Edit", |ui| {
                    let parent = self.current_folder();
                    if ui.button("🗀 New Folder").clicked() {
                        actions.push(Action::NewFolder(parent));
                        ui.close_menu();
                    }
                    new_connection_menu(ui, parent, actions);
                    ui.separator();
                    if ui.button("🔑 Manage Credentials …").clicked() {
                        self.show_credentials = true;
                        ui.close_menu();
                    }
                });
                ui.menu_button("Options", |ui| {
                    ui.checkbox(&mut self.s.dark, "Dark Theme");
                    let embed_hint = if self.embedder.is_some() { "Sessions open as tabs inside this window" } else { "Needs X11 or XWayland" };
                    ui.checkbox(&mut self.s.embed_sessions, "Open Sessions in Tabs").on_hover_text(embed_hint);
                    let mut shown = !self.s.sidebar_collapsed;
                    if ui.checkbox(&mut shown, "Show Sidebar (Ctrl+B)").changed() {
                        self.s.sidebar_collapsed = !shown;
                    }
                    let mut log = !self.s.log_hidden;
                    if ui.checkbox(&mut log, "Show Log (Ctrl+L)").changed() {
                        self.set_log_hidden(!log);
                    }
                    ui.separator();
                    let has_master = self.store.has_master_password().unwrap_or(false);
                    if ui.add_enabled(has_master, egui::Button::new("🔒 Lock Now (Ctrl+Shift+L)")).on_disabled_hover_text("Set a master password first").clicked() {
                        self.lock_now("Locked.");
                        ui.close_menu();
                    }
                    if ui.button("Master Password …").clicked() {
                        self.master_dialog = Some(MasterDialog::default());
                        ui.close_menu();
                    }
                    if ui.button("External Programs …").clicked() {
                        self.show_tools = true;
                        ui.close_menu();
                    }
                    if ui.button("Re-detect External Programs").clicked() {
                        self.tools = Tools::detect();
                        self.tools.freerdp_config = Some(self.store.dir().join("freerdp"));
                        self.push(Level::Info, "External programs re-detected.");
                        ui.close_menu();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("Check for Updates …").clicked() {
                        self.start_update_check(true);
                        ui.close_menu();
                    }
                    ui.checkbox(&mut self.s.update_auto_check, "Check for Updates at Start");
                    if ui.button("Updater Proxy …").clicked() {
                        self.update_proxy_edit = Some(self.s.update_proxy.clone());
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("About RC Manager").clicked() {
                        self.show_about = true;
                        ui.close_menu();
                    }
                });
            });
        });

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.allocate_ui(egui::vec2(300.0, 56.0), |ui| theme::header(ui, "Remote Connection Manager"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Connect").clicked() {
                        actions.push(Action::QuickConnect);
                    }
                    let r = ui.add(egui::TextEdit::singleline(&mut self.quick_host).hint_text("host[:port]").desired_width(220.0));
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        actions.push(Action::QuickConnect);
                    }
                    egui::ComboBox::from_id_salt("quick_proto").selected_text(self.s.quick_protocol.label()).show_ui(ui, |ui| {
                        for p in [Protocol::Rdp, Protocol::Ssh, Protocol::Vnc, Protocol::Web] {
                            ui.selectable_value(&mut self.s.quick_protocol, p, p.label());
                        }
                    });
                    ui.label("Quick connect:");
                });
            });
        });
    }

    pub(super) fn connection_row(&self, ui: &mut egui::Ui, c: &Connection, label: String, actions: &mut Vec<Action>, dnd: &mut TreeDnd) {
        let selected = self.selection == Some(Sel::Connection(c.id));
        // Connected systems are marked in the accent colour, as in the original.
        let connected = self.sessions.values().any(|s| s.conn.id == c.id);
        let mut text = RichText::new(format!("{} {label}", c.protocol.icon()));
        if connected {
            text = text.color(theme::accent(ui.visuals().dark_mode)).strong();
        }
        // A stable id per connection keeps the drag attached while the tree re-renders.
        let r = ui.push_id(c.id, |ui| ui.selectable_label(selected, text)).inner;
        // Older imports stored the description as RTF; show it as plain text.
        let desc = legacy_import::rtf_to_text(&c.description);
        let r = if desc.trim().is_empty() { r.on_hover_text(&c.host) } else { r.on_hover_text(format!("{}\n\n{}", c.host, desc)) };
        let r = if dnd.enabled { r.interact(egui::Sense::click_and_drag()) } else { r };
        if dnd.enabled {
            if r.dragged() {
                dnd.dragged = Some(Sel::Connection(c.id));
            }
            if r.drag_stopped() {
                dnd.released = Some(Sel::Connection(c.id));
            }
            // Dropping onto a connection moves the dragged entry into that connection's folder.
            dnd.zones.push((r.rect, c.parent));
        }
        if r.clicked() {
            actions.push(Action::Select(Sel::Connection(c.id)));
        }
        if r.double_clicked() {
            actions.push(Action::Connect(c.id));
        }
        r.context_menu(|ui| {
            if ui.button(if connected { "⟳ Reconnect" } else { "▶ Connect" }).clicked() {
                actions.push(if connected { Action::Reconnect(c.id) } else { Action::Connect(c.id) });
                ui.close_menu();
            }
            if ui.button("✏ Properties …").clicked() {
                actions.push(Action::Edit(Sel::Connection(c.id)));
                ui.close_menu();
            }
            if ui.button("Duplicate").clicked() {
                actions.push(Action::Duplicate(c.id));
                ui.close_menu();
            }
            if ui.button("Check Reachability").clicked() {
                actions.push(Action::CheckPort(c.id));
                ui.close_menu();
            }
            if ui.add_enabled(!c.mac.trim().is_empty(), egui::Button::new("Wake-on-LAN")).clicked() {
                actions.push(Action::Wake(c.id));
                ui.close_menu();
            }
            ui.separator();
            if ui.button("🗑 Delete …").clicked() {
                actions.push(Action::AskDelete(Sel::Connection(c.id)));
                ui.close_menu();
            }
        });
    }

    pub(super) fn folder_tree(&self, ui: &mut egui::Ui, parent: Option<Uuid>, actions: &mut Vec<Action>, dnd: &mut TreeDnd) {
        for f in self.children_folders(parent) {
            let id = ui.make_persistent_id(("folder", f.id));
            let state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, f.expanded);
            let was_open = state.is_open();
            let selected = self.selection == Some(Sel::Folder(f.id));
            state
                .show_header(ui, |ui| {
                    let r = ui.push_id(f.id, |ui| ui.selectable_label(selected, format!("{} {}", if f.expanded { "🗁" } else { "🗀" }, f.name))).inner;
                    let r = if dnd.enabled { r.interact(egui::Sense::click_and_drag()) } else { r };
                    if dnd.enabled {
                        if r.dragged() {
                            dnd.dragged = Some(Sel::Folder(f.id));
                        }
                        if r.drag_stopped() {
                            dnd.released = Some(Sel::Folder(f.id));
                        }
                        // Dropping onto a folder header moves the dragged entry into that folder.
                        dnd.zones.push((r.rect, Some(f.id)));
                    }
                    if r.clicked() {
                        actions.push(Action::Select(Sel::Folder(f.id)));
                    }
                    r.context_menu(|ui| {
                        if ui.button("🗀 New Subfolder").clicked() {
                            actions.push(Action::NewFolder(Some(f.id)));
                            ui.close_menu();
                        }
                        new_connection_menu(ui, Some(f.id), actions);
                        ui.separator();
                        if ui.button("✏ Properties …").clicked() {
                            actions.push(Action::Edit(Sel::Folder(f.id)));
                            ui.close_menu();
                        }
                        if ui.button("🗑 Delete Folder …").clicked() {
                            actions.push(Action::AskDelete(Sel::Folder(f.id)));
                            ui.close_menu();
                        }
                    });
                })
                .body(|ui| self.folder_tree(ui, Some(f.id), actions, dnd));
            let now_open = egui::collapsing_header::CollapsingState::load(ui.ctx(), id).map(|s| s.is_open()).unwrap_or(was_open);
            if now_open != f.expanded {
                actions.push(Action::SetExpanded(f.id, now_open));
            }
        }
        for c in self.children_connections(parent) {
            self.connection_row(ui, c, c.name.clone(), actions, dnd);
        }
    }

    pub(super) fn ui_tree(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::B)) {
            self.s.sidebar_collapsed = !self.s.sidebar_collapsed;
            self.sidebar_peek = false;
        }
        if !self.s.sidebar_collapsed {
            egui::SidePanel::left("tree").resizable(true).default_width(320.0).min_width(220.0).show(ctx, |ui| {
                self.tree_contents(ui, actions, false);
            });
            return;
        }

        // Narrow strip with a vertical caption. Hovering it shows the sidebar as an overlay,
        // clicking it (or the pin in the overlay) docks the sidebar again.
        let strip = egui::SidePanel::left("tree_collapsed").resizable(false).exact_width(28.0).show(ctx, |ui| {
            let rect = ui.max_rect().expand2(egui::vec2(8.0, 0.0));
            let resp = ui.interact(rect, ui.id().with("expand"), egui::Sense::click());
            let visuals = ui.visuals();
            let active = resp.hovered() || self.sidebar_peek;
            let color = if active { visuals.strong_text_color() } else { visuals.text_color() };
            if active {
                ui.painter().rect_filled(rect, 0.0, visuals.widgets.hovered.weak_bg_fill);
            }
            let x = rect.center().x;
            ui.painter().text(egui::pos2(x, rect.top() + 16.0), Align2::CENTER_CENTER, "»", egui::FontId::proportional(18.0), color);
            let galley = ui.painter().layout_no_wrap("Connections".into(), egui::FontId::proportional(14.0), color);
            // Rotated 90° clockwise: the text runs top to bottom, its height extends to the left of `pos`.
            let pos = egui::pos2(x + galley.size().y / 2.0, rect.top() + 34.0);
            ui.painter().add(egui::epaint::TextShape::new(pos, galley, color).with_angle(std::f32::consts::FRAC_PI_2));
            if resp.clicked() {
                self.s.sidebar_collapsed = false;
                self.sidebar_peek = false;
            }
            if resp.hovered() {
                self.sidebar_peek = true;
            }
            rect
        });
        if !self.s.sidebar_collapsed || !self.sidebar_peek {
            return;
        }

        let strip_rect = strip.inner;
        let width = 320.0;
        let area = egui::Area::new(egui::Id::new("tree_peek"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(strip_rect.right(), strip_rect.top()))
            .show(ctx, |ui| {
                let frame = egui::Frame::window(ui.style()).rounding(0.0).inner_margin(egui::Margin::symmetric(8.0, 6.0));
                frame.show(ui, |ui| {
                    ui.set_width(width);
                    ui.set_height(strip_rect.height() - 18.0);
                    self.tree_contents(ui, actions, true);
                });
            });

        // Keep the overlay while the pointer is over the strip, the overlay itself or one of
        // its menus (context menu, combo box); hide it as soon as the pointer moves away.
        let overlay_rect = area.response.rect;
        let keep = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| {
            strip_rect.contains(p)
                || overlay_rect.contains(p)
                || ctx.layer_id_at(p).is_some_and(|l| l.order == egui::Order::Foreground && l.id != area.response.layer_id.id)
        });
        // A context menu does not register as a popup (any_popup_open only covers combo boxes and
        // the like), so check it separately – otherwise moving onto a host's context menu hid the
        // overlay and closed the menu with it, which looked like flicker.
        let menu_open = ctx.memory(|m| m.any_popup_open()) || ctx.is_context_menu_open();
        if !keep && !menu_open {
            self.sidebar_peek = false;
        }
    }

    /// Header, search, toolbar and tree; used by the docked panel and the hover overlay.
    pub(super) fn tree_contents(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>, overlay: bool) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.strong("Connections");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if overlay {
                    if ui.small_button("📌").on_hover_text("Dock sidebar (Ctrl+B)").clicked() {
                        self.s.sidebar_collapsed = false;
                        self.sidebar_peek = false;
                    }
                } else if ui.small_button("«").on_hover_text("Collapse sidebar (Ctrl+B)").clicked() {
                    self.s.sidebar_collapsed = true;
                }
            });
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("🔍 Search …").desired_width(ui.available_width() - 30.0));
            if !self.filter.is_empty() && ui.small_button("✖").clicked() {
                self.filter.clear();
            }
        });
        ui.horizontal(|ui| {
            let parent = self.current_folder();
            if ui.button("🗀 +").on_hover_text("New folder").clicked() {
                actions.push(Action::NewFolder(parent));
            }
            if ui.button("🖥 +").on_hover_text("New RDP connection").clicked() {
                actions.push(Action::NewConnection(parent, Protocol::Rdp));
            }
            if ui.button("⌨ +").on_hover_text("New SSH connection").clicked() {
                actions.push(Action::NewConnection(parent, Protocol::Ssh));
            }
            if ui.button("🔑").on_hover_text("Manage credentials").clicked() {
                self.show_credentials = true;
            }
        });
        let q = self.filter.trim().to_lowercase();
        // Drag and drop to move entries between folders; off while searching or in the overlay.
        let mut dnd = TreeDnd { enabled: q.is_empty() && !overlay, ..Default::default() };
        ui.add_space(4.0);
        theme::list_frame(ui, |ui| {
            ui.set_min_height(ui.available_height() - 6.0);
            egui::ScrollArea::vertical().id_salt(("tree_scroll", overlay)).auto_shrink([false; 2]).show(ui, |ui| {
                if q.is_empty() {
                    self.folder_tree(ui, None, actions, &mut dnd);
                    if self.data.folders.is_empty() && self.data.connections.is_empty() {
                        ui.add_space(20.0);
                        ui.label(
                            RichText::new("No connections yet.\n\nCreate one via “Edit” or import a backup or database of the Windows RDP-Manager via “File”.")
                                .weak(),
                        );
                    }
                    if dnd.enabled {
                        let pointer = ui.ctx().pointer_interact_pos();
                        let zone_at = |p: egui::Pos2| dnd.zones.iter().find(|(r, _)| r.contains(p)).map(|(r, parent)| (*r, *parent));
                        if dnd.dragged.is_some() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                            if let Some((rect, _)) = pointer.and_then(zone_at) {
                                ui.painter().rect_stroke(rect, egui::Rounding::same(2.0), egui::Stroke::new(1.5_f32, theme::accent(self.s.dark)));
                            }
                        }
                        // On release: into the folder under the pointer, or top level if over none.
                        if let (Some(sel), Some(p)) = (dnd.released, pointer) {
                            let parent = zone_at(p).map(|(_, parent)| parent).unwrap_or(None);
                            actions.push(Action::Move(sel, parent));
                        }
                    }
                } else {
                    let mut hits: Vec<&Connection> = self
                        .data
                        .connections
                        .iter()
                        .filter(|c| [&c.name, &c.host, &c.description].iter().any(|s| s.to_lowercase().contains(&q)))
                        .collect();
                    hits.sort_by_key(|c| c.name.to_lowercase());
                    for c in hits {
                        let path = self.folder_path(c.parent);
                        let label = if path.is_empty() { c.name.clone() } else { format!("{}   ({path})", c.name) };
                        self.connection_row(ui, c, label, actions, &mut dnd);
                    }
                }
            });
        });
    }
}
