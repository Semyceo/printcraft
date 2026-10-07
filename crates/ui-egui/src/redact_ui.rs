//! Redact a PDF (execution plan M8.5–M8.6): mark text and areas with the Redact tool (drag across
//! text to mark it, drag elsewhere to mark a box), mark whole pages, find text or patterns and
//! mark every match, set the default box colour and overlay text, and apply all marks (with a
//! confirmation, as Acrobat asks) or clear them.

use egui::{Color32, CornerRadius, Pos2, Rect, Stroke};
use printcraft_engine::{Edit, NewAnnotation, REDACT_PATTERNS, RedactPattern, Rgb, Shape, Style, rect_quad};
use printcraft_render::DocInfo;

use crate::canvas::{DocView, PageXform};
use crate::theme::Tokens;
use crate::{PrintCraftApp, widgets};

const MARK_RED: Color32 = Color32::from_rgb(0xE3, 0x22, 0x22);

/// Redaction Tool Properties: what new marks look like once applied.
#[derive(Clone, Debug, PartialEq)]
pub struct RedactPrefs {
    /// Box colour (`None` = no box, the content just disappears).
    pub fill: Option<Rgb>,
    pub use_overlay: bool,
    pub overlay: String,
    /// Overlay text font, size (0 = auto), colour, alignment and repetition.
    pub look: printcraft_engine::OverlayLook,
}

impl Default for RedactPrefs {
    fn default() -> Self {
        Self { fill: Some([0.0, 0.0, 0.0]), use_overlay: false, overlay: String::new(), look: Default::default() }
    }
}

impl RedactPrefs {
    pub fn mark(&self, page: usize, quads: Vec<[f64; 8]>, author: &str) -> Edit {
        let shape = Shape::Redact { quads, overlay: if self.use_overlay { self.overlay.clone() } else { String::new() }, look: self.look };
        let mut style = Style::default_for(&shape);
        style.fill = self.fill;
        Edit::AddAnnotation(NewAnnotation { page, shape, style, contents: String::new(), author: author.to_string() })
    }
}

/// Redact pages: which pages to mark.
#[derive(Clone, Debug, PartialEq)]
pub struct PagesDraft {
    pub current: bool,
    pub from: usize,
    pub to: usize,
}

impl Default for PagesDraft {
    fn default() -> Self {
        Self { current: true, from: 1, to: 1 }
    }
}

/// Find text and redact.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchDraft {
    pub patterns: bool,
    pub text: String,
    pub pattern: RedactPattern,
    /// The result of the last search (matches marked), shown in the dialog.
    pub found: Option<usize>,
}

impl Default for SearchDraft {
    fn default() -> Self {
        Self { patterns: false, text: String::new(), pattern: RedactPattern::Phone, found: None }
    }
}

/// A box being drawn with the Redact tool: (page, start).
pub type AreaDrag = Option<(usize, Pos2)>;

fn to_user(xf: &PageXform, info: &DocInfo, page: usize, p: Pos2) -> [f64; 2] {
    let (vx, vy) = xf.screen_to_view(p);
    let u = info.pages[page].view_to_user(vx, vy);
    [u[0] as f64, u[1] as f64]
}

/// Redact tool input on a page. Presses on text are left to text selection (the selection is
/// marked when it ends, see [`after_text`]); presses elsewhere draw a box. Returns `true` when
/// the gesture is a box.
pub(crate) fn page_input(
    ui: &egui::Ui,
    resp: &egui::Response,
    xf: &PageXform,
    page: usize,
    info: &DocInfo,
    over_text: impl Fn(Pos2) -> bool,
    view: &mut DocView,
) -> bool {
    let pointer = ui.input(|i| i.pointer.hover_pos().or(i.pointer.interact_pos()));
    if let Some((dp, start)) = view.redact_drag
        && dp == page
    {
        if resp.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
            view.redact_drag = None;
            let end = pointer.unwrap_or(start);
            let r = Rect::from_two_pos(start, end).intersect(xf.rect);
            if r.width() >= 3.0 && r.height() >= 3.0 {
                let (a, b) = (to_user(xf, info, page, r.min), to_user(xf, info, page, r.max));
                view.pending_redaction = Some((page, vec![rect_quad([a[0], a[1], b[0], b[1]])]));
            }
        }
        return true;
    }
    let Some(p) = pointer.filter(|p| xf.rect.contains(*p)) else { return false };
    if !over_text(p) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if resp.drag_started() {
        let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(p);
        if !over_text(origin) {
            view.redact_drag = Some((page, origin));
            return true;
        }
    }
    false
}

/// A finished text selection with the Redact tool becomes a mark.
pub(crate) fn after_text(resp: &egui::Response, page: usize, info: &DocInfo, view: &mut DocView) {
    if !(resp.drag_stopped() || resp.double_clicked()) || view.redact_drag.is_some() {
        return;
    }
    if let Some((p, quads)) = view.selection_quads(info).filter(|(p, _)| *p == page) {
        view.clear_selection();
        view.pending_redaction = Some((p, quads));
    }
}

/// The box being drawn.
pub(crate) fn paint(ui: &egui::Ui, painter: &egui::Painter, page: usize, view: &DocView) {
    if let (Some((dp, start)), Some(p)) = (view.redact_drag, ui.input(|i| i.pointer.hover_pos()))
        && dp == page
    {
        let r = Rect::from_two_pos(start, p);
        painter.rect_filled(r, CornerRadius::ZERO, MARK_RED.gamma_multiply(0.12));
        painter.rect_stroke(r, CornerRadius::ZERO, Stroke::new(1.5, MARK_RED), egui::StrokeKind::Inside);
    }
}

impl PrintCraftApp {
    /// Mark every match of the search draft on every page; returns how many were marked.
    pub fn redact_search(&mut self) -> usize {
        let Some((_, id)) = self.active_ids() else { return 0 };
        let Some(doc) = self.session.get(id) else { return 0 };
        let d = self.redact_search.clone();
        if !d.patterns && d.text.trim().is_empty() {
            return 0;
        }
        let config = printcraft_render::RenderConfig { password: doc.password.as_deref().map(std::sync::Arc::from), ..Default::default() };
        let mut r = printcraft_render::PageRenderer::new(doc.bytes.clone(), config);
        let mut edits = Vec::new();
        let author = self.comment_prefs.author.clone();
        for page in 0..doc.info.pages.len() {
            let out =
                r.render(printcraft_render::RenderRequest { page, kind: printcraft_render::RequestKind::Text, scale: 1.0, ..Default::default() });
            let Some(text) = out.text else { continue };
            let hits = if d.patterns { text.find_with(|c| printcraft_engine::find_pattern(d.pattern, c)) } else { text.find(&d.text) };
            for h in hits {
                let quads: Vec<[f64; 8]> = text.line_rects(h).into_iter().map(|r| doc.info.pages[page].view_rect_to_quad(r)).collect();
                if !quads.is_empty() {
                    edits.push(self.redact_prefs.mark(page, quads, &author));
                }
            }
        }
        let n = edits.len();
        if n > 0 {
            self.apply_edit(Edit::Batch { label: "Mark for redaction".into(), edits });
        }
        n
    }

    /// Mark the pages of the Redact pages draft.
    pub fn redact_pages(&mut self) {
        let Some((i, id)) = self.active_ids() else { return };
        let Some(doc) = self.session.get(id) else { return };
        let n = doc.info.pages.len();
        let d = self.redact_pages_draft.clone();
        let pages: Vec<usize> = if d.current { vec![self.views[i].current] } else { (d.from.max(1) - 1..d.to.min(n)).collect() };
        let author = self.comment_prefs.author.clone();
        let edits: Vec<Edit> = pages
            .iter()
            .map(|&p| {
                let c = doc.info.pages[p].crop;
                self.redact_prefs.mark(p, vec![rect_quad([c[0] as f64, c[1] as f64, c[2] as f64, c[3] as f64])], &author)
            })
            .collect();
        match <[Edit; 1]>::try_from(edits) {
            Ok([one]) => {
                self.apply_edit(one);
            }
            Err(edits) if edits.is_empty() => {}
            Err(edits) => {
                self.apply_edit(Edit::Batch { label: "Mark pages for redaction".into(), edits });
            }
        }
    }
}

/// Redact pages ("Mark Page Range"). Returns (apply, cancel).
pub(crate) fn pages_body(ui: &mut egui::Ui, d: &mut PagesDraft, pages: usize, _t: &Tokens, lang: crate::i18n::Language) -> (bool, bool) {
    ui.label(egui::RichText::new(lang.tr("Mark Page Range")).font(crate::theme::semibold(18.0)));
    ui.add_space(8.0);
    ui.radio_value(&mut d.current, true, lang.tr("Current page"));
    ui.horizontal(|ui| {
        ui.radio_value(&mut d.current, false, lang.tr("Pages from"));
        ui.add_enabled(!d.current, egui::DragValue::new(&mut d.from).range(1..=pages));
        ui.label(lang.tr("to"));
        ui.add_enabled(!d.current, egui::DragValue::new(&mut d.to).range(1..=pages));
        let of_label = if lang == crate::i18n::Language::Fr { format!("sur {pages}") } else { format!("of {pages}") };
        ui.label(of_label);
    });
    d.to = d.to.max(d.from);
    ui.add_space(12.0);
    buttons(ui, lang.tr("OK"), true, lang)
}

/// Find text and redact. Returns (search, cancel).
pub(crate) fn search_body(ui: &mut egui::Ui, d: &mut SearchDraft, t: &Tokens, lang: crate::i18n::Language) -> (bool, bool) {
    ui.set_width(420.0);
    ui.label(egui::RichText::new(lang.tr("Find text and redact")).font(crate::theme::semibold(18.0)));
    ui.add_space(8.0);
    ui.radio_value(&mut d.patterns, false, lang.tr("Single word or phrase"));
    let mut enter = false;
    ui.add_enabled_ui(!d.patterns, |ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut d.text).hint_text(lang.tr("Text to find")).desired_width(380.0));
        enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    });
    ui.add_space(6.0);
    ui.radio_value(&mut d.patterns, true, lang.tr("Patterns"));
    ui.add_enabled_ui(d.patterns, |ui| {
        egui::ComboBox::from_id_salt("redact-pattern").selected_text(d.pattern.label()).width(240.0).show_ui(ui, |ui| {
            for p in REDACT_PATTERNS {
                ui.selectable_value(&mut d.pattern, p, p.label());
            }
        });
    });
    ui.add_space(6.0);
    let ocr_hint = if lang == crate::i18n::Language::Fr {
        "Le texte des images n'est pas détecté ; effectuez d'abord une reconnaissance de texte (OCR)."
    } else {
        "Text in images isn't found; recognise text (OCR) first."
    };
    ui.label(egui::RichText::new(ocr_hint).small().color(t.text_faint));
    if let Some(n) = d.found {
        let match_msg = if n == 0 {
            lang.tr("No matches.").to_string()
        } else if lang == crate::i18n::Language::Fr {
            format!("{n} correspondance(s) marquée(s) pour la biffure.")
        } else {
            format!("{n} match(es) marked for redaction.")
        };
        ui.label(egui::RichText::new(match_msg).color(t.text_muted));
    }
    ui.add_space(12.0);
    let (go, cancel) = buttons(ui, lang.tr("Mark all"), true, lang);
    (go || enter, cancel)
}

/// Redaction Tool Properties. Returns (apply, cancel).
pub(crate) fn props_body(ui: &mut egui::Ui, d: &mut RedactPrefs, _t: &Tokens, lang: crate::i18n::Language) -> (bool, bool) {
    ui.set_width(380.0);
    ui.label(egui::RichText::new(lang.tr("Redaction Tool Properties")).font(crate::theme::semibold(18.0)));
    ui.add_space(8.0);
    egui::Grid::new("redact-props").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
        let fill_col_label = if lang == crate::i18n::Language::Fr { "Couleur de remplissage de la zone :" } else { "Redacted area fill colour:" };
        ui.label(fill_col_label);
        ui.horizontal(|ui| {
            let mut none = d.fill.is_none();
            let no_col_label = if lang == crate::i18n::Language::Fr { "Sans couleur" } else { "No colour" };
            if ui.checkbox(&mut none, no_col_label).changed() {
                d.fill = if none { None } else { Some([0.0, 0.0, 0.0]) };
            }
        });
        ui.end_row();
        ui.label("");
        if let Some(c) = crate::comments::swatch_grid(ui, d.fill) {
            d.fill = Some(c);
        }
        ui.end_row();
        ui.label("");
        let overlay_label = if lang == crate::i18n::Language::Fr { "Utiliser un texte de superposition" } else { "Use overlay text" };
        ui.checkbox(&mut d.use_overlay, overlay_label);
        ui.end_row();
        let l = ui.label(lang.tr("Custom text:"));
        ui.add_enabled(d.use_overlay, egui::TextEdit::singleline(&mut d.overlay).desired_width(220.0)).labelled_by(l.id);
        ui.end_row();
        let on = d.use_overlay;
        let look = &mut d.look;
        ui.label(lang.tr("Font:"));
        ui.add_enabled_ui(on, |ui| {
            egui::ComboBox::from_id_salt("overlay-font").selected_text(look.font.name()).show_ui(ui, |ui| {
                for f in printcraft_engine::OverlayFont::ALL {
                    ui.selectable_value(&mut look.font, f, f.name());
                }
            });
        });
        ui.end_row();
        ui.label(lang.tr("Font size:"));
        ui.add_enabled_ui(on, |ui| {
            ui.horizontal(|ui| {
                let mut auto = look.size <= 0.0;
                let auto_label = if lang == crate::i18n::Language::Fr {
                    "Adapter automatiquement à la zone de biffure"
                } else {
                    "Auto-size text to fit redaction region"
                };
                if ui.checkbox(&mut auto, auto_label).changed() {
                    look.size = if auto { 0.0 } else { 10.0 };
                }
                if !auto {
                    ui.add(egui::DragValue::new(&mut look.size).range(2.0..=144.0).suffix(" pt"));
                }
            });
        });
        ui.end_row();
        let font_col_label = if lang == crate::i18n::Language::Fr { "Couleur de police :" } else { "Font colour:" };
        ui.label(font_col_label);
        ui.add_enabled_ui(on, |ui| {
            if let Some(c) = crate::comments::swatch_grid(ui, Some(look.color)) {
                look.color = c;
            }
        });
        ui.end_row();
        ui.label("");
        let rep_label = if lang == crate::i18n::Language::Fr { "Répéter le texte de superposition" } else { "Repeat overlay text" };
        ui.add_enabled(on, egui::Checkbox::new(&mut look.repeat, rep_label));
        ui.end_row();
        let align_label = if lang == crate::i18n::Language::Fr { "Alignement du texte :" } else { "Text alignment:" };
        ui.label(align_label);
        ui.add_enabled_ui(on, |ui| {
            ui.horizontal(|ui| {
                for (a, label) in [(0u8, "Left"), (1, "Center"), (2, "Right")] {
                    ui.radio_value(&mut look.align, a, lang.tr(label));
                }
            });
        });
        ui.end_row();
    });
    ui.add_space(12.0);
    buttons(ui, lang.tr("OK"), true, lang)
}

/// Apply redactions confirmation. Returns (apply, cancel).
pub(crate) fn apply_body(ui: &mut egui::Ui, marks: usize, t: &Tokens, lang: crate::i18n::Language) -> (bool, bool) {
    ui.set_width(420.0);
    ui.label(egui::RichText::new(lang.tr("Apply redactions")).font(crate::theme::semibold(18.0)));
    ui.add_space(8.0);
    let warn_text = if lang == crate::i18n::Language::Fr {
        format!(
            "Vous êtes sur le point d'appliquer {marks} marque{} de biffure. Le texte, les images et les tracés sous les marques, ainsi que les commentaires et formulaires qui les chevauchent, seront définitivement supprimés.",
            if marks == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "You are about to apply {marks} redaction mark{}. Text, images and drawings under the marks, and comments and form fields that overlap them, are removed permanently.",
            if marks == 1 { "" } else { "s" }
        )
    };
    ui.label(warn_text);
    ui.add_space(4.0);
    let save_hint = if lang == crate::i18n::Language::Fr {
        "Enregistrez le document ensuite : l'enregistrement réécrit l'intégralité du fichier pour qu'aucune trace du contenu supprimé ne subsiste."
    } else {
        "Save the document afterwards: saving rewrites the whole file so no trace of the removed content stays in it."
    };
    ui.label(egui::RichText::new(save_hint).small().color(t.text_muted));
    ui.add_space(12.0);
    buttons(ui, lang.tr("Apply"), true, lang)
}

fn buttons(ui: &mut egui::Ui, ok: &str, primary: bool, lang: crate::i18n::Language) -> (bool, bool) {
    let (mut a, mut c) = (false, false);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if widgets::pill_button(ui, ok, primary).clicked() {
            a = true;
        }
        if widgets::pill_button(ui, lang.tr("Cancel"), false).clicked() {
            c = true;
        }
    });
    (a, c)
}

/// Remove Hidden Information: each category with what was found, checked by default when
/// something was.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HiddenDraft {
    pub found: Vec<(printcraft_engine::Hidden, usize, bool)>,
}

impl PrintCraftApp {
    pub fn open_remove_hidden(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        let Some(doc) = self.session.get(id) else { return };
        self.hidden_draft = HiddenDraft { found: doc.hidden_info().into_iter().map(|(h, n)| (h, n, n > 0)).collect() };
        self.dialog = Some(crate::Dialog::RemoveHidden);
    }
}

/// Returns (remove, cancel).
pub(crate) fn hidden_body(ui: &mut egui::Ui, d: &mut HiddenDraft, t: &Tokens, lang: crate::i18n::Language) -> (bool, bool) {
    ui.set_width(440.0);
    ui.label(egui::RichText::new(lang.tr("Remove hidden information")).font(crate::theme::semibold(18.0)));
    ui.add_space(4.0);
    let sel_hint = if lang == crate::i18n::Language::Fr {
        "Sélectionnez les éléments à supprimer de ce document."
    } else {
        "Select the items to remove from this document."
    };
    ui.label(egui::RichText::new(sel_hint).color(t.text_muted));
    ui.add_space(8.0);
    let total: usize = d.found.iter().map(|f| f.1).sum();
    for (h, n, on) in d.found.iter_mut() {
        ui.add_enabled_ui(*n > 0, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(on, lang.tr(h.label()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let count_str = if *n == 0 {
                        if lang == crate::i18n::Language::Fr { "Aucun trouvé".to_string() } else { "None found".to_string() }
                    } else {
                        n.to_string()
                    };
                    ui.label(egui::RichText::new(count_str).color(t.text_muted));
                });
            });
        });
    }
    ui.add_space(6.0);
    let flat_hint = if lang == crate::i18n::Language::Fr {
        "Les champs de formulaire sont aplatis : leurs valeurs restent visibles. L'enregistrement réécrit l'intégralité du fichier."
    } else {
        "Form fields are flattened: their values stay visible. Saving rewrites the whole file."
    };
    ui.label(egui::RichText::new(flat_hint).small().color(t.text_faint));
    ui.add_space(12.0);
    let any = d.found.iter().any(|f| f.2 && f.1 > 0);
    let (mut a, mut c) = (false, false);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let rem_label = if lang == crate::i18n::Language::Fr { "Supprimer" } else { "Remove" };
        if ui.add_enabled_ui(any && total > 0, |ui| widgets::pill_button(ui, rem_label, true)).inner.clicked() {
            a = true;
        }
        if widgets::pill_button(ui, lang.tr("Cancel"), false).clicked() {
            c = true;
        }
    });
    (a, c)
}

/// Sanitize Document confirmation. Returns (sanitize, cancel).
pub(crate) fn sanitize_body(ui: &mut egui::Ui, t: &Tokens, lang: crate::i18n::Language) -> (bool, bool) {
    ui.set_width(440.0);
    ui.label(egui::RichText::new(lang.tr("Sanitize document")).font(crate::theme::semibold(18.0)));
    ui.add_space(8.0);
    let sanitize_msg = if lang == crate::i18n::Language::Fr {
        "Le nettoyage supprime les informations masquées du document : métadonnées, pièces jointes, commentaires, champs de formulaire (aplatis), texte et calques masqués, signets, liens, actions et scripts, ainsi que les données d'application privées."
    } else {
        "Sanitizing removes hidden information from the document: metadata, file attachments, comments, form fields (flattened), hidden text and layers, bookmarks, links, actions and scripts, and private application data."
    };
    ui.label(sanitize_msg);
    ui.add_space(4.0);
    let save_hint = if lang == crate::i18n::Language::Fr {
        "Enregistrez le document ensuite ; l'enregistrement réécrit l'intégralité du fichier pour qu'aucun élément supprimé ne subsiste."
    } else {
        "Save the document afterwards; saving rewrites the whole file so nothing removed stays in it."
    };
    ui.label(egui::RichText::new(save_hint).small().color(t.text_muted));
    ui.add_space(12.0);
    let san_btn = if lang == crate::i18n::Language::Fr { "Nettoyer" } else { "Sanitize" };
    buttons(ui, san_btn, true, lang)
}
