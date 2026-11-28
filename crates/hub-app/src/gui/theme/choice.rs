//! A select for choices with descriptions, such as project names and their paths.
use eframe::egui;
use kylig_ui::{ListItem, ListItemOptions};
use std::borrow::Cow;

const ROW_PADDING: f32 = 10.0;
const CHECK_SIZE: f32 = 15.0;

pub(in crate::gui) struct Choice<T> {
    value: T,
    title: String,
    detail: String,
    path_start: Option<usize>,
}

impl<T> Choice<T> {
    pub(in crate::gui) fn new(
        value: T,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self { value, title: title.into(), detail: detail.into(), path_start: None }
    }

    pub(in crate::gui) fn with_path(mut self, path: &str) -> Self {
        if !self.detail.is_empty() {
            self.detail.push_str(" · ");
        }
        self.path_start = Some(self.detail.len());
        self.detail.push_str(path);
        self
    }

    fn fitted_detail(&self, ui: &egui::Ui, width: f32) -> Cow<'_, str> {
        let Some((prefix, path)) = self.path_start.and_then(|at| self.detail.split_at_checked(at))
        else {
            return Cow::Borrowed(&self.detail);
        };
        let font = egui::TextStyle::Small.resolve(ui.style());
        let fits = |text: &str| {
            ui.painter()
                .layout_no_wrap(text.to_owned(), font.clone(), ui.visuals().weak_text_color())
                .size()
                .x
                <= width
        };
        if fits(&self.detail) {
            return Cow::Borrowed(&self.detail);
        }
        // Keep the nearest parent directories when the full path does not fit.
        let mut compact = None;
        for (offset, _) in path.rmatch_indices(std::path::is_separator) {
            if offset == 0 {
                break;
            }
            let Some(suffix) = path.get(offset..) else { break };
            let candidate = format!("{prefix}…{suffix}");
            if !fits(&candidate) {
                compact.get_or_insert(candidate);
                break;
            }
            compact = Some(candidate);
        }
        compact.map_or(Cow::Borrowed(&self.detail), Cow::Owned)
    }

    fn description(&self) -> String {
        if self.detail.is_empty() {
            self.title.clone()
        } else {
            format!("{}\n{}", self.title, self.detail)
        }
    }
}

pub(in crate::gui) fn detailed_select<'a, T: PartialEq + egui::AsIdSalt>(
    ui: &mut egui::Ui,
    label: &str,
    placeholder: &str,
    current: &T,
    choices: &'a [Choice<T>],
    max_width: f32,
) -> Option<&'a T> {
    let width = ui.available_width().min(max_width);
    ui.allocate_ui_with_layout(
        egui::vec2(width, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 4.0;
            let header = ui.small(label);
            let index = choices.iter().position(|item| item.value == *current);
            let selected = index.and_then(|i| choices.get(i));
            let id = ui.make_persistent_id(egui::IdSalt::new(label));
            let was_open = egui::ComboBox::is_open(ui.ctx(), id);
            let mut picked = None;
            if !was_open
                && let Some(field) = ui.ctx().read_response(id).filter(egui::Response::has_focus)
            {
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowDown)) {
                    egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&field));
                } else {
                    picked = keyboard_target(ui, id, choices, index)
                        .and_then(|i| choices.get(i))
                        .map(|item| &item.value);
                }
            }
            let caption = selected.map_or(placeholder, |item| item.title.as_str());
            let menu = egui::ComboBox::from_id_salt(label)
                .selected_text(caption)
                .width(width)
                .truncate()
                .icon(|ui, rect, visuals, open| {
                    let glyph = if open { kylig_icons::CARET_UP } else { kylig_icons::CARET_DOWN };
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        glyph,
                        kylig_icons::font_id(ui.ctx(), 14.0, None),
                        visuals.fg_stroke.color,
                    );
                })
                .show_ui(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let focused = ui.memory(egui::Memory::focused);
                    let row_id =
                        |choice: &Choice<T>| ui.make_persistent_id(egui::Id::unique(&choice.value));
                    let at = choices.iter().position(|item| Some(row_id(item)) == focused);
                    let target = if !was_open {
                        index.or(Some(0))
                    } else if at.is_some() || focused == Some(id) {
                        keyboard_target(ui, id, choices, at.or(index))
                    } else {
                        None
                    };
                    for (i, item) in choices.iter().enumerate() {
                        let response = choice_row(ui, item, Some(i) == index);
                        if Some(i) == target {
                            response.request_focus();
                            response.scroll_to_me(None);
                        }
                        if response.clicked() {
                            picked = Some(&item.value);
                            ui.close();
                        }
                    }
                });
            let response = menu.response.labelled_by(header.id);
            if was_open
                && !egui::ComboBox::is_open(ui.ctx(), id)
                && (picked.is_some() || ui.input(|i| i.key_pressed(egui::Key::Escape)))
            {
                response.request_focus();
            }
            if let Some(item) = selected {
                response.on_hover_text(item.description());
            }
            picked
        },
    )
    .inner
}

fn choice_row<T: egui::AsIdSalt>(
    ui: &mut egui::Ui,
    item: &Choice<T>,
    active: bool,
) -> egui::Response {
    let mut row = ListItem::new(&item.title)
        .id_salt(&item.value)
        .options(ListItemOptions { height: 46.0, padding: ROW_PADDING, ..Default::default() })
        .selected(active)
        .clickable(true);
    if !item.detail.is_empty() {
        let check_space = if active { CHECK_SIZE + 12.0 } else { 0.0 };
        let subtitle =
            item.fitted_detail(ui, ui.available_width() - 2.0 * ROW_PADDING - check_space);
        row = row.subtitle(egui::RichText::new(subtitle.as_ref()).small());
    }
    row.show_with(ui, |ui| {
        if active {
            ui.add(
                kylig_icons::Icon::new(kylig_icons::CHECK)
                    .size(CHECK_SIZE)
                    .color(ui.visuals().hyperlink_color),
            );
        }
    })
    .response
    .on_hover_text(item.description())
}

// The same shortcuts pick immediately in a closed field and move focus in an open list.
fn keyboard_target<T>(
    ui: &egui::Ui,
    id: egui::Id,
    choices: &[Choice<T>],
    at: Option<usize>,
) -> Option<usize> {
    use egui::{Key, Modifiers};
    let last = choices.len().checked_sub(1)?;
    let target = ui.input_mut(|input| {
        if input.consume_key(Modifiers::NONE, Key::ArrowDown) {
            Some(at.map_or(0, |i| (i + 1).min(last)))
        } else if input.consume_key(Modifiers::NONE, Key::ArrowUp) {
            Some(at.map_or(last, |i| i.saturating_sub(1)))
        } else if input.consume_key(Modifiers::NONE, Key::Home) {
            Some(0)
        } else if input.consume_key(Modifiers::NONE, Key::End) {
            Some(last)
        } else {
            None
        }
    });
    if target.is_some() {
        ui.memory_mut(|mem| mem.move_focus(egui::FocusDirection::None));
        return target;
    }
    let (now, text) = ui.input(|i| {
        (
            i.time,
            i.events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::Text(text) if !text.trim().is_empty() => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>(),
        )
    });
    if text.is_empty() {
        return None;
    }
    let prefix = ui.data_mut(|data| {
        let (time, prefix) = data.get_temp_mut_or_default::<(f64, String)>(id.with("prefix"));
        if now - *time > 0.7 {
            prefix.clear();
        }
        *time = now;
        prefix.push_str(&text.to_lowercase());
        prefix.clone()
    });
    let first = prefix.chars().next()?;
    let repeated = prefix.chars().all(|ch| ch == first);
    let start = if repeated { at.map_or(0, |i| i + 1) } else { at.unwrap_or(0) };
    let prefix = if repeated { first.to_string() } else { prefix };
    choices
        .iter()
        .enumerate()
        .cycle()
        .skip(start)
        .take(choices.len())
        .find(|(_, item)| item.title.to_lowercase().starts_with(&prefix))
        .map(|(index, _)| index)
}
