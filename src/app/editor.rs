//! Central panel: the session tab strip, the session area and the connection/folder property forms.
//! Split out of `app.rs`; these are `App` methods.

use super::*;

impl App {
    pub(super) fn credential_combo(ui: &mut egui::Ui, id: &str, value: &mut CredentialRef, creds: &[Credential], allow_inherit: bool, inherited: Option<&Credential>) {
        let text = match value {
            CredentialRef::None => "(none)".to_string(),
            CredentialRef::Inherit => format!("Inherit from folder → {}", inherited.map(|c| c.display()).unwrap_or_else(|| "nothing".into())),
            CredentialRef::Id(id) => creds.iter().find(|c| c.id == *id).map(|c| c.display()).unwrap_or_else(|| "(deleted)".into()),
        };
        egui::ComboBox::from_id_salt(id).selected_text(text).width(320.0).show_ui(ui, |ui| {
            ui.selectable_value(value, CredentialRef::None, "(none)");
            if allow_inherit {
                ui.selectable_value(value, CredentialRef::Inherit, "Inherit from folder");
            }
            let mut sorted: Vec<&Credential> = creds.iter().collect();
            sorted.sort_by_key(|c| c.name.to_lowercase());
            for c in sorted {
                ui.selectable_value(value, CredentialRef::Id(c.id), c.display());
            }
        });
    }

    pub(super) fn folder_combo(ui: &mut egui::Ui, id: &str, value: &mut Option<Uuid>, choices: &[(Option<Uuid>, String)]) {
        let text = choices.iter().find(|(i, _)| i == value).map(|(_, n)| n.trim().to_string()).unwrap_or_default();
        egui::ComboBox::from_id_salt(id).selected_text(text).width(320.0).show_ui(ui, |ui| {
            for (i, n) in choices {
                ui.selectable_value(value, *i, n);
            }
        });
    }

    pub(super) fn ui_editor(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        egui::CentralPanel::default().show(ctx, |ui| {
            self.session_rect = None;
            if !self.tabs.is_empty() {
                self.tab_strip(ui, actions);
                ui.add_space(2.0);
            }
            if !self.active_tab.is_some_and(|id| self.tabs.iter().any(|t| t.session == id)) {
                self.active_tab = self.tabs.first().map(|t| t.session);
            }
            match self.active_tab {
                Some(id) => self.session_area(ui, id, actions),
                None => {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new("Double-click a connection to connect.\nRight-click an entry for its properties and more.").weak(),
                        )
                    });
                }
            }
        });
    }

    pub(super) fn tab_strip(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        // Snapshot so the loop does not borrow `self` and can offer the connection's actions.
        struct TabInfo {
            session: u64,
            icon: &'static str,
            name: String,
            /// The connection it belongs to, if it still exists (for the context menu).
            connection: Option<Uuid>,
            has_mac: bool,
            /// The session is recorded, so its history can be copied.
            has_log: bool,
            /// The session ended on its own; the connection to reconnect, if it still exists.
            ended_conn: Option<Option<Uuid>>,
        }
        let tabs: Vec<TabInfo> = self
            .tabs
            .iter()
            .map(|t| {
                let session = self.sessions.get(&t.session);
                let connection = session.map(|s| s.conn.id).filter(|id| self.connection(*id).is_some());
                let has_mac = connection.and_then(|id| self.connection(id)).is_some_and(|c| !c.mac.trim().is_empty());
                let has_log = session.is_some_and(|s| s.log.is_some());
                let ended_conn = t.ended.as_ref().map(|e| e.conn.filter(|id| self.connection(*id).is_some()));
                TabInfo { session: t.session, icon: t.icon, name: t.name.clone(), connection, has_mac, has_log, ended_conn }
            })
            .collect();
        let mut close = None;
        let mut activate = None;
        // Drag to reorder: rectangles per tab (in list order), the tab being dragged and,
        // on release, the move to apply to `self.tabs`.
        let mut rects: Vec<egui::Rect> = Vec::new();
        let mut dragging: Option<u64> = None;
        let mut released: Option<u64> = None;
        let mut reorder: Option<(usize, usize)> = None;
        egui::ScrollArea::horizontal().id_salt("session_tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (i, t) in tabs.iter().enumerate() {
                    if i > 0 {
                        ui.separator();
                    }
                    let sel = self.active_tab == Some(t.session);
                    let label = if t.ended_conn.is_some() { format!("{} {} ⚠", t.icon, t.name) } else { format!("{} {}", t.icon, t.name) };
                    let text = RichText::new(label);
                    let text = if t.ended_conn.is_some() { text.weak() } else { text };
                    // A stable id per session keeps the drag attached while the order changes.
                    let r = ui.push_id(t.session, |ui| ui.add(egui::SelectableLabel::new(sel, if sel { text.strong() } else { text }))).inner;
                    let r = r.interact(egui::Sense::click_and_drag());
                    rects.push(r.rect);
                    if r.dragged() {
                        dragging = Some(t.session);
                    }
                    if r.drag_stopped() {
                        released = Some(t.session);
                    }
                    if r.clicked() {
                        activate = Some(t.session);
                    }
                    if r.middle_clicked() {
                        close = Some(t.session);
                    }
                    // The connection's actions, like the tree's context menu; the session is
                    // running, so "Connect" reads "Reconnect".
                    r.context_menu(|ui| {
                        if let Some(conn) = t.ended_conn {
                            if ui.add_enabled(conn.is_some(), egui::Button::new("⟳ Reconnect")).clicked() {
                                actions.push(Action::ReconnectTab(t.session));
                                ui.close_menu();
                            }
                            ui.separator();
                        } else if let Some(id) = t.connection {
                            if ui.button("⟳ Reconnect").clicked() {
                                actions.push(Action::Reconnect(id));
                                ui.close_menu();
                            }
                            if ui.button("✏ Properties …").clicked() {
                                actions.push(Action::Edit(Sel::Connection(id)));
                                ui.close_menu();
                            }
                            if ui.button("Duplicate").clicked() {
                                actions.push(Action::Duplicate(id));
                                ui.close_menu();
                            }
                            if ui.button("Check Reachability").clicked() {
                                actions.push(Action::CheckPort(id));
                                ui.close_menu();
                            }
                            if ui.add_enabled(t.has_mac, egui::Button::new("Wake-on-LAN")).clicked() {
                                actions.push(Action::Wake(id));
                                ui.close_menu();
                            }
                            ui.separator();
                        }
                        if t.has_log && ui.button("📋 Copy session history").on_hover_text("Copy the whole session output to the clipboard").clicked() {
                            actions.push(Action::CopySessionLog(t.session));
                            ui.close_menu();
                        }
                        if ui.button("✖ Close session").clicked() {
                            close = Some(t.session);
                            ui.close_menu();
                        }
                    });
                    if ui.small_button("✖").on_hover_text("Close session").clicked() {
                        close = Some(t.session);
                    }
                }
                // Drag-to-reorder feedback and the drop position (pointer vs the tab centres).
                let pointer = ui.ctx().pointer_interact_pos();
                let target_of = |p: egui::Pos2| rects.iter().position(|r| p.x < r.center().x).unwrap_or(rects.len());
                if dragging.is_some() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                    if let (Some(p), Some(first)) = (pointer, rects.first()) {
                        let t = target_of(p);
                        let x = if t == 0 { first.left() } else { rects.get(t - 1).map(|r| r.right()).unwrap_or(first.right()) };
                        ui.painter().vline(x, first.y_range(), egui::Stroke::new(2.0_f32, theme::accent(self.s.dark)));
                    }
                }
                if let (Some(session), Some(p)) = (released, pointer) {
                    if let Some(from) = tabs.iter().position(|t| t.session == session) {
                        let mut to = target_of(p);
                        if from < to {
                            to -= 1;
                        }
                        let to = to.min(tabs.len().saturating_sub(1));
                        if to != from {
                            reorder = Some((from, to));
                        }
                    }
                }
            });
        });
        if let Some((from, to)) = reorder {
            if from < self.tabs.len() {
                let tab = self.tabs.remove(from);
                self.tabs.insert(to.min(self.tabs.len()), tab);
            }
        }
        if let Some(id) = activate {
            self.active_tab = Some(id);
            self.session_focus = None;
        }
        if let Some(id) = close {
            actions.push(Action::CloseTab(id));
        }
    }

    /// Reserves the area the active session's window is placed over.
    pub(super) fn session_area(&mut self, ui: &mut egui::Ui, id: u64, actions: &mut Vec<Action>) {
        let rect = ui.available_rect_before_wrap();
        let (bg, _) = theme::session_colors(self.s.dark);
        ui.painter().rect_filled(rect, 0.0, bg);
        // A tab whose session ended on its own: no window is placed here any more; offer to
        // reconnect or close instead of reserving the area for embedding.
        if let Some((name, reason, can_reconnect)) = self.tabs.iter().find(|t| t.session == id).and_then(|t| {
            t.ended.as_ref().map(|e| (t.name.clone(), e.reason.clone(), e.conn.is_some_and(|c| self.connection(c).is_some())))
        }) {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() / 2.0 - 48.0).max(12.0));
                ui.label(RichText::new(format!("“{name}” disconnected")).strong().size(15.0));
                ui.label(RichText::new(reason).weak());
                ui.add_space(14.0);
                if ui.add_enabled(can_reconnect, egui::Button::new(RichText::new("⟳ Reconnect").strong())).clicked() {
                    actions.push(Action::ReconnectTab(id));
                }
                ui.add_space(4.0);
                if ui.button("✖ Close tab").clicked() {
                    actions.push(Action::CloseTab(id));
                }
            });
            return;
        }
        ui.allocate_rect(rect, egui::Sense::hover());
        let waiting = self.tabs.iter().find(|t| t.session == id).map(|t| t.adopt.is_some()).unwrap_or(false);
        let text = if waiting { "Waiting for the session window …" } else { "Connecting …" };
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, egui::FontId::proportional(14.0), ui.visuals().weak_text_color());
        self.session_rect = Some(rect);
    }

    pub(super) fn editor_contents(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        {
            let Some(mut editor) = self.editor.clone() else {
                ui.centered_and_justified(|ui| ui.label(RichText::new("Select a connection or folder.\nDouble-click connects.").weak()));
                return;
            };
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                match &mut editor {
                    Editor::Folder(f) => {
                        ui.heading(RichText::new(format!("🗀 {}", f.name)).color(theme::accent(ui.visuals().dark_mode)));
                        ui.add_space(8.0);
                        let choices = self.folder_choices(Some(f.id));
                        egui::Grid::new("folder_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                            ui.label("Name");
                            ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(320.0));
                            ui.end_row();
                            ui.label("Parent folder");
                            Self::folder_combo(ui, "folder_parent", &mut f.parent, &choices);
                            ui.end_row();
                            ui.label("Credentials for\nconnections inside");
                            let mut r = f.credential.map(CredentialRef::Id).unwrap_or_default();
                            Self::credential_combo(ui, "folder_cred", &mut r, &self.data.credentials, false, None);
                            f.credential = match r {
                                CredentialRef::Id(id) => Some(id),
                                _ => None,
                            };
                            ui.end_row();
                            ui.label("Sort order");
                            ui.add(egui::DragValue::new(&mut f.sorting));
                            ui.end_row();
                            ui.label("Description");
                            ui.add(egui::TextEdit::multiline(&mut f.description).desired_width(320.0).desired_rows(4));
                            ui.end_row();
                        });
                    }
                    Editor::Connection(c) => {
                        ui.horizontal(|ui| {
                            ui.heading(RichText::new(format!("{} {}", c.protocol.icon(), c.name)).color(theme::accent(ui.visuals().dark_mode)));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(egui::Button::new(RichText::new("▶ Connect").strong())).clicked() {
                                    actions.push(Action::Connect(c.id));
                                }
                            });
                        });
                        if let Some(t) = &c.last_connected {
                            ui.label(RichText::new(format!("Last connected: {t}")).weak());
                        }
                        ui.add_space(8.0);
                        let choices = self.folder_choices(None);
                        let inherited = {
                            let mut probe = c.clone();
                            probe.credential = CredentialRef::Inherit;
                            self.resolve_credential(&probe)
                        };
                        egui::Grid::new("conn_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                            ui.label("Name");
                            ui.add(egui::TextEdit::singleline(&mut c.name).desired_width(320.0));
                            ui.end_row();
                            ui.label("Protocol");
                            egui::ComboBox::from_id_salt("conn_proto").selected_text(c.protocol.label()).width(320.0).show_ui(ui, |ui| {
                                for p in Protocol::ALL {
                                    ui.selectable_value(&mut c.protocol, p, p.label());
                                }
                            });
                            ui.end_row();
                            ui.label(if c.protocol == Protocol::Web { "URL" } else { "Host" });
                            ui.add(egui::TextEdit::singleline(&mut c.host).desired_width(320.0));
                            ui.end_row();
                            if c.protocol != Protocol::External {
                                ui.label("Port");
                                ui.horizontal(|ui| {
                                    ui.add(egui::DragValue::new(&mut c.port).range(0..=65535));
                                    ui.label(RichText::new(format!("0 = default ({})", c.protocol.default_port())).weak());
                                });
                                ui.end_row();
                            }
                            ui.label("Folder");
                            Self::folder_combo(ui, "conn_parent", &mut c.parent, &choices);
                            ui.end_row();
                            ui.label("Credentials");
                            Self::credential_combo(ui, "conn_cred", &mut c.credential, &self.data.credentials, true, inherited.as_ref());
                            ui.end_row();
                            ui.label("MAC (Wake-on-LAN)");
                            ui.add(egui::TextEdit::singleline(&mut c.mac).hint_text("00:11:22:33:44:55").desired_width(320.0));
                            ui.end_row();
                            ui.label("Description");
                            ui.add(egui::TextEdit::multiline(&mut c.description).desired_width(320.0).desired_rows(3));
                            ui.end_row();
                        });
                        ui.add_space(8.0);
                        Self::protocol_options(ui, c);
                    }
                }
            });
            self.editor = Some(editor);
        }
    }

    pub(super) fn protocol_options(ui: &mut egui::Ui, c: &mut Connection) {
        match c.protocol {
            Protocol::Rdp => {
                let o = &mut c.options.rdp;
                ui.collapsing("Display", |ui| {
                    egui::Grid::new("rdp_disp").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                        ui.label("Mode");
                        egui::ComboBox::from_id_salt("rdp_mode").selected_text(o.window_mode.label()).show_ui(ui, |ui| {
                            for m in [WindowMode::Tab, WindowMode::Window, WindowMode::Fullscreen, WindowMode::MultiMonitor] {
                                ui.selectable_value(&mut o.window_mode, m, m.label());
                            }
                        });
                        ui.end_row();
                        ui.label("Resolution");
                        ui.add(egui::TextEdit::singleline(&mut o.resolution).hint_text("e.g. 1920x1080").desired_width(120.0));
                        ui.end_row();
                        ui.label("Colour depth");
                        let depth = if o.color_depth == 0 { "Default".into() } else { format!("{} bit", o.color_depth) };
                        egui::ComboBox::from_id_salt("rdp_bpp").selected_text(depth).show_ui(ui, |ui| {
                            ui.selectable_value(&mut o.color_depth, 0, "Default");
                            for b in [8u8, 15, 16, 24, 32] {
                                ui.selectable_value(&mut o.color_depth, b, format!("{b} bit"));
                            }
                        });
                        ui.end_row();
                        ui.label("Graphics");
                        egui::ComboBox::from_id_salt("rdp_gfx").selected_text(o.graphics.label()).show_ui(ui, |ui| {
                            for g in RdpGraphics::ALL {
                                ui.selectable_value(&mut o.graphics, g, g.label());
                            }
                        });
                        ui.end_row();
                    });
                    ui.small("Graphics acceleration smooths photos and video. Use “Bitmap” only if the server does not support the newer modes.");
                    ui.checkbox(&mut o.dynamic_resolution, "Adapt the remote resolution to the window size")
                        .on_hover_text("Needs server support (Windows 8.1 / Server 2012 R2 or newer). When off, the desktop is scaled to the window.");
                });
                ui.collapsing("Local Resources", |ui| {
                    ui.checkbox(&mut o.clipboard, "Clipboard");
                    ui.checkbox(&mut o.sound, "Play audio locally");
                    ui.checkbox(&mut o.microphone, "Microphone");
                    ui.checkbox(&mut o.printers, "Printers");
                    ui.checkbox(&mut o.smartcards, "Smart cards");
                    ui.checkbox(&mut o.home_drive, "Share home folder as drive “home”");
                    ui.label("Additional drives (one per line, name=/path):");
                    let mut drives = o.drives.join("\n");
                    if ui.add(egui::TextEdit::multiline(&mut drives).desired_rows(2).desired_width(320.0)).changed() {
                        o.drives = drives.lines().map(|l| l.to_string()).collect();
                    }
                });
                ui.collapsing("Advanced", |ui| {
                    ui.checkbox(&mut o.console, "Admin / console session");
                    ui.checkbox(&mut o.ignore_certificate, "Do not verify certificate (insecure)");
                    egui::Grid::new("rdp_adv").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                        ui.label("RD Gateway");
                        ui.add(egui::TextEdit::singleline(&mut o.gateway).desired_width(240.0));
                        ui.end_row();
                        ui.label("Gateway user");
                        ui.add(egui::TextEdit::singleline(&mut o.gateway_user).hint_text("empty = same as connection").desired_width(240.0));
                        ui.end_row();
                        ui.label("Gateway domain");
                        ui.add(egui::TextEdit::singleline(&mut o.gateway_domain).desired_width(240.0));
                        ui.end_row();
                        ui.label("Keyboard layout");
                        ui.add(egui::TextEdit::singleline(&mut o.keyboard_layout).hint_text("e.g. 00000409").desired_width(240.0));
                        ui.end_row();
                        ui.label("RemoteApp / program");
                        ui.add(egui::TextEdit::singleline(&mut o.remote_app).desired_width(240.0));
                        ui.end_row();
                        ui.label("Extra arguments");
                        ui.add(egui::TextEdit::singleline(&mut o.extra_args).hint_text("FreeRDP options").desired_width(240.0));
                        ui.end_row();
                    });
                });
            }
            Protocol::Ssh => {
                let o = &mut c.options.ssh;
                ui.collapsing("SSH", |ui| {
                    ui.checkbox(&mut o.x11_forwarding, "X11 forwarding (-X)");
                    ui.checkbox(&mut o.compression, "Compression (-C)");
                    egui::Grid::new("ssh_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                        ui.label("Identity file");
                        ui.add(egui::TextEdit::singleline(&mut o.identity_file).hint_text("~/.ssh/id_ed25519").desired_width(240.0));
                        ui.end_row();
                        ui.label("Remote command");
                        ui.add(egui::TextEdit::singleline(&mut o.remote_command).desired_width(240.0));
                        ui.end_row();
                        ui.label("Extra arguments");
                        ui.add(egui::TextEdit::singleline(&mut o.extra_args).hint_text("e.g. -L 8080:localhost:80").desired_width(240.0));
                        ui.end_row();
                    });
                });
            }
            Protocol::Vnc => {
                let o = &mut c.options.vnc;
                ui.collapsing("VNC", |ui| {
                    ui.checkbox(&mut o.view_only, "View only");
                    ui.checkbox(&mut o.fullscreen, "Full screen");
                    ui.checkbox(&mut o.shared, "Shared session");
                    ui.horizontal(|ui| {
                        ui.label("Extra arguments");
                        ui.add(egui::TextEdit::singleline(&mut o.extra_args).desired_width(240.0));
                    });
                });
            }
            Protocol::External => {
                let o = &mut c.options.external;
                ui.collapsing("Program", |ui| {
                    egui::Grid::new("ext_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                        ui.label("Command line");
                        ui.add(egui::TextEdit::singleline(&mut o.command).hint_text("remmina -c rdp://{U}@{T}").desired_width(320.0));
                        ui.end_row();
                        ui.label("Working directory");
                        ui.add(egui::TextEdit::singleline(&mut o.working_dir).desired_width(320.0));
                        ui.end_row();
                    });
                    ui.label(RichText::new("Placeholders: {T} host, {PORT} port, {N} name, {U} user, {D} domain, {P} password").weak());
                });
            }
            Protocol::Web => {}
        }
    }
}
