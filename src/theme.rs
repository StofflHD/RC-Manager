//! Look and feel, shared with the IP Calculator (github.com/StofflHD/IP-Calculator):
//! DejaVu Sans, high-contrast greys, Windows-blue selection and the logo palette.

use egui::{Align2, Color32, FontId, Sense, Stroke, Ui, Vec2};

pub const LOGO_BG: Color32 = Color32::from_rgb(31, 78, 121);
pub const LOGO_LINE: Color32 = Color32::from_rgb(156, 195, 230);
pub const LOGO_ACCENT: Color32 = Color32::from_rgb(245, 166, 35);

/// Uses DejaVu Sans for proportional text; egui's built-in font is a thin
/// light weight. The built-in fonts stay as fallback (symbols, emoji).
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("DejaVuSans".into(), egui::FontData::from_static(include_bytes!("../assets/fonts/DejaVuSans.ttf")));
    fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "DejaVuSans".into());
    ctx.set_fonts(fonts);
}

pub fn apply_style(ctx: &egui::Context, dark: bool) {
    // egui's default greys are soft; text and borders are darkened (light)
    // or brightened (dark) for readability.
    let (mut v, panel, field, stripe, text, strong, border, frame, button) = if dark {
        (egui::Visuals::dark(), 30, 16, 40, 225, 255, 115, 80, 58)
    } else {
        (egui::Visuals::light(), 240, 255, 230, 20, 0, 115, 150, 225)
    };
    let gray = Color32::from_gray;
    v.panel_fill = gray(panel);
    v.window_fill = gray(panel);
    v.extreme_bg_color = gray(field);
    v.faint_bg_color = gray(stripe);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0f32, gray(text));
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0f32, gray(frame));
    v.widgets.inactive.fg_stroke = Stroke::new(1.0f32, gray(text));
    v.widgets.inactive.bg_stroke = Stroke::new(1.0f32, gray(border));
    v.widgets.inactive.bg_fill = gray(button);
    v.widgets.inactive.weak_bg_fill = gray(button);
    v.widgets.hovered.fg_stroke = Stroke::new(1.5f32, gray(strong));
    v.widgets.active.fg_stroke = Stroke::new(2.0f32, gray(strong));
    v.widgets.hovered.bg_stroke = Stroke::new(1.0f32, Color32::from_rgb(0, 120, 215));
    v.error_fg_color = tone(dark, (185, 0, 0), (255, 120, 120));
    v.warn_fg_color = tone(dark, (165, 95, 0), (255, 190, 90));
    v.selection.bg_fill = Color32::from_rgb(0, 102, 204);
    v.selection.stroke = Stroke::new(1.0f32, Color32::WHITE);
    ctx.set_visuals(v);
}

/// Picks the light- or dark-theme variant of a colour.
pub fn tone(dark: bool, light: (u8, u8, u8), dark_rgb: (u8, u8, u8)) -> Color32 {
    let (r, g, b) = if dark { dark_rgb } else { light };
    Color32::from_rgb(r, g, b)
}

/// Blue of the logo family; the dark variant would vanish on a dark panel.
pub fn accent_blue(ui: &Ui) -> Color32 {
    if ui.visuals().dark_mode {
        LOGO_LINE
    } else {
        Color32::from_rgb(46, 117, 182)
    }
}

/// Paints the logo (a monitor with an active session) into a square of side
/// `size` at `min`. Drawn on a 32×32 grid, like `packaging/rc-manager.svg`.
pub fn paint_logo(p: &egui::Painter, min: egui::Pos2, size: f32) {
    let k = size / 32.0;
    let rect = |x: f32, y: f32, w: f32, h: f32| egui::Rect::from_min_size(min + Vec2::new(x, y) * k, Vec2::new(w, h) * k);
    p.rect_filled(rect(0.0, 0.0, 32.0, 32.0), 7.0 * k, LOGO_BG);
    p.rect_filled(rect(5.0, 6.5, 22.0, 15.0), 1.5 * k, LOGO_LINE);
    p.rect_filled(rect(7.0, 8.5, 18.0, 11.0), 0.0, LOGO_BG);
    p.rect_filled(rect(15.1, 21.5, 1.8, 3.0), 0.0, LOGO_LINE);
    p.rect_filled(rect(10.5, 24.5, 11.0, 1.8), 0.0, LOGO_LINE);
    p.circle_filled(min + Vec2::new(13.0, 14.0) * k, 2.8 * k, Color32::WHITE);
    p.rect_filled(rect(17.5, 17.0, 7.0, 7.0), 1.5 * k, LOGO_ACCENT);
}

/// Logo and wordmark ("rc" in blue, "manager" in grey), drawn rather than loaded.
pub fn header(ui: &mut Ui, subtitle: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 52.0), Sense::hover());
    let p = ui.painter_at(rect);
    let font = FontId::proportional(28.0);
    let grey = if ui.visuals().dark_mode { Color32::from_gray(225) } else { Color32::from_gray(55) };
    let blue = accent_blue(ui);
    let g_rc = p.layout_no_wrap("rc".into(), font.clone(), blue);
    let g_mgr = p.layout_no_wrap("manager".into(), font, grey);
    let icon = 40.0;
    let x = rect.left() + 8.0;
    paint_logo(&p, egui::pos2(x, rect.top() + 6.0), icon);
    let tx = x + icon + 10.0;
    let y = rect.top() + 6.0;
    p.galley(egui::pos2(tx, y), g_rc.clone(), blue);
    p.galley(egui::pos2(tx + g_rc.size().x, y), g_mgr, grey);
    p.text(
        egui::pos2(tx + 1.0, rect.bottom() - 8.0),
        Align2::LEFT_CENTER,
        subtitle,
        FontId::proportional(13.0),
        ui.visuals().weak_text_color(),
    );
}

/// Bordered field-coloured frame for lists, as used for the tables in the IP Calculator.
pub fn list_frame<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::none()
        .fill(ui.visuals().extreme_bg_color)
        .stroke(ui.visuals().widgets.inactive.bg_stroke)
        .inner_margin(4.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}
