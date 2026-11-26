//! Settings profiles (the original's group settings): the profile manager and the choice of a
//! profile in the properties of folders and connections.

use super::*;

impl App {
    /// Connections and folders that use a profile directly.
    pub(super) fn profile_users(&self, id: Uuid) -> (usize, usize) {
        (
            self.data.connections.iter().filter(|c| c.settings == SettingsRef::Profile(id)).count(),
            self.data.folders.iter().filter(|f| f.profiles.contains(&id)).count(),
        )
    }

    /// Opens the profile manager with `id` selected.
    pub(super) fn open_profile(&mut self, id: Option<Uuid>) {
        self.show_profiles = true;
        self.profile_selected = id;
        self.profile_edit = None;
        self.profile_new = false;
    }

    /// Profile choice of a folder for one protocol: none (its parent's applies) or a profile.
    pub(super) fn folder_profile_combo(ui: &mut egui::Ui, f: &mut Folder, protocol: Protocol, profiles: &[SettingsProfile], inherited: Option<&SettingsProfile>) {
        let mine = profiles.iter().find(|p| p.protocol == protocol && f.profiles.contains(&p.id));
        let none = match inherited {
            Some(p) => format!("From parent folder → {}", p.name),
            None => "(none)".to_string(),
        };
        let mut chosen = mine.map(|p| p.id);
        egui::ComboBox::from_id_salt(("folder_profile", protocol.key()))
            .selected_text(mine.map(|p| p.name.clone()).unwrap_or_else(|| none.clone()))
            .width(320.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut chosen, None, none);
                for p in profiles.iter().filter(|p| p.protocol == protocol) {
                    ui.selectable_value(&mut chosen, Some(p.id), &p.name);
                }
            });
        if chosen != mine.map(|p| p.id) {
            f.profiles.retain(|id| !profiles.iter().any(|p| p.id == *id && p.protocol == protocol));
            f.profiles.extend(chosen);
        }
    }

    /// Symbol of a folder in the tree: a folder, or the symbol of a connection type.
    pub(super) fn folder_icon_combo(ui: &mut egui::Ui, f: &mut Folder) {
        let label = |icon: Option<Protocol>| match icon {
            Some(p) => format!("{} {}", p.icon(), p.label()),
            None => "🗀 Folder".to_string(),
        };
        egui::ComboBox::from_id_salt("folder_icon").selected_text(label(f.icon)).width(200.0).show_ui(ui, |ui| {
            ui.selectable_value(&mut f.icon, None, label(None));
            for p in Protocol::ALL {
                ui.selectable_value(&mut f.icon, Some(p), label(Some(p)));
            }
        });
    }

    /// Colour of a folder: none (the theme's), one of the offered colours, or a custom one.
    pub(super) fn folder_color_choice(ui: &mut egui::Ui, f: &mut Folder) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if ui.selectable_label(f.color.is_none(), "None").on_hover_text("The theme's colour").clicked() {
                f.color = None;
            }
            let size = egui::vec2(18.0, 18.0);
            for c in Folder::COLORS {
                let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
                let fill = egui::Color32::from_rgb(c[0], c[1], c[2]);
                ui.painter().rect_filled(rect.shrink(2.0), 3.0_f32, fill);
                if f.color == Some(c) {
                    ui.painter().rect_stroke(rect, 3.0_f32, egui::Stroke::new(2.0_f32, ui.visuals().strong_text_color()));
                } else if resp.hovered() {
                    ui.painter().rect_stroke(rect, 3.0_f32, egui::Stroke::new(1.0_f32, ui.visuals().text_color()));
                }
                if resp.clicked() {
                    f.color = Some(c);
                }
            }
            let mut custom = f.color.unwrap_or([0x80, 0x80, 0x80]);
            if ui.color_edit_button_srgb(&mut custom).on_hover_text("Custom colour").changed() {
                f.color = Some(custom);
            }
        });
    }

    /// Settings choice of a connection: its own, the folder's profile, or a profile.
    pub(super) fn settings_combo(ui: &mut egui::Ui, c: &mut Connection, profiles: &[SettingsProfile], inherited: Option<&SettingsProfile>) {
        let inherit = match inherited {
            Some(p) => format!("Inherit from folder → {}", p.name),
            None => "Inherit from folder → none (own settings)".to_string(),
        };
        let text = match c.settings {
            SettingsRef::Own => "Own settings".to_string(),
            SettingsRef::Inherit => inherit.clone(),
            SettingsRef::Profile(id) => profiles.iter().find(|p| p.id == id).map(|p| p.name.clone()).unwrap_or_else(|| "(deleted)".into()),
        };
        egui::ComboBox::from_id_salt("conn_settings").selected_text(text).width(320.0).show_ui(ui, |ui| {
            ui.selectable_value(&mut c.settings, SettingsRef::Own, "Own settings");
            if c.parent.is_some() {
                ui.selectable_value(&mut c.settings, SettingsRef::Inherit, inherit);
            }
            for p in profiles.iter().filter(|p| p.protocol == c.protocol) {
                ui.selectable_value(&mut c.settings, SettingsRef::Profile(p.id), &p.name);
            }
        });
    }

    pub(super) fn ui_profiles(&mut self, ctx: &egui::Context) {
        if !self.show_profiles {
            return;
        }
        let stored = self.profile_selected.and_then(|id| self.profile(id).cloned());
        if !self.profile_new && self.profile_edit.as_ref().map(|p| p.id) != self.profile_selected {
            self.profile_edit = stored.clone();
        }
        let dirty = match &self.profile_edit {
            Some(e) => self.profile_new || stored.as_ref() != Some(e),
            None => false,
        };
        let mut open = true;
        let (mut select, mut new, mut save, mut revert, mut delete) = (None, None, false, false, None);
        let mut edit = self.profile_edit.clone();
        egui::Window::new("Settings Profiles")
            .id(egui::Id::new("profile_manager"))
            .open(&mut open)
            .collapsible(false)
            .default_size([820.0, 560.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!dirty, |ui| {
                        for protocol in SettingsProfile::PROTOCOLS {
                            if ui.button(format!("➕ New {}", protocol.label())).clicked() {
                                new = Some(SettingsProfile::new(&format!("{} settings", protocol.label()), protocol));
                            }
                        }
                        if let Some(p) = &stored {
                            if ui.button("🗐 Duplicate").clicked() {
                                new = Some(SettingsProfile { id: Uuid::new_v4(), name: format!("{} (Copy)", p.name), ..p.clone() });
                            }
                            let (nc, nf) = self.profile_users(p.id);
                            if ui
                                .add_enabled(nc + nf == 0, egui::Button::new("🗑 Delete"))
                                .on_disabled_hover_text(format!("Used by {nc} connection(s) and {nf} folder(s)."))
                                .clicked()
                            {
                                delete = Some(p.id);
                            }
                        }
                    });
                });
                ui.label(
                    RichText::new("A profile is used by connections that choose it, or that inherit it from their folder (Properties → Settings).")
                        .weak(),
                );
                ui.separator();
                ui.horizontal_top(|ui| {
                    // --- list
                    ui.vertical(|ui| {
                        ui.set_width(230.0);
                        theme::list_frame(ui, |ui| {
                            egui::ScrollArea::vertical().id_salt("profile_list").max_height(420.0).auto_shrink([false, false]).show(ui, |ui| {
                                ui.add_enabled_ui(!dirty, |ui| {
                                    let mut list: Vec<&SettingsProfile> = self.data.profiles.iter().collect();
                                    list.sort_by_key(|p| (p.protocol.key(), p.name.to_lowercase()));
                                    for p in list {
                                        let label = format!("{} {}", p.protocol.icon(), p.name);
                                        if ui.selectable_label(self.profile_selected == Some(p.id), label).clicked() {
                                            select = Some(p.id);
                                        }
                                    }
                                    if self.data.profiles.is_empty() {
                                        ui.label(RichText::new("No profiles yet – ➕ New").weak());
                                    }
                                });
                            });
                        });
                        ui.label(RichText::new(if dirty { "Save or revert the changes first." } else { " " }).weak());
                    });
                    ui.separator();
                    // --- the selected profile
                    ui.vertical(|ui| {
                        let Some(p) = edit.as_mut() else {
                            ui.label(RichText::new("Select a profile or create a new one.").weak());
                            return;
                        };
                        egui::Grid::new("profile_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                            ui.label("Name");
                            ui.add(egui::TextEdit::singleline(&mut p.name).desired_width(300.0));
                            ui.end_row();
                            ui.label("Protocol");
                            ui.label(p.protocol.label());
                            ui.end_row();
                            let (nc, nf) = self.profile_users(p.id);
                            ui.label("Used by");
                            ui.label(format!("{nc} connection(s), {nf} folder(s) directly"));
                            ui.end_row();
                        });
                        ui.add_space(4.0);
                        egui::ScrollArea::vertical().id_salt("profile_options").max_height(380.0).show(ui, |ui| {
                            let mut c = Connection::new(&p.name, p.protocol, None);
                            c.options = p.options.clone();
                            Self::protocol_options(ui, &mut c, &self.data.credentials);
                            p.options = c.options;
                        });
                        ui.horizontal(|ui| {
                            let ok = !p.name.trim().is_empty();
                            if ui.add_enabled(dirty && ok, egui::Button::new("💾 Save")).clicked() {
                                save = true;
                            }
                            if ui.add_enabled(dirty, egui::Button::new("↺ Revert")).clicked() {
                                revert = true;
                            }
                        });
                    });
                });
            });
        self.profile_edit = edit;
        if save {
            if let Some(mut p) = self.profile_edit.clone() {
                p.name = p.name.trim().to_string();
                if self.result(self.store.save_profile(&p)).is_some() {
                    match self.data.profiles.iter_mut().find(|x| x.id == p.id) {
                        Some(x) => *x = p.clone(),
                        None => self.data.profiles.push(p.clone()),
                    }
                    self.push(Level::Info, format!("Settings profile “{}” saved.", p.name));
                    self.profile_selected = Some(p.id);
                    self.profile_edit = Some(p);
                    self.profile_new = false;
                    // The properties dialog shows the profile names: refresh it.
                    self.editor_original = self.editor.clone();
                }
            }
        }
        if revert {
            self.profile_new = false;
            self.profile_edit = self.profile_selected.and_then(|id| self.profile(id).cloned());
        }
        if let Some(p) = new {
            self.profile_selected = None;
            self.profile_edit = Some(p);
            self.profile_new = true;
        }
        if let Some(id) = select {
            self.open_profile(Some(id));
        }
        if let Some(id) = delete {
            if self.result(self.store.delete_profile(id)).is_some() {
                if let Some(p) = self.data.profiles.iter().position(|p| p.id == id).map(|i| self.data.profiles.remove(i)) {
                    self.push(Level::Info, format!("Settings profile “{}” deleted.", p.name));
                }
                self.open_profile(None);
            }
        }
        if !open {
            self.show_profiles = false;
            self.profile_edit = None;
            self.profile_new = false;
        }
    }
}
