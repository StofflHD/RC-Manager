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
            /// RDP: `Unlock` is offered; whether a password is stored for it.
            unlock: Option<bool>,
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
                let unlock = session.filter(|s| s.conn.protocol == Protocol::Rdp && t.ended.is_none()).map(|s| {
                    self.connection(s.conn.id).and_then(|c| self.resolve_credential(c)).is_some_and(|k| !k.password.is_empty())
                });
                TabInfo { session: t.session, icon: t.icon, name: t.name.clone(), connection, has_mac, unlock, has_log, ended_conn }
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
                for t in tabs.iter() {
                    let sel = self.active_tab == Some(t.session);
                    let ended = t.ended_conn.is_some();
                    let label = if ended { format!("{} {} ⚠", t.icon, t.name) } else { format!("{} {}", t.icon, t.name) };
                    // Each tab is a chip in a fitting colour (see theme::tab_colors), so the strip
                    // stands off the panel. Laid out by hand to paint the background and border.
                    let font = egui::TextStyle::Button.resolve(ui.style());
                    // PLACEHOLDER lets painter.galley() set the colour per tab state below.
                    let galley = ui.painter().layout_no_wrap(label, font, egui::Color32::PLACEHOLDER);
                    let pad = egui::vec2(9.0, 4.0);
                    let size = galley.size() + 2.0 * pad;
                    // A stable id per session keeps the drag attached while the order changes.
                    let r = ui.push_id(t.session, |ui| ui.allocate_response(size, egui::Sense::click_and_drag())).inner;
                    let (bg, fg, border) = theme::tab_colors(self.s.dark, sel, r.hovered(), ended);
                    let rounding = egui::Rounding { nw: 5.0, ne: 5.0, sw: 0.0, se: 0.0 };
                    ui.painter().rect(r.rect, rounding, bg, border);
                    let text_pos = egui::pos2(r.rect.left() + pad.x, r.rect.center().y - galley.size().y / 2.0);
                    ui.painter().galley(text_pos, galley, fg);
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
                            if let Some(password) = t.unlock {
                                let r = ui
                                    .add_enabled(password, egui::Button::new("🔓 Unlock"))
                                    .on_hover_text("Types the stored password and Enter into the session, e.g. on its lock screen.")
                                    .on_disabled_hover_text("No password is stored for this connection.");
                                if r.clicked() {
                                    actions.push(Action::UnlockTab(t.session));
                                    ui.close_menu();
                                }
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
            let mut create_cred: Option<Credential> = None;
            let mut cancel_cred = false;
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
                            // A nested folder may inherit the set from its parent folder.
                            let inherited = self.inherited_credential(f.parent);
                            Self::credential_combo(ui, "folder_cred", &mut f.credential, &self.data.credentials, f.parent.is_some(), inherited.as_ref());
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
                            ui.horizontal(|ui| {
                                Self::credential_combo(ui, "conn_cred", &mut c.credential, &self.data.credentials, true, inherited.as_ref());
                                let open = self.new_cred.as_ref().is_some_and(|(id, _)| *id == c.id);
                                if ui.add_enabled(!open, egui::Button::new("+ New …")).on_hover_text("Create new credentials for this connection").clicked() {
                                    self.new_cred = Some((c.id, Credential::new(&c.name)));
                                }
                            });
                            ui.end_row();
                            // New credentials, stored and assigned to this connection on "Create".
                            if let Some((_, nc)) = self.new_cred.as_mut().filter(|(id, _)| *id == c.id) {
                                ui.label("");
                                ui.vertical(|ui| ui.group(|ui| {
                                    egui::Grid::new("new_cred_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                                        ui.label("Name");
                                        ui.add(egui::TextEdit::singleline(&mut nc.name).desired_width(220.0));
                                        ui.end_row();
                                        ui.label("User");
                                        ui.add(egui::TextEdit::singleline(&mut nc.username).desired_width(220.0));
                                        ui.end_row();
                                        ui.label("Domain");
                                        ui.add(egui::TextEdit::singleline(&mut nc.domain).hint_text("optional").desired_width(220.0));
                                        ui.end_row();
                                        ui.label("Password");
                                        ui.add(egui::TextEdit::singleline(&mut nc.password).password(true).desired_width(220.0));
                                        ui.end_row();
                                    });
                                    ui.horizontal(|ui| {
                                        let ok = !nc.name.trim().is_empty();
                                        if ui.add_enabled(ok, egui::Button::new("Create")).on_disabled_hover_text("A name is needed.").clicked() {
                                            create_cred = Some(nc.clone());
                                            c.credential = CredentialRef::Id(nc.id);
                                        }
                                        if ui.button("Cancel").clicked() {
                                            cancel_cred = true;
                                        }
                                    });
                                }));
                                ui.end_row();
                            }
                            ui.label("MAC (Wake-on-LAN)");
                            ui.add(egui::TextEdit::singleline(&mut c.mac).hint_text("00:11:22:33:44:55").desired_width(320.0));
                            ui.end_row();
                            ui.label("Description");
                            ui.add(egui::TextEdit::multiline(&mut c.description).desired_width(320.0).desired_rows(3));
                            ui.end_row();
                        });
                        ui.add_space(8.0);
                        Self::protocol_options(ui, c, &self.data.credentials);
                    }
                }
            });
            self.editor = Some(editor);
            if cancel_cred {
                self.new_cred = None;
            }
            if let Some(mut cred) = create_cred {
                cred.name = cred.name.trim().to_string();
                if self.result(self.store.save_credential(&cred)).is_some() {
                    self.push(Level::Info, format!("Credentials “{}” created.", cred.name));
                    self.data.credentials.push(cred);
                    self.new_cred = None;
                }
            }
        }
    }

    pub(super) fn protocol_options(ui: &mut egui::Ui, c: &mut Connection, creds: &[Credential]) {
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
                ui.collapsing("Experience", |ui| {
                    ui.checkbox(&mut o.wallpaper, "Desktop background");
                    ui.checkbox(&mut o.full_window_drag, "Show contents of windows while dragging");
                    ui.checkbox(&mut o.menu_animations, "Menu and window animation");
                    ui.checkbox(&mut o.themes, "Themes");
                    ui.checkbox(&mut o.font_smoothing, "Font smoothing");
                    ui.checkbox(&mut o.bitmap_caching, "Persistent bitmap caching");
                    ui.checkbox(&mut o.auto_detect, "Detect connection quality automatically")
                        .on_hover_text("Client and server adapt the transfer to the connection; this may switch off some of the options above.");
                    ui.checkbox(&mut o.disable_compression, "Disable compression");
                });
                ui.collapsing("Local Resources", |ui| {
                    egui::Grid::new("rdp_res").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                        ui.label("Sound");
                        let sound = |local: bool, remote: bool| match (local, remote) {
                            (true, _) => "On this computer",
                            (false, true) => "On the remote computer",
                            (false, false) => "Do not play",
                        };
                        egui::ComboBox::from_id_salt("rdp_sound").selected_text(sound(o.sound, o.sound_remote)).show_ui(ui, |ui| {
                            for (local, remote) in [(true, false), (false, true), (false, false)] {
                                if ui.selectable_label(sound(o.sound, o.sound_remote) == sound(local, remote), sound(local, remote)).clicked() {
                                    o.sound = local;
                                    o.sound_remote = remote;
                                }
                            }
                        });
                        ui.end_row();
                        ui.label("Key combinations");
                        egui::ComboBox::from_id_salt("rdp_keys")
                            .selected_text(o.shortcuts.label())
                            .show_ui(ui, |ui| {
                                for k in RdpShortcuts::ALL {
                                    ui.selectable_value(&mut o.shortcuts, k, k.label());
                                }
                            })
                            .response
                            .on_hover_text("Where Windows key combinations such as Alt+Tab apply.");
                        ui.end_row();
                    });
                    ui.checkbox(&mut o.clipboard, "Clipboard");
                    ui.checkbox(&mut o.microphone, "Remote audio recording (microphone)");
                    ui.checkbox(&mut o.printers, "Printers");
                    ui.checkbox(&mut o.smartcards, "Smart cards");
                    ui.checkbox(&mut o.redirect_ports, "Ports (COM, LPT)").on_hover_text("Windows RDP client only.");
                    ui.checkbox(&mut o.redirect_pnp, "Plug and Play devices").on_hover_text("Windows RDP client only.");
                    ui.checkbox(&mut o.home_drive, "Share home folder as drive \u{201c}home\u{201d}");
                    ui.label("Additional drives (one per line, name=/path):");
                    let mut drives = o.drives.join("\n");
                    if ui.add(egui::TextEdit::multiline(&mut drives).desired_rows(2).desired_width(320.0)).changed() {
                        o.drives = drives.lines().map(|l| l.to_string()).collect();
                    }
                });
                ui.collapsing("RD Gateway", |ui| {
                    let mut mode = o.gateway_mode();
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut mode, GatewayMode::Auto, "Detect automatically");
                        ui.radio_value(&mut mode, GatewayMode::Off, "Do not use");
                        ui.radio_value(&mut mode, GatewayMode::Manual, "Use these settings");
                    });
                    if mode != o.gateway_mode() {
                        o.gateway_mode = Some(mode);
                    }
                    if mode == GatewayMode::Auto {
                        ui.small("The Windows RDP client finds the gateway itself; FreeRDP connects directly.");
                    }
                    if mode != GatewayMode::Off {
                        egui::Grid::new("rdp_gw").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                            if mode == GatewayMode::Manual {
                                ui.label("Server");
                                ui.add(egui::TextEdit::singleline(&mut o.gateway).desired_width(240.0));
                                ui.end_row();
                            }
                            ui.label("");
                            ui.checkbox(&mut o.gateway_bypass_local, "Bypass for local addresses");
                            ui.end_row();
                            ui.label("Credentials");
                            let text = o
                                .gateway_credential
                                .map(|id| creds.iter().find(|c| c.id == id).map(|c| c.display()).unwrap_or_else(|| "(deleted)".into()))
                                .unwrap_or_else(|| "Same as the connection".into());
                            egui::ComboBox::from_id_salt("rdp_gw_cred").selected_text(text).width(240.0).show_ui(ui, |ui| {
                                ui.selectable_value(&mut o.gateway_credential, None, "Same as the connection");
                                let mut sorted: Vec<&Credential> = creds.iter().collect();
                                sorted.sort_by_key(|c| c.name.to_lowercase());
                                for c in sorted {
                                    ui.selectable_value(&mut o.gateway_credential, Some(c.id), c.display());
                                }
                            });
                            ui.end_row();
                            // Older connections may name a gateway user without stored credentials.
                            if o.gateway_credential.is_none() && !(o.gateway_user.is_empty() && o.gateway_domain.is_empty()) {
                                ui.label("Gateway user");
                                ui.add(egui::TextEdit::singleline(&mut o.gateway_user).desired_width(240.0));
                                ui.end_row();
                                ui.label("Gateway domain");
                                ui.add(egui::TextEdit::singleline(&mut o.gateway_domain).desired_width(240.0));
                                ui.end_row();
                            }
                        });
                    }
                });
                ui.collapsing("Advanced", |ui| {
                    ui.checkbox(&mut o.console, "Admin / console session");
                    ui.checkbox(&mut o.disable_nla, "Do not use Network Level Authentication (NLA)");
                    egui::Grid::new("rdp_adv").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                        ui.label("Server authentication");
                        let auth = |ignore: bool, refuse: bool| match (ignore, refuse) {
                            (true, _) => "Connect without warning (insecure)",
                            (false, true) => "Do not connect",
                            (false, false) => "Warn",
                        };
                        egui::ComboBox::from_id_salt("rdp_auth")
                            .selected_text(auth(o.ignore_certificate, o.refuse_unverified))
                            .width(240.0)
                            .show_ui(ui, |ui| {
                                for (ignore, refuse) in [(false, false), (false, true), (true, false)] {
                                    if ui.selectable_label(auth(o.ignore_certificate, o.refuse_unverified) == auth(ignore, refuse), auth(ignore, refuse)).clicked() {
                                        o.ignore_certificate = ignore;
                                        o.refuse_unverified = refuse;
                                    }
                                }
                            })
                            .response
                            .on_hover_text("What happens when the server's certificate cannot be verified.");
                        ui.end_row();
                        ui.label("Keyboard layout");
                        ui.add(egui::TextEdit::singleline(&mut o.keyboard_layout).hint_text("e.g. 00000409").desired_width(240.0));
                        ui.end_row();
                        ui.label("Start program");
                        ui.add(egui::TextEdit::singleline(&mut o.start_program).hint_text("instead of the desktop").desired_width(240.0));
                        ui.end_row();
                        ui.label("Start in folder");
                        ui.add(egui::TextEdit::singleline(&mut o.start_dir).desired_width(240.0));
                        ui.end_row();
                        ui.label("RemoteApp");
                        ui.add(egui::TextEdit::singleline(&mut o.remote_app).hint_text("published program (runs with FreeRDP)").desired_width(240.0));
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
