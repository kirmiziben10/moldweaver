//! Generic parameter editor built from a mold type's [`ParamSpec`]s, so new
//! mold types get a UI without UI code.

use eframe::egui;
use moldweaver_core::molds::{ParamKind, ParamSpec, ParamValue, ParamValues};

/// Something the parameter panel asks the app to do.
pub enum ParamAction {
    PickSpout,
}

pub fn show(ui: &mut egui::Ui, specs: &[ParamSpec], values: &mut ParamValues) -> Option<ParamAction> {
    let mut action = None;
    let mut groups: Vec<&str> = Vec::new();
    for s in specs {
        if !groups.contains(&s.group) {
            groups.push(s.group);
        }
    }
    for (gi, group) in groups.iter().enumerate() {
        egui::CollapsingHeader::new(egui::RichText::new(*group).strong())
            .id_salt(("params", *group))
            .default_open(gi == 0)
            .show(ui, |ui| {
                egui::Grid::new(("grid", *group)).num_columns(2).spacing([8.0, 4.0]).striped(false).show(ui, |ui| {
                    for spec in specs.iter().filter(|s| s.group == *group) {
                        if !values.is_active(spec) {
                            continue;
                        }
                        ui.label(spec.label).on_hover_text(spec.tooltip);
                        param_widget(ui, spec, values).on_hover_text(spec.tooltip);
                        ui.end_row();
                        if spec.key == "spout_auto" && values.0.get("spout_auto") == Some(&ParamValue::Bool(false)) {
                            ui.label("");
                            if ui
                                .button("Pick on model")
                                .on_hover_text("Click a point on the model in the 3D view to put the pour spout there.")
                                .clicked()
                            {
                                action = Some(ParamAction::PickSpout);
                            }
                            ui.end_row();
                        }
                    }
                });
            });
    }
    action
}

fn param_widget(ui: &mut egui::Ui, spec: &ParamSpec, values: &mut ParamValues) -> egui::Response {
    let current = values.0.get(spec.key).cloned().unwrap_or_else(|| spec.default.clone());
    match &spec.kind {
        ParamKind::Float { min, max, step, unit } => {
            let mut v = match current {
                ParamValue::Float(f) => f,
                ParamValue::Int(i) => i as f64,
                _ => 0.0,
            };
            let decimals = if *step >= 1.0 {
                0
            } else if *step >= 0.1 {
                1
            } else {
                2
            };
            let r = ui.add(
                egui::DragValue::new(&mut v)
                    .range(*min..=*max)
                    .speed(*step)
                    .fixed_decimals(decimals)
                    .suffix(format!(" {unit}")),
            );
            if r.changed() {
                values.set(spec.key, ParamValue::Float(v));
            }
            r
        }
        ParamKind::Int { min, max } => {
            let mut v = match current {
                ParamValue::Int(i) => i,
                ParamValue::Float(f) => f.round() as i64,
                _ => 0,
            };
            let r = ui.add(egui::DragValue::new(&mut v).range(*min..=*max).speed(0.1));
            if r.changed() {
                values.set(spec.key, ParamValue::Int(v));
            }
            r
        }
        ParamKind::Bool => {
            let mut v = matches!(current, ParamValue::Bool(true));
            let r = ui.checkbox(&mut v, "");
            if r.changed() {
                values.set(spec.key, ParamValue::Bool(v));
            }
            r
        }
        ParamKind::Choice(options) => {
            let cur = match &current {
                ParamValue::Choice(s) => s.clone(),
                _ => String::new(),
            };
            let label = options.iter().find(|(id, _)| *id == cur).map(|(_, l)| *l).unwrap_or("?");
            let mut chosen = cur.clone();
            let r = egui::ComboBox::from_id_salt(("choice", spec.key))
                .selected_text(label)
                .show_ui(ui, |ui| {
                    for (id, l) in options.iter() {
                        ui.selectable_value(&mut chosen, id.to_string(), *l);
                    }
                })
                .response;
            if chosen != cur {
                values.set(spec.key, ParamValue::Choice(chosen));
            }
            r
        }
    }
}
