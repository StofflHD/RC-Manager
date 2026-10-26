//! Update, properties, reachability, export, reopen, prompt, credentials, picker and import dialogs.
//! Split out of `app.rs`; these are `App` methods driven from the main `update` loop.

use super::*;

impl App {
    /// The updater dialog (like notepad-rs) and the proxy setting.
    pub(super) fn ui_update(&mut self, ctx: &egui::Context) {
        if let Some(mut proxy) = self.update_proxy_edit.take() {
            let mut done = false;
            Self::modal("Updater Proxy").show(ctx, |ui| {
                ui.label("Proxy for update checks and downloads (empty = none):");
                ui.add(egui::TextEdit::singleline(&mut proxy).hint_text("http://proxy:3128").desired_width(320.0));
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        self.s.update_proxy = proxy.trim().to_string();
                        done = true;
                    }
                    if ui.button("Cancel").clicked() {
                        done = true;
                    }
                });
            });
            if !done && !ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.update_proxy_edit = Some(proxy);
            }
        }
        let Some(dlg) = self.update.take() else { return };
        let mut keep = true;
        // 1 install, 2 release page, 3 skip, 4 restart
        let mut action: Option<u8> = None;
        let current = env!("CARGO_PKG_VERSION");
        let sessions = self.sessions.len();
        Self::modal("Update RC Manager").show(ctx, |ui| {
            ui.set_min_width(400.0);
            ui.set_max_width(520.0);
            match &dlg.state {
                UpdateState::Checking => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Checking for updates …");
                    });
                }
                UpdateState::Installing => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Downloading and verifying the update …");
                    });
                }
                UpdateState::UpToDate => {
                    ui.label(format!("RC Manager {current} is up to date."));
                    if ui.button("OK").clicked() {
                        keep = false;
                    }
                }
                UpdateState::Available(rel) => {
                    ui.label(RichText::new(format!("RC Manager {} is available (installed: {current}).", rel.version)).strong());
                    if !rel.notes.trim().is_empty() {
                        theme::list_frame(ui, |ui| {
                            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                                ui.label(rel.notes.trim());
                            });
                        });
                    }
                    let appimage = crate::update::running_appimage();
                    if appimage.is_none() {
                        ui.label(RichText::new("Not started from an AppImage: download the new version from the release page.").weak());
                    } else if rel.appimage.is_none() {
                        ui.label(RichText::new("The release has no AppImage for this system.").weak());
                    }
                    ui.horizontal(|ui| {
                        if appimage.is_some() && rel.appimage.is_some() && ui.button("Update now").clicked() {
                            action = Some(1);
                        }
                        if ui.button("Open release page").clicked() {
                            action = Some(2);
                        }
                        if ui.button("Later").clicked() {
                            keep = false;
                        }
                        if ui.button("Skip this version").clicked() {
                            action = Some(3);
                        }
                    });
                }
                UpdateState::Installed(p) => {
                    ui.label("The update has been installed. It takes effect after a restart.");
                    ui.label(RichText::new(p.display().to_string()).weak());
                    if sessions > 0 {
                        ui.colored_label(ui.visuals().warn_fg_color, format!("A restart closes the {sessions} open session(s)."));
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Restart now").clicked() {
                            action = Some(4);
                        }
                        if ui.button("Later").clicked() {
                            keep = false;
                        }
                    });
                }
                UpdateState::Error(e) => {
                    ui.label(RichText::new("The update failed:").strong());
                    ui.label(e);
                    ui.horizontal(|ui| {
                        if ui.button("Open release page").clicked() {
                            action = Some(2);
                        }
                        if ui.button("OK").clicked() {
                            keep = false;
                        }
                    });
                }
            }
        });
        let busy = matches!(dlg.state, UpdateState::Checking | UpdateState::Installing);
        if !busy && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            keep = false;
        }
        match (action, &dlg.state) {
            (Some(1), UpdateState::Available(rel)) => {
                let rel = rel.clone();
                self.start_update_install(rel);
                return;
            }
            (Some(2), UpdateState::Available(rel)) => {
                let page = rel.page.clone();
                self.open_url(&page);
                keep = false;
            }
            (Some(2), _) => {
                self.open_url(&crate::update::releases_page());
                keep = false;
            }
            (Some(3), UpdateState::Available(rel)) => {
                self.s.update_skip = rel.version.clone();
                keep = false;
            }
            (Some(4), UpdateState::Installed(p)) => {
                self.restart_exe = Some(p.clone());
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                keep = false;
            }
            _ => {}
        }
        if keep {
            self.update = Some(dlg);
        }
    }

    /// Properties of the selected folder or connection, like the original's edit dialog.
    pub(super) fn ui_properties(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if !self.props_open || self.editor.is_none() {
            self.props_open = false;
            return;
        }
        let title = match &self.editor_original {
            Some(Editor::Folder(f)) => format!("Properties – {}", f.name),
            Some(Editor::Connection(c)) => format!("Properties – {}", c.name),
            None => "Properties".into(),
        };
        let mut open = true;
        let (mut ok, mut apply, mut cancel) = (false, false, false);
        let max_h = ctx.screen_rect().height() * 0.85;
        // Fixed size and centred like the original's dialog; long content scrolls.
        egui::Window::new(title)
            .id(egui::Id::new("properties_dialog"))
            .open(&mut open)
            .collapsible(false)
            .fixed_size([640.0, 540.0f32.min(max_h)])
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::TopBottomPanel::bottom("properties_buttons").show_inside(ui, |ui| {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ok = ui.button("OK").clicked();
                        apply = ui.add_enabled(self.editor_dirty(), egui::Button::new("Apply")).clicked();
                        cancel = ui.button("Cancel").clicked();
                        if self.editor_dirty() {
                            ui.label(RichText::new("unsaved changes").weak());
                        }
                    });
                });
                egui::CentralPanel::default().show_inside(ui, |ui| self.editor_contents(ui, actions));
            });
        // Escape cancels, as in the original's dialog (unless it closes a popup or the
        // reachability dialog on top).
        if self.reach.is_none() && !ctx.memory(|m| m.any_popup_open()) && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        if ok || apply {
            self.save_editor();
            self.select(self.selection);
        }
        if cancel || !open {
            self.editor = self.editor_original.clone();
        }
        if ok || cancel || !open {
            self.props_open = false;
        }
    }

    /// Continuous reachability check, like the original's "Ping" (ping -t).
    pub(super) fn ui_reachability(&mut self, ctx: &egui::Context) {
        let Some(r) = &mut self.reach else { return };
        r.poll();
        let mut open = true;
        let mut close = false;
        egui::Window::new(format!("Reachability – {}", r.name))
            .id(egui::Id::new("reachability_dialog"))
            .open(&mut open)
            .collapsible(false)
            .fixed_size([620.0, 330.0])
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("{} – port {} (TCP, every 2 s) and ping", r.host, r.port));
                ui.add_space(4.0);
                theme::list_frame(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(260.0).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                        for (lvl, line) in &r.lines {
                            let text = RichText::new(line).monospace();
                            match lvl {
                                Level::Info => ui.label(text),
                                Level::Warn => ui.colored_label(ui.visuals().warn_fg_color, text),
                                Level::Error => ui.colored_label(ui.visuals().error_fg_color, text),
                            };
                        }
                    });
                });
                ui.add_space(4.0);
                close = ui.button("Close").clicked();
            });
        if close || !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.reach = None;
        }
    }

    pub(super) fn modal(title: &str) -> egui::Window<'_> {
        egui::Window::new(title).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0])
    }

    /// Warns before exporting, because the file contains passwords in clear text.
    pub(super) fn ui_export(&mut self, ctx: &egui::Context) {
        if !self.export_confirm {
            return;
        }
        let mut close = false;
        Self::modal("Export Database").show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.label("This exports all connections, folders and credentials to a file you can import again or move to another machine.");
            ui.add_space(4.0);
            ui.colored_label(ui.visuals().warn_fg_color, "The file contains the stored passwords in clear text. Keep it in a safe place.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Choose file …").clicked() {
                    self.pick(Pick::ExportDb);
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.export_confirm = false;
        }
    }

    /// Offers to reopen the sessions that were open when RC Manager last closed.
    pub(super) fn ui_reopen(&mut self, ctx: &egui::Context) {
        if self.reopen_offer.is_empty() {
            return;
        }
        // Only connections that still exist can be reopened.
        let items: Vec<(Uuid, String)> =
            self.reopen_offer.iter().filter_map(|id| self.connection(*id).map(|c| (*id, format!("{} {}", c.protocol.icon(), c.name)))).collect();
        if items.is_empty() {
            self.reopen_offer.clear();
            self.reopen_sel = None;
            return;
        }
        // Selection ticked per connection; starts with all selected (the original offers all).
        let sel = self.reopen_sel.get_or_insert_with(|| items.iter().map(|(id, _)| *id).collect());
        sel.retain(|id| items.iter().any(|(i, _)| i == id));
        let mut reopen = false;
        let mut dismiss = false;
        Self::modal("Reopen Sessions").show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(format!("{} session(s) were open when RC Manager last closed. Choose which to reopen:", items.len()));
            theme::list_frame(ui, |ui| {
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for (id, name) in &items {
                        let mut on = sel.contains(id);
                        if ui.checkbox(&mut on, name).changed() {
                            if on {
                                sel.insert(*id);
                            } else {
                                sel.remove(id);
                            }
                        }
                    }
                });
            });
            let mut all = items.iter().all(|(id, _)| sel.contains(id));
            if ui.checkbox(&mut all, "Select all").changed() {
                match all {
                    true => sel.extend(items.iter().map(|(id, _)| *id)),
                    false => sel.clear(),
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let n = sel.len();
                if ui.add_enabled(n > 0, egui::Button::new(format!("↻ Reopen {n} session(s)"))).clicked() {
                    reopen = true;
                }
                if ui.button("Not now").clicked() {
                    dismiss = true;
                }
            });
        });
        if !reopen && !dismiss && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            dismiss = true;
        }
        if reopen {
            let chosen: Vec<Uuid> = items.iter().filter(|(id, _)| sel.contains(id)).map(|(id, _)| *id).collect();
            self.reopen_offer.clear();
            self.reopen_sel = None;
            for id in chosen {
                self.connect(id);
            }
        } else if dismiss {
            self.reopen_offer.clear();
            self.reopen_sel = None;
        }
    }

    pub(super) fn ui_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(sel) = self.confirm_delete {
            let text = match sel {
                Sel::Connection(id) => format!("Delete connection “{}”?", self.connection(id).map(|c| c.name.as_str()).unwrap_or("")),
                Sel::Folder(id) => {
                    let (f, c) = self.descendants(id);
                    format!(
                        "Delete folder “{}” with {} subfolder(s) and {} connection(s)?",
                        self.folder(id).map(|f| f.name.as_str()).unwrap_or(""),
                        f.len() - 1,
                        c.len()
                    )
                }
            };
            let mut close = false;
            Self::modal("Delete").show(ctx, |ui| {
                ui.label(text);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        self.delete(sel);
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
            if close {
                self.confirm_delete = None;
            }
        }

        if let Some(mut ask) = self.ask_credentials.take() {
            let mut keep = true;
            let mut go = false;
            Self::modal("Log On").show(ctx, |ui| {
                if let Some(note) = &ask.note {
                    ui.colored_label(ui.visuals().warn_fg_color, note);
                }
                egui::Grid::new("ask_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("User");
                    ui.text_edit_singleline(&mut ask.cred.username);
                    ui.end_row();
                    ui.label("Domain");
                    ui.text_edit_singleline(&mut ask.cred.domain);
                    ui.end_row();
                    ui.label("Password");
                    let r = ui.add(egui::TextEdit::singleline(&mut ask.cred.password).password(true));
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        go = true;
                    }
                    ui.end_row();
                });
                if !ask.connection.is_nil() {
                    ui.checkbox(&mut ask.save, "Save as credentials and assign to this connection");
                }
                ui.horizontal(|ui| {
                    if ui.button("Connect").clicked() {
                        go = true;
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                        self.quick_pending = None;
                    }
                });
            });
            if go {
                self.finish_ask(ask);
            } else if keep {
                self.ask_credentials = Some(ask);
            }
        }

        self.ui_prompt(ctx);
        self.ui_reopen(ctx);
        self.ui_credentials(ctx);
        self.ui_import(ctx);
        self.ui_rdp_import(ctx);
        self.ui_export(ctx);
        self.ui_master(ctx);

        if self.show_tools {
            let mut open = true;
            egui::Window::new("External Programs").open(&mut open).collapsible(false).show(ctx, |ui| {
                egui::Grid::new("tools").num_columns(2).spacing([12.0, 6.0]).striped(true).show(ui, |ui| {
                    for (k, v) in self.tools.summary() {
                        ui.label(k);
                        ui.label(v);
                        ui.end_row();
                    }
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Debian/Ubuntu: sudo apt install freerdp3-x11 tigervnc-viewer sshpass\n\
                         Fedora:        sudo dnf install freerdp tigervnc sshpass\n\
                         Arch:          sudo pacman -S freerdp tigervnc sshpass",
                    )
                    .monospace(),
                );
            });
            self.show_tools = open;
        }
        if self.show_about {
            let mut open = true;
            egui::Window::new("About RC Manager")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_max_width(520.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(format!("RC Manager {}", env!("CARGO_PKG_VERSION"))).heading().color(theme::accent(ui.visuals().dark_mode)),
                        );
                        ui.label("Connection manager for RDP, SSH, VNC, web and external programs.");
                        ui.label("Reads databases of the Windows RDP-Manager (Cinspiration, DB version 12).");
                        ui.label(RichText::new(format!("Data: {}", Store::default_dir().display())).weak());
                        ui.hyperlink_to(format!("github.com/{}", crate::update::repo()), env!("CARGO_PKG_REPOSITORY"));
                        ui.add_space(10.0);
                        ui.separator();
                        ui.label(RichText::new(env!("CARGO_PKG_AUTHORS")).strong());
                        ui.add_space(6.0);
                        ui.label("License: GNU General Public License v3.0 or later");
                        ui.label(
                            RichText::new(
                                "This program comes with ABSOLUTELY NO WARRANTY. It is free software: you can \
                                 redistribute it and/or modify it under the terms of the GPL.",
                            )
                            .small()
                            .weak(),
                        );
                        ui.hyperlink_to("gnu.org/licenses/gpl-3.0", "https://www.gnu.org/licenses/gpl-3.0.html");
                    });
                    ui.add_space(4.0);
                    egui::CollapsingHeader::new("License Text").show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                            // Wide enough for the 77 columns of the GPL text.
                            ui.set_min_width(470.0);
                            let text = RichText::new(LICENSE.replace('\r', "")).font(egui::FontId::monospace(10.0));
                            ui.add(egui::Label::new(text).wrap());
                        });
                    });
                });
            self.show_about = open;
        }
    }

    /// Shows a client's question (certificate check) and writes the answer to its stdin.
    pub(super) fn ui_prompt(&mut self, ctx: &egui::Context) {
        let Some(p) = self.prompts.front() else { return };
        let certificate = p.question.contains("certificate");
        let mut answer = None;
        Self::modal(if certificate { "Certificate Check" } else { "Question" }).show(ctx, |ui| {
            ui.set_max_width(560.0);
            ui.label(RichText::new(format!("{}:", p.name)).strong());
            if p.changed {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    RichText::new("The certificate of this server has CHANGED since it was trusted. This can mean that someone intercepts the connection (man in the middle). Only continue if you know that the server got a new certificate.").strong(),
                );
            } else if certificate {
                ui.label("The identity of the server could not be verified. Check the details below; trust the certificate only if it really belongs to this server.");
            }
            theme::list_frame(ui, |ui| {
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for line in &p.details {
                        ui.label(RichText::new(line).monospace().small());
                    }
                });
            });
            ui.label(RichText::new(&p.question).weak());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if certificate {
                    if ui.button("Trust once").clicked() {
                        answer = Some("Y");
                    }
                    let remember = ui.add_enabled(p.certificate.is_some(), egui::Button::new("Trust always"));
                    if remember.on_hover_text("Remember the fingerprint of this certificate for this server").clicked() {
                        answer = Some("always");
                    }
                    if ui.button("Cancel connection").clicked() {
                        answer = Some("N");
                    }
                } else {
                    if ui.button("Yes").clicked() {
                        answer = Some("Y");
                    }
                    if ui.button("No").clicked() {
                        answer = Some("N");
                    }
                }
            });
        });
        if let Some(mut a) = answer {
            let p = self.prompts.pop_front().unwrap();
            // "T" accepts the certificate for this connection only, so FreeRDP never writes its
            // own store; RC Manager passes trusted fingerprints itself.
            let temporary = p.question.contains("Y/T/N");
            if a == "always" {
                // Accept now and remember the fingerprint in RC Manager's own store.
                a = "Y";
                if let Some((host_port, fp)) = &p.certificate {
                    match self.store.trust_certificate(host_port, fp) {
                        Ok(()) => self.push(Level::Info, format!("Certificate of {host_port} is trusted from now on.")),
                        Err(e) => self.push(Level::Warn, format!("Cannot remember the certificate: {e:#}")),
                    }
                }
            }
            if a == "Y" && temporary {
                a = "T";
            }
            let ok = self.sessions.get_mut(&p.session).and_then(|s| s.stdin.as_mut()).map(|stdin| {
                use std::io::Write;
                stdin.write_all(format!("{a}\n").as_bytes()).and_then(|_| stdin.flush()).is_ok()
            });
            if ok != Some(true) {
                self.push(Level::Warn, format!("{}: the answer could not be sent", p.name));
            }
        }
    }

    pub(super) fn finish_ask(&mut self, ask: AskCredentials) {
        if ask.connection.is_nil() {
            if let Some(c) = self.quick_pending.take() {
                self.launch(&c, Some(&ask.cred));
            }
            return;
        }
        if ask.save {
            let mut cred = ask.cred.clone();
            cred.id = Uuid::new_v4();
            if cred.name.is_empty() {
                cred.name = cred.username.clone();
            }
            if self.result(self.store.save_credential(&cred)).is_some() {
                self.data.credentials.push(cred.clone());
                if let Some(mut c) = self.connection(ask.connection).cloned() {
                    c.credential = CredentialRef::Id(cred.id);
                    self.result(self.store.save_connection(&c));
                    if let Some(x) = self.data.connections.iter_mut().find(|x| x.id == c.id) {
                        *x = c.clone();
                    }
                    if matches!(&self.editor, Some(Editor::Connection(e)) if e.id == c.id) {
                        self.editor = Some(Editor::Connection(c));
                        self.editor_original = self.editor.clone();
                    }
                }
            }
        }
        if let Some(c) = self.connection(ask.connection).cloned() {
            self.launch(&c, Some(&ask.cred));
        }
    }

    /// The credentials still referring to a set: connections first, then folders.
    pub(super) fn credential_users(&self, id: Uuid) -> Vec<(Sel, String)> {
        let mut users: Vec<(Sel, String)> = self
            .data
            .connections
            .iter()
            .filter(|c| c.credential == CredentialRef::Id(id))
            .map(|c| (Sel::Connection(c.id), format!("{} {}", c.protocol.icon(), c.name)))
            .collect();
        users.sort_by_key(|(_, n)| n.to_lowercase());
        let mut folders: Vec<(Sel, String)> =
            self.data.folders.iter().filter(|f| f.credential == Some(id)).map(|f| (Sel::Folder(f.id), format!("🗀 {}", f.name))).collect();
        folders.sort_by_key(|(_, n)| n.to_lowercase());
        users.extend(folders);
        users
    }

    /// Credential manager: list with search on the left, the selected set on the right.
    /// Changes are kept in a working copy until "Save" (like the original's form); while it
    /// has unsaved changes the selection cannot change, so nothing is lost or saved by accident.
    pub(super) fn ui_credentials(&mut self, ctx: &egui::Context) {
        if !self.show_credentials {
            return;
        }
        let mut open = true;
        let mut save = false;
        let mut revert = false;
        let mut select: Option<Uuid> = None;
        let mut new: Option<Credential> = None;
        let mut edit_user: Option<Sel> = None;
        let stored = |app: &Self, id: Uuid| app.credential(id).cloned();
        if self.cred_edit.is_none() && !self.cred_new {
            self.cred_edit = self.cred_selected.and_then(|id| stored(self, id));
        }
        let dirty = match &self.cred_edit {
            Some(e) => self.cred_new || stored(self, e.id).as_ref() != Some(e),
            None => false,
        };
        // Fixed size and centred, like the original's form (a free window would open at a
        // different place every time and move while its content changes).
        egui::Window::new("Credentials")
            .id(egui::Id::new("credential_manager"))
            .open(&mut open)
            .collapsible(false)
            .fixed_size([860.0, 440.0])
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!dirty, |ui| {
                        if ui.button("➕ New").clicked() {
                            new = Some(Credential::new(""));
                        }
                        let sel = self.cred_selected.and_then(|id| stored(self, id));
                        if ui.add_enabled(sel.is_some(), egui::Button::new("🗐 Duplicate")).clicked() {
                            if let Some(c) = sel.clone() {
                                new = Some(Credential { id: Uuid::new_v4(), name: format!("{} (Copy)", c.name), ..c });
                            }
                        }
                        if ui.add_enabled(sel.is_some(), egui::Button::new("🗑 Delete …")).clicked() {
                            self.cred_confirm_delete = self.cred_selected;
                        }
                    });
                    ui.separator();
                    ui.add(egui::TextEdit::singleline(&mut self.cred_filter).hint_text("🔍 Search …").desired_width(200.0));
                });
                ui.add_space(4.0);
                ui.columns(2, |cols| {
                    // --- list
                    let ui = &mut cols[0];
                    let filter = self.cred_filter.trim().to_lowercase();
                    let mut rows: Vec<(Credential, usize)> = self
                        .data
                        .credentials
                        .iter()
                        .filter(|c| {
                            filter.is_empty() || [&c.name, &c.username, &c.domain].iter().any(|t| t.to_lowercase().contains(&filter))
                        })
                        .map(|c| (c.clone(), self.credential_users(c.id).len()))
                        .collect();
                    rows.sort_by_key(|(c, _)| c.name.to_lowercase());
                    theme::list_frame(ui, |ui| {
                        egui::ScrollArea::vertical().id_salt("cred_list").max_height(340.0).auto_shrink([false, false]).show(ui, |ui| {
                            ui.add_enabled_ui(!dirty, |ui| {
                                egui::Grid::new("cred_table").num_columns(4).spacing([12.0, 4.0]).show(ui, |ui| {
                                    ui.label(RichText::new("Name").strong());
                                    ui.label(RichText::new("User").strong());
                                    ui.label(RichText::new("Domain").strong());
                                    ui.label(RichText::new("Used").strong());
                                    ui.end_row();
                                    for (c, used) in &rows {
                                        let selected = self.cred_selected == Some(c.id);
                                        if ui.selectable_label(selected, &c.name).clicked() {
                                            select = Some(c.id);
                                        }
                                        ui.label(&c.username);
                                        ui.label(&c.domain);
                                        ui.label(used.to_string());
                                        ui.end_row();
                                    }
                                });
                                if rows.is_empty() {
                                    ui.label(RichText::new(if filter.is_empty() { "No credentials yet – ➕ New" } else { "No match" }).weak());
                                }
                            });
                        });
                    });
                    // Always one line, so the window keeps its height (and position).
                    ui.label(RichText::new(if dirty { "Save or revert the changes to select another entry." } else { " " }).weak());

                    // --- details of the selected set
                    let ui = &mut cols[1];
                    let Some(mut c) = self.cred_edit.clone() else {
                        ui.label(RichText::new("Select an entry or create a new one.").weak());
                        return;
                    };
                    ui.heading(RichText::new(if c.name.trim().is_empty() { "New credentials".to_string() } else { c.name.clone() }).color(theme::accent(ui.visuals().dark_mode)));
                    ui.add_space(6.0);
                    egui::Grid::new("cred_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                        ui.label("Name");
                        ui.add(egui::TextEdit::singleline(&mut c.name).hint_text("e.g. Domain admin").desired_width(240.0));
                        ui.end_row();
                        ui.label("User");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut c.username).desired_width(240.0));
                            if ui.add_enabled(!c.username.is_empty(), egui::Button::new("Copy").small()).on_hover_text("Copy the user name").clicked() {
                                ui.ctx().copy_text(c.username.clone());
                            }
                        });
                        ui.end_row();
                        ui.label("Domain");
                        ui.add(egui::TextEdit::singleline(&mut c.domain).hint_text("optional").desired_width(240.0));
                        ui.end_row();
                        ui.label("Password");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut c.password).password(!self.cred_show_password).desired_width(240.0));
                            let eye = if self.cred_show_password { "Hide" } else { "Show" };
                            if ui.add(egui::Button::new("👁").small().selected(self.cred_show_password)).on_hover_text(eye).clicked() {
                                self.cred_show_password = !self.cred_show_password;
                            }
                            if ui.add_enabled(!c.password.is_empty(), egui::Button::new("Copy").small()).on_hover_text("Copy the password").clicked() {
                                ui.ctx().copy_text(c.password.clone());
                            }
                        });
                        ui.end_row();
                    });
                    // Hints that do not block saving, except a missing name.
                    let name_missing = c.name.trim().is_empty();
                    let duplicate = self.data.credentials.iter().any(|x| x.id != c.id && x.name.trim().eq_ignore_ascii_case(c.name.trim()));
                    if name_missing {
                        ui.colored_label(ui.visuals().warn_fg_color, "A name is required.");
                    } else if duplicate {
                        ui.colored_label(ui.visuals().warn_fg_color, "Another entry has the same name.");
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.add_enabled(dirty && !name_missing, egui::Button::new("💾 Save")).clicked() {
                            save = true;
                        }
                        if ui.add_enabled(dirty, egui::Button::new(if self.cred_new { "Discard" } else { "Revert" })).clicked() {
                            revert = true;
                        }
                    });
                    ui.add_space(10.0);
                    let users = if self.cred_new { Vec::new() } else { self.credential_users(c.id) };
                    ui.label(RichText::new(format!("Used by {} connection(s)/folder(s)", users.len())).strong());
                    egui::ScrollArea::vertical().id_salt("cred_users").max_height(120.0).show(ui, |ui| {
                        for (sel, name) in &users {
                            if ui.link(name).on_hover_text("Open the properties").clicked() {
                                edit_user = Some(*sel);
                            }
                        }
                    });
                    self.cred_edit = Some(c);
                });
            });

        // Escape closes the manager when nothing is unsaved (and no dialog is on top).
        if !dirty && self.cred_confirm_delete.is_none() && !ctx.memory(|m| m.any_popup_open()) && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            open = false;
        }
        if save {
            if let Some(c) = self.cred_edit.clone() {
                let c = Credential { name: c.name.trim().to_string(), username: c.username.trim().to_string(), domain: c.domain.trim().to_string(), ..c };
                if self.result(self.store.save_credential(&c)).is_some() {
                    match self.data.credentials.iter_mut().find(|x| x.id == c.id) {
                        Some(x) => *x = c.clone(),
                        None => self.data.credentials.push(c.clone()),
                    }
                    self.push(Level::Info, format!("Credentials “{}” saved.", c.name));
                    self.cred_selected = Some(c.id);
                    self.cred_edit = Some(c);
                    self.cred_new = false;
                    // The properties dialog shows the credential names: refresh it.
                    self.editor_original = self.editor.clone();
                }
            }
        }
        if revert {
            if self.cred_new {
                self.cred_new = false;
                self.cred_edit = self.cred_selected.and_then(|id| stored(self, id));
            } else {
                self.cred_edit = self.cred_selected.and_then(|id| stored(self, id));
            }
        }
        if let Some(c) = new {
            self.cred_selected = None;
            self.cred_edit = Some(c);
            self.cred_new = true;
            self.cred_show_password = false;
        }
        if let Some(id) = select {
            self.cred_selected = Some(id);
            self.cred_edit = stored(self, id);
            self.cred_new = false;
            self.cred_show_password = false;
        }

        // Confirmation before deleting.
        if let Some(id) = self.cred_confirm_delete {
            let name = self.credential(id).map(|c| c.name.clone()).unwrap_or_default();
            let used = self.credential_users(id).len();
            let mut answer = None;
            Self::modal("Delete Credentials").show(ctx, |ui| {
                ui.label(format!("Delete the credentials “{name}”?"));
                if used > 0 {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("{used} connection(s)/folder(s) use them and will have no credentials afterwards."),
                    );
                }
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        answer = Some(false);
                    }
                });
            });
            match answer {
                Some(true) => {
                    self.cred_confirm_delete = None;
                    if self.result(self.store.delete_credential(id)).is_some() {
                        self.data.credentials.retain(|c| c.id != id);
                        for c in &mut self.data.connections {
                            if c.credential == CredentialRef::Id(id) {
                                c.credential = CredentialRef::None;
                            }
                        }
                        for f in &mut self.data.folders {
                            if f.credential == Some(id) {
                                f.credential = None;
                            }
                        }
                        self.push(Level::Info, format!("Credentials “{name}” deleted."));
                        self.cred_selected = None;
                        self.cred_edit = None;
                        self.cred_new = false;
                        let sel = self.selection;
                        self.editor_original = self.editor.clone();
                        self.select(sel);
                    }
                }
                Some(false) => self.cred_confirm_delete = None,
                None => {}
            }
        }

        if let Some(sel) = edit_user {
            self.select(Some(sel));
            self.props_open = self.editor.is_some();
        }
        if !open {
            // Closing drops unsaved changes (the window's ✖ is the "close" of the original).
            self.cred_edit = None;
            self.cred_new = false;
            self.cred_confirm_delete = None;
            self.cred_show_password = false;
        }
        self.show_credentials = open;
    }

    /// Opens a native file dialog without blocking the UI; the answer arrives in `poll_picker`.
    pub(super) fn pick(&mut self, kind: Pick) {
        if self.picker.is_some() {
            return;
        }
        let (tx, rx) = channel();
        self.picker = Some(rx);
        let ctx = self.ctx.clone();
        let home = dirs::home_dir();
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new();
            if let Some(h) = &home {
                dialog = dialog.set_directory(h);
            }
            let picked = match kind {
                Pick::Database => dialog
                    .set_title("Select an RDP-Manager backup (.zip) or database (SqLiteDatabase.db3)")
                    .add_filter("RDP-Manager backup or database", &["zip", "db3"])
                    .add_filter("RDP-Manager backup", &["zip"])
                    .add_filter("RDP-Manager database", &["db3"])
                    .add_filter("All files", &["*"])
                    .pick_file()
                    .map(Picked::Database),
                Pick::RdpFiles => dialog
                    .set_title("Select .rdp files")
                    .add_filter("Remote Desktop files", &["rdp"])
                    .pick_files()
                    .map(Picked::Rdp),
                Pick::RdpFolder => dialog.set_title("Select a folder with .rdp files").pick_folder().map(|dir| {
                    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
                        .map(|rd| {
                            rd.flatten()
                                .map(|e| e.path())
                                .filter(|p| p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("rdp")))
                                .collect()
                        })
                        .unwrap_or_default();
                    files.sort();
                    Picked::Rdp(files)
                }),
                Pick::ExportDb => dialog
                    .set_title("Export the RC Manager database to a file")
                    .set_file_name("rc-manager-export.json")
                    .add_filter("RC Manager export", &["json"])
                    .save_file()
                    .map(Picked::ExportTo),
                Pick::ImportDb => dialog
                    .set_title("Import an RC Manager export")
                    .add_filter("RC Manager export", &["json"])
                    .add_filter("All files", &["*"])
                    .pick_file()
                    .map(Picked::ImportDb),
            };
            tx.send(picked).ok();
            ctx.request_repaint();
        });
    }

    pub(super) fn poll_picker(&mut self) {
        let Some(rx) = &self.picker else { return };
        let picked = match rx.try_recv() {
            Ok(p) => p,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        self.picker = None;
        match picked {
            Some(Picked::Database(path)) => {
                let app_password = self.import_dialog.take().map(|d| d.app_password).unwrap_or_default();
                let backup = backup::is_zip(&path).then(|| backup::inspect(&path).map_err(|e| format!("{e:#}")));
                self.import_dialog = Some(ImportDialog { path, app_password, backup });
            }
            Some(Picked::Rdp(files)) => {
                if files.is_empty() {
                    self.push(Level::Warn, "The selected folder contains no .rdp files.");
                }
                if let Some(d) = &mut self.rdp_import {
                    d.files = files;
                }
            }
            Some(Picked::ExportTo(mut path)) => {
                if path.extension().is_none() {
                    path.set_extension("json");
                }
                match crate::store::export_json(&self.data) {
                    // The file holds passwords in clear text, so it is written readable only by
                    // the current user (mode 0600).
                    Ok(json) => match crate::vault::write_private(&path, json.as_bytes()) {
                        Ok(()) => self.push(
                            Level::Info,
                            format!(
                                "Exported {} connection(s), {} folder(s) and {} credential(s) to {} (contains passwords in clear text).",
                                self.data.connections.len(),
                                self.data.folders.len(),
                                self.data.credentials.len(),
                                path.display()
                            ),
                        ),
                        Err(e) => self.push(Level::Error, format!("Export failed: {e:#}")),
                    },
                    Err(e) => self.push(Level::Error, format!("Export failed: {e:#}")),
                }
            }
            Some(Picked::ImportDb(path)) => match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| crate::store::import_json(&t).map_err(|e| format!("{e:#}"))) {
                Ok(data) => {
                    let (nf, nc, nk) = (data.folders.len(), data.connections.len(), data.credentials.len());
                    if self.result(self.store.save_all(&data)).is_some() {
                        if let Some(d) = self.result(self.store.load()) {
                            self.data = d;
                        }
                        self.editor_original = self.editor.clone();
                        self.select(None);
                        self.push(Level::Info, format!("Imported {nc} connection(s), {nf} folder(s) and {nk} credential(s)."));
                    }
                }
                Err(e) => self.push(Level::Error, format!("Import failed: {e}")),
            },
            None => {}
        }
    }

    pub(super) fn file_label(ui: &mut egui::Ui, path: &std::path::Path) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let dir = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
        ui.vertical(|ui| {
            // Leave room for a button on the right; long folders are shortened with "…".
            ui.set_max_width((ui.available_width() - 100.0).max(120.0));
            ui.add(egui::Label::new(RichText::new(name).strong()).truncate());
            ui.add(egui::Label::new(RichText::new(dir).weak().small()).truncate());
        });
    }

    pub(super) fn ui_import(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.import_dialog.take() else { return };
        let mut keep = true;
        let mut run = false;
        let mut change = false;
        let busy = self.picker.is_some();
        Self::modal("Import Windows RDP-Manager").show(ctx, |ui| {
            ui.set_max_width(480.0);
            theme::list_frame(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("🗄");
                    Self::file_label(ui, &d.path);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(!busy, egui::Button::new("Change …")).clicked() {
                            change = true;
                        }
                    });
                });
            });
            match &d.backup {
                Some(Ok(info)) => {
                    let mut text = format!("Backup of RDP-Manager {}", if info.version.is_empty() { "?" } else { &info.version });
                    if !info.date.is_empty() {
                        text += &format!(" from {}", info.date);
                    }
                    ui.label(text);
                    if !info.comment.is_empty() {
                        ui.label(RichText::new(format!("“{}”", info.comment)).italics());
                    }
                }
                Some(Err(e)) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                None => {
                    ui.label(RichText::new("A “DisableShowPw” file next to it is picked up automatically.").weak());
                }
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Application password (if set):");
                ui.add(egui::TextEdit::singleline(&mut d.app_password).password(true));
            });
            ui.label(RichText::new("Entries with the same ID are overwritten; importing twice is harmless.").weak());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let importable = !matches!(d.backup, Some(Err(_)));
                if ui.add_enabled(!busy && importable, egui::Button::new("Import")).clicked() {
                    run = true;
                }
                if ui.button("Cancel").clicked() {
                    keep = false;
                }
            });
        });
        if change {
            self.import_dialog = Some(d);
            self.pick(Pick::Database);
            return;
        }
        if run {
            let pw = Some(d.app_password.as_str()).filter(|p| !p.is_empty());
            match legacy_import::import(&d.path, pw) {
                Ok(r) => {
                    if self.result(self.store.save_all(&r.data)).is_some() {
                        let msg = format!(
                            "Import finished: {} folders, {} connections, {} credentials ({} deleted entries skipped).",
                            r.data.folders.len(),
                            r.data.connections.len(),
                            r.data.credentials.len(),
                            r.skipped_deleted
                        );
                        self.push(Level::Info, msg);
                        for w in r.warnings {
                            self.push(Level::Warn, w);
                        }
                        // Take over the layout of the original: an auto-hidden connection tree
                        // corresponds to the collapsed sidebar with hover overlay.
                        if let Some(collapsed) = r.backup.as_ref().and_then(|b| b.sidebar_autohide) {
                            self.s.sidebar_collapsed = collapsed;
                            self.push(
                                Level::Info,
                                if collapsed { "Sidebar collapsed as in the backup (auto-hide)." } else { "Sidebar docked as in the backup." },
                            );
                        }
                        if let Some(data) = self.result(self.store.load()) {
                            self.data = data;
                        }
                        self.editor_original = self.editor.clone();
                        self.select(None);
                        keep = false;
                    }
                }
                Err(e) => self.push(Level::Error, format!("Import failed: {e:#}")),
            }
        }
        if keep {
            self.import_dialog = Some(d);
        }
    }

    pub(super) fn ui_rdp_import(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.rdp_import.take() else { return };
        let mut keep = true;
        let mut run = false;
        let mut pick = None;
        let busy = self.picker.is_some();
        let choices = self.folder_choices(None);
        Self::modal("Import .rdp Files").show(ctx, |ui| {
            ui.set_max_width(480.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!busy, egui::Button::new("📄 Choose files …")).clicked() {
                    pick = Some(Pick::RdpFiles);
                }
                if ui.add_enabled(!busy, egui::Button::new("🗀 Choose folder …")).clicked() {
                    pick = Some(Pick::RdpFolder);
                }
                if busy {
                    ui.spinner();
                }
            });
            ui.add_space(4.0);
            theme::list_frame(ui, |ui| {
                egui::ScrollArea::vertical().max_height(180.0).auto_shrink([false, true]).show(ui, |ui| {
                    if d.files.is_empty() {
                        ui.label(RichText::new("No files selected.").weak());
                    }
                    for f in &d.files {
                        ui.horizontal(|ui| {
                            ui.label("🖥");
                            Self::file_label(ui, f);
                        });
                    }
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Target folder");
                Self::folder_combo(ui, "rdp_import_folder", &mut d.folder, &choices);
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let label = if d.files.is_empty() { "Import".to_string() } else { format!("Import {} file(s)", d.files.len()) };
                if ui.add_enabled(!busy && !d.files.is_empty(), egui::Button::new(label)).clicked() {
                    run = true;
                }
                if ui.button("Cancel").clicked() {
                    keep = false;
                }
            });
        });
        if let Some(kind) = pick {
            self.rdp_import = Some(d);
            self.pick(kind);
            return;
        }
        if run {
            let files = std::mem::take(&mut d.files);
            let mut ok = 0;
            for f in files {
                match rdpfile::parse_file(&f, d.folder) {
                    Ok(r) => {
                        for w in &r.warnings {
                            self.push(Level::Warn, w.clone());
                        }
                        let mut conn = r.connection;
                        if !r.username.is_empty() {
                            let existing = self.data.credentials.iter().find(|c| c.username == r.username && c.domain == r.domain).map(|c| c.id);
                            let id = match existing {
                                Some(id) => id,
                                None => {
                                    let name = if r.domain.is_empty() { r.username.clone() } else { format!("{}\\{}", r.domain, r.username) };
                                    let mut c = Credential::new(&name);
                                    c.username = r.username.clone();
                                    c.domain = r.domain.clone();
                                    let id = c.id;
                                    if self.result(self.store.save_credential(&c)).is_some() {
                                        self.data.credentials.push(c);
                                    }
                                    id
                                }
                            };
                            conn.credential = CredentialRef::Id(id);
                        }
                        if self.result(self.store.save_connection(&conn)).is_some() {
                            self.data.connections.push(conn);
                            ok += 1;
                        }
                    }
                    Err(e) => self.push(Level::Warn, format!("{}: {e:#}", f.display())),
                }
            }
            self.push(Level::Info, format!("{ok} .rdp file(s) imported."));
            keep = false;
        }
        if keep {
            self.rdp_import = Some(d);
        }
    }
}
