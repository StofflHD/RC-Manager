//! UI colour scheme ("Rust"), shared with notepad-rs (github.com/StofflHD/notepad-rs) and the
//! IP Calculator: warm neutrals with the orange of the logo as accent, in a dark and a light
//! variant. Plus the header with logo and wordmark.

use egui::{Align2, Color32, FontId, Sense, Stroke, Ui, Vec2, Visuals};

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

struct Palette {
    text: Color32,
    /// Supporting text (`.weak()`).
    weak: Color32,
    strong: Color32,
    panel: Color32,
    window: Color32,
    extreme: Color32,
    faint: Color32,
    border: Color32,
    button: Color32,
    hover: Color32,
    active: Color32,
    accent: Color32,
    selection: Color32,
    selection_text: Color32,
    link: Color32,
    warn: Color32,
    error: Color32,
}

const DARK: Palette = Palette {
    text: rgb(242, 237, 232),
    weak: rgb(184, 175, 166),
    strong: rgb(255, 255, 255),
    panel: rgb(38, 34, 31),
    window: rgb(44, 40, 37),
    extreme: rgb(27, 25, 23),
    faint: rgb(50, 45, 41),
    border: rgb(78, 70, 63),
    button: rgb(61, 54, 49),
    hover: rgb(78, 68, 60),
    active: rgb(96, 80, 68),
    accent: rgb(240, 119, 58),
    selection: rgb(122, 46, 18),
    selection_text: rgb(255, 244, 236),
    link: rgb(246, 156, 108),
    warn: rgb(240, 177, 58),
    error: rgb(255, 110, 92),
};

const LIGHT: Palette = Palette {
    text: rgb(31, 27, 24),
    weak: rgb(94, 85, 77),
    strong: rgb(0, 0, 0),
    panel: rgb(247, 244, 240),
    window: rgb(251, 249, 246),
    extreme: rgb(255, 255, 255),
    faint: rgb(240, 235, 229),
    border: rgb(214, 205, 196),
    button: rgb(233, 226, 218),
    hover: rgb(224, 214, 203),
    active: rgb(212, 198, 184),
    accent: rgb(184, 65, 27),
    selection: rgb(246, 201, 176),
    selection_text: rgb(90, 30, 8),
    link: rgb(168, 56, 15),
    warn: rgb(154, 91, 0),
    error: rgb(192, 38, 27),
};

/// Accent colour (logo orange, darker in light mode for contrast).
pub fn accent(dark: bool) -> Color32 {
    if dark {
        DARK.accent
    } else {
        LIGHT.accent
    }
}

pub fn visuals(dark: bool) -> Visuals {
    let p = if dark { &DARK } else { &LIGHT };
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    // egui draws weak text as the 50 % mix of the text colour and this colour; pick it so that the
    // mix is exactly `weak` (disabled widgets fade towards it too).
    let fade = Color32::from_rgb(
        (2 * p.weak.r() as i16 - p.text.r() as i16).clamp(0, 255) as u8,
        (2 * p.weak.g() as i16 - p.text.g() as i16).clamp(0, 255) as u8,
        (2 * p.weak.b() as i16 - p.text.b() as i16).clamp(0, 255) as u8,
    );
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.window;
    w.noninteractive.weak_bg_fill = fade;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.inactive.bg_fill = p.button;
    w.inactive.weak_bg_fill = p.button;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.hovered.bg_fill = p.hover;
    w.hovered.weak_bg_fill = p.hover;
    w.hovered.bg_stroke = Stroke::new(1.0_f32, p.accent);
    w.hovered.fg_stroke = Stroke::new(1.5_f32, p.strong);
    w.active.bg_fill = p.active;
    w.active.weak_bg_fill = p.active;
    w.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    w.active.fg_stroke = Stroke::new(2.0_f32, p.strong);
    w.open.bg_fill = p.button;
    w.open.weak_bg_fill = p.button;
    w.open.bg_stroke = Stroke::new(1.0_f32, p.border);
    w.open.fg_stroke = Stroke::new(1.0_f32, p.text);
    v.selection.bg_fill = p.selection;
    v.selection.stroke = Stroke::new(1.0_f32, p.selection_text);
    v.hyperlink_color = p.link;
    v.faint_bg_color = p.faint;
    v.extreme_bg_color = p.extreme;
    v.code_bg_color = p.faint;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.error;
    v.window_fill = p.window;
    v.window_stroke = Stroke::new(1.0_f32, p.border);
    v.panel_fill = p.panel;
    v.text_cursor.stroke = Stroke::new(2.0_f32, p.accent);
    v
}

pub fn apply(ctx: &egui::Context, dark: bool) {
    ctx.set_visuals(visuals(dark));
}

/// Background behind embedded session windows and the colours of embedded terminals:
/// (background, text).
pub fn session_colors(dark: bool) -> (Color32, Color32) {
    if dark {
        (DARK.extreme, DARK.text)
    } else {
        (LIGHT.extreme, LIGHT.text)
    }
}

/// The application icon (`packaging/make_icon.py`), loaded once per context.
fn logo(ctx: &egui::Context) -> egui::TextureHandle {
    let id = egui::Id::new("rc-manager-logo");
    if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return t;
    }
    let img = eframe::icon_data::from_png_bytes(include_bytes!("../packaging/rc-manager.png")).expect("embedded icon");
    let ci = egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.rgba);
    let t = ctx.load_texture("logo", ci, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    t
}

/// Paints the logo into a square of side `size` at `min`.
pub fn paint_logo(p: &egui::Painter, min: egui::Pos2, size: f32) {
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    p.image(logo(p.ctx()).id(), egui::Rect::from_min_size(min, Vec2::splat(size)), uv, Color32::WHITE);
}

/// Logo and wordmark ("rc" in the accent colour, "manager" in the text colour).
pub fn header(ui: &mut Ui, subtitle: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 52.0), Sense::hover());
    let p = ui.painter_at(rect);
    let font = FontId::proportional(28.0);
    let text = ui.visuals().text_color();
    let accent = accent(ui.visuals().dark_mode);
    let g_rc = p.layout_no_wrap("rc".into(), font.clone(), accent);
    let g_mgr = p.layout_no_wrap("manager".into(), font, text);
    let icon = 40.0;
    let x = rect.left() + 8.0;
    paint_logo(&p, egui::pos2(x, rect.top() + 6.0), icon);
    let tx = x + icon + 10.0;
    let y = rect.top() + 6.0;
    p.galley(egui::pos2(tx, y), g_rc.clone(), accent);
    p.galley(egui::pos2(tx + g_rc.size().x, y), g_mgr, text);
    p.text(
        egui::pos2(tx + 1.0, rect.bottom() - 8.0),
        Align2::LEFT_CENTER,
        subtitle,
        FontId::proportional(13.0),
        ui.visuals().weak_text_color(),
    );
}

/// Bordered field-coloured frame for lists.
pub fn list_frame<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::none()
        .fill(ui.visuals().extreme_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .inner_margin(4.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG 2 contrast ratio.
    fn contrast(a: Color32, b: Color32) -> f32 {
        let lum = |c: Color32| {
            let f = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        };
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn readable_in_both_modes() {
        for dark in [true, false] {
            let v = visuals(dark);
            let mode = if dark { "dark" } else { "light" };
            for bg in [v.panel_fill, v.window_fill, v.extreme_bg_color, v.widgets.inactive.bg_fill] {
                assert!(contrast(v.text_color(), bg) >= 7.0, "{mode}: text {:.1}", contrast(v.text_color(), bg));
                assert!(contrast(v.weak_text_color(), bg) >= 4.5, "{mode}: weak {:.1}", contrast(v.weak_text_color(), bg));
            }
            assert!(contrast(v.widgets.hovered.fg_stroke.color, v.widgets.hovered.bg_fill) >= 7.0, "{mode}: hovered");
            assert!(contrast(v.widgets.active.fg_stroke.color, v.widgets.active.bg_fill) >= 7.0, "{mode}: active");
            assert!(contrast(v.selection.stroke.color, v.selection.bg_fill) >= 7.0, "{mode}: selection");
            assert!(contrast(v.hyperlink_color, v.window_fill) >= 4.5, "{mode}: link");
            assert!(contrast(accent(dark), v.window_fill) >= 4.5, "{mode}: accent");
            // The weak colour is exactly what the palette asks for.
            let p = if dark { &DARK } else { &LIGHT };
            let wk = v.weak_text_color();
            assert!((wk.r() as i16 - p.weak.r() as i16).abs() <= 1 && (wk.g() as i16 - p.weak.g() as i16).abs() <= 1, "{mode}: {wk:?}");
        }
    }
}
