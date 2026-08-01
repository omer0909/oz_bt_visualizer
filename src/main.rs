use eframe::egui::{self};
use eframe::egui::{Pos2, Rect, Vec2};
use egui::Color32;
use egui::Stroke;
use egui::epaint::CubicBezierShape;
use egui::{FontData, FontFamily};
use egui_toast::Toasts;
use serde::{Deserialize, Serialize};
use std::ops::Mul;
use std::os::unix::io::RawFd;
use std::thread;
use std::time::Duration;

fn main() -> eframe::Result {
    let socket = zmq::Context::new().socket(zmq::PULL).unwrap();
    socket.set_conflate(true).expect("Zmq conflate açılamadı!");

    socket
        .connect("tcp://localhost:5555")
        .expect("Bağlanılamadı!");

    let raw_fd: RawFd = socket.get_fd().expect("Zmq fd alınamadı.");

    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([800.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "My egui App",
        options,
        Box::new(|cc| {
            let egui_ctx = cc.egui_ctx.clone();
            thread::spawn(move || {
                loop {
                    let mut items = [zmq::PollItem::from_fd(raw_fd, zmq::PollEvents::POLLIN)];
                    zmq::poll(&mut items, -1)
                        .expect("Verinin gelip gelmediğine bakılırken hata oluştu!");
                    egui_ctx.request_repaint();
                    thread::sleep(Duration::from_millis(10));
                }
            });
            egui_extras::install_image_loaders(&cc.egui_ctx);

            let mut fonts = egui::FontDefinitions::default();

            fonts.font_data.insert(
                "custom_font".to_owned(),
                FontData::from_static(include_bytes!("../assets/NotoSans-Bold.ttf")).into(),
            );

            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .insert(0, "custom_font".to_owned());

            egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
            cc.egui_ctx.set_fonts(fonts);

            cc.egui_ctx.set_visuals(egui::Visuals::light());
            cc.egui_ctx.set_zoom_factor(1.5);

            Ok(Box::<MyApp>::new(MyApp::new(socket)))
        }),
    )
}

struct MyApp {
    socket: zmq::Socket,
    watching_group: Vec<usize>,
    tree: WatchContent,
    reset_view: bool,
    start_time: chrono::DateTime<chrono::Utc>,
    last_message_time: chrono::DateTime<chrono::Utc>,
    toasts: Toasts,
    transform: egui::emath::TSTransform,
    last_scene_center: egui::Vec2,
    scene_font_scale: f32,
}

impl MyApp {
    fn new(socket: zmq::Socket) -> Self {
        Self {
            socket: socket,
            watching_group: Vec::new(),
            tree: WatchContent {
                node_type: NodeTypes::Flow,
                name: "Empty".to_string(),
                watch_state: WatchState::None,
                childs: Vec::new(),
                comment: Some("This app visualizes behavior trees.".to_string()),
            },
            reset_view: true,
            start_time: Default::default(),
            last_message_time: Default::default(),
            toasts: Toasts::default(),
            transform: Default::default(),
            last_scene_center: Vec2::ZERO,
            scene_font_scale: 5.0,
        }
    }
}

impl WatchContent {
    fn without_childs(&self) -> Self {
        Self {
            node_type: self.node_type.clone(),
            name: self.name.clone(),
            watch_state: self.watch_state,
            childs: Vec::new(),
            comment: self.comment.clone(),
        }
    }
}

impl GroupContent {
    fn without_childs(&self) -> Self {
        Self {
            childs: Vec::new(),
            extra_data: self.extra_data.clone(),
            watch_content: self.watch_content.clone(),
            width: self.width,
            all_width: self.all_width,
            address: self.address.clone(),
        }
    }

    fn get_group(nodes: &Vec<WatchContent>, deep: i32, parent_address: Vec<usize>) -> Vec<Self> {
        nodes
            .iter()
            .enumerate()
            .flat_map(|(index, node)| {
                let address: Vec<usize> = parent_address
                    .iter()
                    .copied()
                    .chain(std::iter::once(index))
                    .collect();

                let mut active_deep = deep;

                let include_self_before = active_deep == 0;

                if matches!(node.node_type, NodeTypes::GroupOut(_)) {
                    active_deep += 1;
                }
                if matches!(node.node_type, NodeTypes::GroupIn(_)) {
                    active_deep -= 1;
                }

                let include_self = active_deep == 0 || include_self_before;

                let watch_content = {
                    let mut watch = node.without_childs();
                    watch.name = watch.name.to_uppercase();
                    watch
                };

                if include_self {
                    vec![Self {
                        childs: Self::get_group(&node.childs, active_deep, address.clone()),
                        extra_data: ExtraData::Empty,
                        watch_content: watch_content,
                        width: 0.0,
                        all_width: 0.0,
                        address: address,
                    }]
                } else {
                    Self::get_group(&node.childs, active_deep, address)
                }
            })
            .collect()
    }

    fn calculate_group_outputs(node: Self) -> Self {
        let base = node.without_childs();
        if !matches!(node.watch_content.node_type, NodeTypes::GroupIn(_))
            || matches!(node.extra_data, ExtraData::GroupBegin)
        {
            return GroupContent {
                childs: node
                    .childs
                    .into_iter()
                    .map(|child| Self::calculate_group_outputs(child))
                    .collect(),
                ..base
            };
        }

        let out_names = node
            .childs
            .iter()
            .map(|child| {
                if let NodeTypes::GroupOut(out_name) = &child.watch_content.node_type {
                    Some(out_name.clone())
                } else {
                    None
                }
            })
            .collect::<Option<Vec<String>>>()
            .expect("Tree gruplarının child larına bakarken bir hata oluştu!");

        GroupContent {
            childs: node
                .childs
                .into_iter()
                .flat_map(|child| child.childs)
                .map(|child| Self::calculate_group_outputs(child))
                .collect(),
            extra_data: ExtraData::GroupNode(out_names),
            ..base
        }
    }

    fn get_height(&self) -> f32 {
        let childs_height: f32 = self
            .childs
            .iter()
            .map(|child| child.get_height() + 20.0)
            .max_by(f32::total_cmp)
            .unwrap_or(0.0);

        childs_height + 60.0
    }
}

impl MyApp {
    fn draw(&mut self, ui: &mut egui::Ui, node: &GroupContent, pos: Pos2, edge_out: Option<Pos2>) {
        let space: f32 = 10.0;
        let node_height: f32 = 60.0;
        let corner_radius = 10.0;

        let color = match node.watch_content.watch_state {
            WatchState::Cancelled => Color32::MAGENTA,
            WatchState::Failed => Color32::LIGHT_RED,
            WatchState::None => Color32::LIGHT_GRAY,
            WatchState::Succeeded => Color32::LIGHT_GREEN,
            WatchState::Running => Color32::LIGHT_BLUE,
        };

        let node_pos = pos + Vec2::new((node.all_width - node.width) / 2.0, 0.0);
        let up_pos = self
            .transform
            .mul_pos(node_pos + Vec2::new((node.width - space) / 2.0, 0.0));

        let rect = self.transform.mul_rect(Rect::from_min_size(
            node_pos,
            Vec2::new(node.width - space, node_height),
        ));

        if ui.clip_rect().intersects(rect) {
            match &node.watch_content.node_type {
                NodeTypes::GroupIn(group_name) => {
                    if let ExtraData::GroupNode(_) = node.extra_data {
                        if self.group_in_draw(ui, node, rect, color).clicked() {
                            self.watching_group = node.address.clone();
                            self.reset_view = true;
                        }
                    } else {
                        self.node_desc_draw(ui, node, rect, color, &group_name);
                    }
                }

                NodeTypes::Event(event_name) => {
                    self.node_desc_draw(ui, node, rect, color, &event_name);
                }
                NodeTypes::GroupOut(out_name) => {
                    self.node_desc_draw(ui, node, rect, color, &out_name);
                }
                _ => {
                    self.basic_node_draw(ui, node, rect, color);
                }
            };

            if let Some(comment) = &node.watch_content.comment {
                ui.allocate_rect(rect, egui::Sense::hover())
                    .on_hover_text(comment);

                let icon_pos = self.transform.mul_pos(
                    node_pos + egui::vec2(node.width - space - corner_radius, corner_radius),
                );

                ui.painter().circle_filled(
                    icon_pos,
                    6.0 * self.transform.scaling,
                    egui::Color32::from_white_alpha(200),
                );

                self.text(
                    ui,
                    egui_phosphor::regular::INFO,
                    icon_pos,
                    10.0,
                    egui::Color32::DARK_BLUE,
                );
            }
        }

        let edge_in: Vec<f32> = if node.childs.len() < 2 {
            vec![0.0; node.childs.len()]
        } else {
            if let ExtraData::GroupNode(data) = &node.extra_data {
                self.calculate_group_out_edges(ui, data, " | ")
            } else {
                let target_step: f32 = 5.0 * self.transform.scaling;
                let area = ((node.childs.len() as f32 - 1.0) * target_step)
                    .min((node.width - 20.0) * self.transform.scaling);
                let start = -area / 2.0;
                let step = area / (node.childs.len() as f32 - 1.0);
                (0..node.childs.len())
                    .map(|i| start + step * i as f32)
                    .collect()
            }
        };

        let child_add_pos =
            (node.all_width - node.childs.iter().map(|child| child.all_width).sum::<f32>()) / 2.0;
        let mut child_pos = pos - Vec2::new(-child_add_pos, -(node_height + 20.0));
        for (child, edge_in) in node.childs.iter().zip(edge_in) {
            self.draw(
                ui,
                child,
                child_pos,
                Some(up_pos + Vec2::new(edge_in, node_height * self.transform.scaling)),
            );
            child_pos += Vec2::new(child.all_width, 0.0);
        }

        if let Some(out) = edge_out {
            let radius = (2.0 * self.transform.scaling).max(1.0);
            if ui.clip_rect().intersects(Rect::from_two_pos(
                out - Vec2::new(0.0, radius),
                up_pos + Vec2::new(0.0, radius),
            )) {
                let painter = ui.painter();
                let point_color = color.mul(Color32::from_rgb(200, 200, 200));

                let stroke = Stroke::new((1.0 * self.transform.scaling).max(1.0), point_color);
                let shape = CubicBezierShape::from_points_stroke(
                    [
                        up_pos,
                        up_pos - Vec2::new(0.0, 10.0) * self.transform.scaling,
                        out + Vec2::new(0.0, 10.0) * self.transform.scaling,
                        out,
                    ],
                    false,
                    Color32::TRANSPARENT,
                    stroke,
                );

                painter.add(shape);

                painter.circle_filled(up_pos, radius, point_color);
                painter.circle_filled(out, radius, point_color);
            }
        }
    }

    fn get_text_size(&self, ui: &egui::Ui, text: &str) -> Vec2 {
        let galley = ui.fonts_mut(|f| {
            f.layout_no_wrap(
                text.to_string(),
                egui::FontId::proportional(14.0 * self.scene_font_scale),
                egui::Color32::WHITE,
            )
        });

        galley.size() / self.scene_font_scale
    }

    fn calculate_width(&self, ui: &egui::Ui, node: GroupContent) -> GroupContent {
        let base = node.without_childs();
        let childs: Vec<GroupContent> = node
            .childs
            .into_iter()
            .map(|child| self.calculate_width(ui, child))
            .collect();

        let childs_width: f32 = childs.iter().map(|child| child.all_width).sum();

        let width = match node.watch_content.node_type {
            NodeTypes::Event(event_name) => self.get_text_size(ui, &event_name).x,
            NodeTypes::GroupOut(out_name) => self.get_text_size(ui, &out_name).x,
            NodeTypes::GroupIn(group_name) => {
                let out_names_size = match node.extra_data {
                    ExtraData::GroupNode(out_names) => {
                        self.get_text_size(ui, &out_names.join(" | ")).x
                    }
                    _ => 0.0f32,
                };
                out_names_size.max(self.get_text_size(ui, &group_name).x)
            }
            _ => 0.0f32,
        }
        .max(self.get_text_size(ui, &node.watch_content.name).x)
            + 40.0;

        GroupContent {
            width: width,
            all_width: childs_width.max(width),
            childs: childs,
            ..base
        }
    }

    fn group_in_draw(
        &self,
        ui: &mut egui::Ui,
        node: &GroupContent,
        rect: egui::Rect,
        color: Color32,
    ) -> egui::Response {
        let response = ui.allocate_rect(rect, egui::Sense::click());

        if ui.is_rect_visible(rect) {
            let visuals = ui.style().interact(&response);

            ui.painter().rect(
                rect,
                10.0 * self.transform.scaling,
                color,
                egui::Stroke::new(1.0 * self.transform.scaling, egui::Color32::DARK_BLUE),
                egui::StrokeKind::Outside,
            );

            let tex_color = visuals.text_color();

            self.text(
                ui,
                &node.watch_content.name,
                rect.center() + Vec2::new(0.0, -20.0) * self.transform.scaling,
                14.0,
                tex_color,
            );

            let NodeTypes::GroupIn(group_name) = &node.watch_content.node_type else {
                panic!("Gurup node içinde gurup ismi bulunamadı!");
            };

            self.text(ui, &group_name, rect.center(), 14.0, tex_color);

            let ExtraData::GroupNode(out_names) = &node.extra_data else {
                panic!("Gurup node içinde out_names bulunamadı!");
            };

            self.text(
                ui,
                &out_names.join(" | "),
                rect.center() + Vec2::new(0.0, 20.0) * self.transform.scaling,
                14.0,
                tex_color,
            );
        }

        response
    }

    fn node_desc_draw(
        &self,
        ui: &mut egui::Ui,
        node: &GroupContent,
        rect: egui::Rect,
        color: Color32,
        desc: &str,
    ) -> egui::Response {
        let response = ui.allocate_rect(rect, egui::Sense::click());

        if ui.is_rect_visible(rect) {
            let visuals = ui.style().interact(&response);

            ui.painter().rect(
                rect,
                10.0 * self.transform.scaling,
                color,
                egui::Stroke::new(1.0 * self.transform.scaling, egui::Color32::GRAY),
                egui::StrokeKind::Inside,
            );

            let tex_color = visuals.text_color();

            self.text(
                ui,
                &node.watch_content.name,
                rect.center() + Vec2::new(0.0, -10.0) * self.transform.scaling,
                14.0,
                tex_color,
            );

            self.text(
                ui,
                desc,
                rect.center() + Vec2::new(0.0, 10.0) * self.transform.scaling,
                14.0,
                tex_color,
            );
        }

        response
    }

    fn text(&self, ui: &mut egui::Ui, text: &str, pos: Pos2, font_size: f32, color: Color32) {
        let galley = ui.painter().layout_no_wrap(
            text.to_string(),
            egui::FontId::proportional(font_size * self.scene_font_scale),
            color,
        );

        let text_pos = pos - galley.size() * 0.5;
        let text_shape = egui::epaint::TextShape::new(text_pos, galley, color);
        let mut shape: egui::Shape = text_shape.into();

        let scale_factor = self.transform.scaling / self.scene_font_scale;
        let transform = egui::emath::TSTransform {
            scaling: scale_factor,
            translation: pos.to_vec2() * (1.0 - scale_factor),
        };
        shape.transform(transform);

        ui.painter().add(shape);
    }

    fn basic_node_draw(
        &self,
        ui: &mut egui::Ui,
        node: &GroupContent,
        rect: egui::Rect,
        color: Color32,
    ) -> egui::Response {
        let response = ui.allocate_rect(rect, egui::Sense::click());

        if ui.is_rect_visible(rect) {
            let visuals = ui.style().interact(&response);

            ui.painter().rect(
                rect,
                10.0 * self.transform.scaling,
                color,
                egui::Stroke::new(1.0 * self.transform.scaling, egui::Color32::GRAY),
                egui::StrokeKind::Inside,
            );

            self.text(
                ui,
                &node.watch_content.name,
                rect.center(),
                14.0,
                visuals.text_color(),
            );
        }

        response
    }

    fn calculate_group_out_edges(
        &self,
        ui: &mut egui::Ui,
        out_names: &[String],
        seperator: &str,
    ) -> Vec<f32> {
        let mut result = Vec::<f32>::new();

        let scale_factor = self.transform.scaling / self.scene_font_scale;
        let galley = ui.fonts_mut(|f| {
            f.layout_no_wrap(
                out_names.join(seperator),
                egui::FontId::proportional(14.0 * self.scene_font_scale),
                egui::Color32::WHITE,
            )
        });

        let mut glyphs = galley
            .rows
            .first()
            .map(|row| row.glyphs.clone())
            .unwrap_or(Vec::new())
            .into_iter()
            .rev();
        let add_pos = -galley.size().x / 2.0;
        let mut default_pos = galley.size().x;
        for word in out_names.iter().rev() {
            let start = default_pos;
            let mut end = default_pos;

            for glyph in glyphs.by_ref().take(word.chars().count()) {
                end = glyph.pos.x;
                default_pos = end;
            }

            result.push(((start + end) / 2.0 + add_pos) * scale_factor);

            for glyph in glyphs.by_ref().take(seperator.chars().count()) {
                default_pos = glyph.pos.x;
            }
        }
        result.reverse();
        result
    }

    fn take_node(&self, address: &Vec<usize>) -> Option<&WatchContent> {
        address
            .iter()
            .try_fold(&self.tree, |node, &index| node.childs.get(index))
    }

    fn take_path(&self, mut address: Vec<usize>) -> Option<String> {
        let mut result = String::new();

        loop {
            let Some(node) = self.take_node(&address) else {
                return None;
            };

            if let NodeTypes::GroupIn(group_name) = &node.node_type {
                result = format!("/{}{}", group_name, result);
            }

            if address.pop().is_none() {
                break;
            }
        }
        Some("Begin".to_string() + &result)
    }
}

impl eframe::App for MyApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Ok(bytes) = self.socket.recv_bytes(zmq::DONTWAIT) {
            let (message, _): (VisualizerMessage, usize) = bincode_next::serde::decode_from_slice(
                bytes.as_slice(),
                bincode_next::config::standard(),
            )
            .expect("Message parse failed!");

            self.tree = WatchContent {
                node_type: NodeTypes::Flow,
                name: "Begin".to_string(),
                watch_state: WatchState::Running,
                childs: vec![message.watch_content],
                comment: None,
            };

            self.last_message_time = message.send_time;

            if self.start_time != message.start_time {
                if self.start_time == chrono::DateTime::<chrono::Utc>::default() {
                    self.reset_view = true;
                }

                self.start_time = message.start_time;
                self.toasts.add(egui_toast::Toast {
                    kind: egui_toast::ToastKind::Info,
                    text: "New Connection!".into(),
                    options: egui_toast::ToastOptions::default()
                        .duration(Some(Duration::from_secs(5))),
                    style: Default::default(),
                });
            }
        }

        self.toasts.show(ui);
        ui.request_repaint_after(std::time::Duration::from_secs_f32(1.0));

        let group = {
            while 0 < self.watching_group.len() {
                let Some(node) = self.take_node(&self.watching_group) else {
                    self.watching_group.pop();
                    continue;
                };
                if matches!(node.node_type, NodeTypes::GroupIn(_)) {
                    break;
                }
                self.watching_group.pop();
                self.reset_view = true;
            }

            let Some(group_parent) = self.take_node(&self.watching_group) else {
                panic!("İzlenilen gurup bulunamadı!");
            };
            let watch_content = {
                let mut watch = group_parent.without_childs();
                watch.name = watch.name.to_uppercase();
                watch
            };

            let group_with_out = GroupContent {
                childs: GroupContent::get_group(
                    &group_parent.childs,
                    0,
                    self.watching_group.clone(),
                ),
                extra_data: ExtraData::GroupBegin,
                watch_content: watch_content,
                width: 0.0,
                all_width: 0.0,
                address: Vec::new(),
            };

            let group_no_width = GroupContent::calculate_group_outputs(group_with_out);

            self.calculate_width(ui, group_no_width)
        };

        let screen = egui::Frame::default().fill(egui::Color32::from_rgb(253, 246, 227));

        egui::CentralPanel::default().frame(screen).show(ui, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                ui.label(
                    self.take_path(self.watching_group.clone())
                        .unwrap_or("İzlenilen gurup bulunamadı!".to_string()),
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(2.0);
                    self.reset_view = ui
                        .button(egui_phosphor::regular::FRAME_CORNERS)
                        .on_hover_text("Reset Viev")
                        .clicked()
                        || self.reset_view;
                    if ui
                        .add_enabled(
                            self.watching_group.len() != 0,
                            egui::Button::new(egui_phosphor::regular::CARET_DOUBLE_UP),
                        )
                        .on_hover_text("Back Group")
                        .clicked()
                    {
                        self.watching_group.pop();
                        self.reset_view = true;
                    }
                });
            });

            if chrono::Utc::now() - self.last_message_time > chrono::TimeDelta::seconds(1) {
                let info = if self.start_time == chrono::DateTime::<chrono::Utc>::default() {
                    "Bağlantı yok!".to_string()
                } else {
                    ui.request_repaint_after(std::time::Duration::from_secs_f32(0.1));

                    if let Ok(std_duration) = (chrono::Utc::now() - self.last_message_time).to_std()
                    {
                        format!("Mesaj bekleniyor: {:.2}s", std_duration.as_secs_f32())
                    } else {
                        "Mesaj bekleniyor!".to_string()
                    }
                };

                egui::Area::new(egui::Id::new("status_box"))
                    .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(0.0, 0.0))
                    .show(ui.ctx(), |ui| {
                        egui::Frame::default()
                            .fill(egui::Color32::DARK_RED)
                            .inner_margin(2.0)
                            .show(ui, |ui| {
                                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                                ui.colored_label(egui::Color32::WHITE, info);
                            });
                    });
            }

            let zoom_min = 0.2;
            let zoom_max = 5.0;

            ui.add(egui::Separator::default().spacing(0.0));
            let (rect, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::drag());
            ui.set_clip_rect(rect);
            if response.dragged_by(egui::PointerButton::Middle)
                || response.dragged_by(egui::PointerButton::Primary)
            {
                let delta = response.drag_delta();
                self.transform.translation += delta;
            }
            let mut clamped = 0.0;
            if response.hovered() {
                let scroll_delta = ui.input(|i| i.smooth_scroll_delta.y);

                if scroll_delta != 0.0 {
                    let pointer_pos = response.hover_pos().unwrap_or(rect.center());
                    let zoom_factor = (scroll_delta * 0.01).exp();
                    let old_scaling = self.transform.scaling * zoom_factor;
                    let new_scaling =
                        (self.transform.scaling * zoom_factor).clamp(zoom_min, zoom_max);
                    clamped = (old_scaling - new_scaling).abs();
                    let pointer_in_scene = self.transform.inverse() * pointer_pos;
                    self.transform.scaling = new_scaling;
                    self.transform.translation =
                        pointer_pos - (pointer_in_scene * self.transform.scaling);
                }
            }

            if ui.input(|i| i.smooth_scroll_delta.y) == 0.0 {
                self.scene_font_scale = self.transform.scaling;
            }

            {
                let clamped_anim_id = ui.make_persistent_id("smooth_clamped");
                let current_clamped = ui.ctx().animate_value_with_time(
                    clamped_anim_id,
                    clamped * 100.0 / self.transform.scaling,
                    0.3,
                );

                if current_clamped > 0.0 {
                    ui.painter().rect_stroke(
                        rect,
                        0.0,
                        Stroke::new(current_clamped, Color32::LIGHT_GRAY),
                        egui::StrokeKind::Inside,
                    );
                }
            }

            {
                let center = rect.center().to_vec2();
                let center_diff = center - self.last_scene_center;
                self.transform.translation += center_diff;
                self.last_scene_center = center;
            }

            if self.reset_view && rect.size().x < 5000.0 {
                let group_height = group.get_height();
                let group_width = group.all_width - 10.0;
                let size = {
                    let x = rect.size().x / group_width;
                    let y = rect.size().y / group_height;
                    let target_size = x.min(y);
                    (target_size * 0.98).clamp(zoom_min, zoom_max)
                };
                let group_center_x = rect.size().x / 2.0 - group_width * size / 2.0;
                let group_center_y = rect.size().y / 2.0 - group_height * size / 2.0;

                self.transform = egui::emath::TSTransform::new(
                    rect.min.to_vec2() + Vec2::new(group_center_x, group_center_y),
                    size,
                );
                self.reset_view = false;
            }

            self.draw(ui, &group, Pos2::ZERO, None);
        });
    }
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize, Clone, Copy)]
pub enum WatchState {
    Running,
    Succeeded,
    Failed,
    Cancelled,
    None,
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize, Clone)]
pub enum NodeTypes {
    Flow,
    Leaf,
    Decorator,
    Event(String),
    GroupIn(String),
    GroupOut(String),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WatchContent {
    pub node_type: NodeTypes,
    pub name: String,
    pub watch_state: WatchState,
    pub childs: Vec<WatchContent>,
    pub comment: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VisualizerMessage {
    pub start_time: chrono::DateTime<chrono::Utc>,
    pub send_time: chrono::DateTime<chrono::Utc>,
    pub watch_content: WatchContent,
}

#[derive(Debug)]
struct GroupContent {
    watch_content: WatchContent,
    extra_data: ExtraData,
    all_width: f32,
    width: f32,
    childs: Vec<GroupContent>,
    address: Vec<usize>,
}

#[derive(Debug, Clone)]
enum ExtraData {
    Empty,
    GroupBegin,
    GroupNode(Vec<String>),
}
