#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod client;
mod config;
mod icon;
mod input;
mod protocol;
mod server;

use std::sync::Arc;
use eframe::egui;
use egui::{Color32, RichText, Vec2};
use tokio::sync::mpsc;
use tokio::runtime::Runtime;

use client::{Cmd, Evt};
use config::Config;
use protocol::{InputEvent, MouseBtn};

#[derive(PartialEq, Clone)]
enum Screen { FirstRun, Main, Remote, Files, Settings }

struct App {
    config:        Config,
    screen:        Screen,
    last_screen:   Screen,
    rt:            Arc<Runtime>,
    // first run
    fr_pass1:      String,
    fr_pass2:      String,
    fr_error:      String,
    // connect
    pass_buf:      String,
    conn_error:    String,
    connecting:    bool,
    // sessão principal
    cmd_tx:        Option<mpsc::Sender<Cmd>>,
    evt_rx:        Option<mpsc::Receiver<Evt>>,
    server_w:      u32,
    server_h:      u32,
    peer_platform: String,
    // sessão secundária (monitor 2)
    cmd_tx2:       Option<mpsc::Sender<Cmd>>,
    evt_rx2:       Option<mpsc::Receiver<Evt>>,
    server_w2:     u32,
    server_h2:     u32,
    // frame principal
    frame_tex:     Option<egui::TextureHandle>,
    canvas:        Option<Vec<u8>>,
    canvas_w:      u32,
    canvas_h:      u32,
    // frame segunda janela
    frame_tex2:    Option<egui::TextureHandle>,
    canvas2:       Option<Vec<u8>>,
    canvas2_w:     u32,
    canvas2_h:     u32,
    fps_count:     u32,
    fps_last:      std::time::Instant,
    fps_display:   f32,
    // files
    file_items:    Vec<protocol::FileItem>,
    file_folder:   String,
    file_selected: Option<usize>,
    file_status:   String,
    // settings buffers
    cfg_pass:      String,
    cfg_port:      String,
    cfg_fps:       String,
    cfg_quality:   String,
    cfg_folder:    String,
    cfg_relay_host: String,
    cfg_relay_port: String,
    cfg_saved:     bool,
    // monitores remotos
    monitors:       Vec<protocol::MonitorInfo>,
    active_monitor: u8,
    show_monitor2:  bool,
    // info
    local_ip:      String,
    drag_status:   String,
    is_secondary:  bool,  // true quando aberto via --connect (processo filho)
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut vis = egui::Visuals::dark();
        vis.panel_fill       = Color32::from_rgb(13, 13, 15);
        vis.window_fill      = Color32::from_rgb(19, 19, 26);
        vis.extreme_bg_color = Color32::from_rgb(10, 10, 12);
        cc.egui_ctx.set_visuals(vis);

        let config   = Config::load();
        let screen   = if config.first_run { Screen::FirstRun } else { Screen::Main };
        let cfg_pass    = config.password.clone();
        let cfg_port    = config.port.to_string();
        let cfg_fps     = config.fps.to_string();
        let cfg_quality = config.jpeg_quality.to_string();
        let cfg_folder  = config.shared_folder.clone();
        let cfg_relay_host = config.relay_host.clone();
        let cfg_relay_port = config.relay_port.to_string();
        let local_ip    = get_local_ip();
        let rt          = Arc::new(Runtime::new().expect("tokio runtime"));

        {
            let cfg_arc = Arc::new(config.clone());
            rt.spawn(server::run(cfg_arc));
        }

        Self {
            config, screen: screen.clone(), last_screen: screen, rt,
            fr_pass1: String::new(), fr_pass2: String::new(), fr_error: String::new(),
            pass_buf: String::new(),
            conn_error: String::new(), connecting: false,
            cmd_tx: None, evt_rx: None,
            server_w: 1920, server_h: 1080, peer_platform: String::new(),
            cmd_tx2: None, evt_rx2: None, server_w2: 1920, server_h2: 1080,
            frame_tex: None, canvas: None, canvas_w: 0, canvas_h: 0,
            frame_tex2: None, canvas2: None, canvas2_w: 0, canvas2_h: 0,
            fps_count: 0, fps_last: std::time::Instant::now(), fps_display: 0.0,
            file_items: Vec::new(), file_folder: String::new(),
            file_selected: None, file_status: String::new(),
            cfg_pass, cfg_port, cfg_fps, cfg_quality, cfg_folder,
            cfg_relay_host, cfg_relay_port, cfg_saved: false,
            monitors: Vec::new(), active_monitor: 0, show_monitor2: false,
            local_ip, drag_status: String::new(), is_secondary: false,
        }
    }

    fn poll_events(&mut self, ctx: &egui::Context) {
        // Sessão principal
        let events: Vec<Evt> = {
            let Some(rx) = self.evt_rx.as_mut() else { return };
            let mut evs = Vec::new();
            while let Ok(e) = rx.try_recv() { evs.push(e); }
            evs
        };
        for evt in events {
            match evt {
                Evt::MonitorList { monitors } => { self.monitors = monitors; }
                Evt::Connected { screen_w, screen_h, platform } => {
                    self.server_w = screen_w; self.server_h = screen_h;
                    self.peer_platform = platform;
                    self.screen = Screen::Remote;
                    self.conn_error = String::new(); self.connecting = false;
                }
                Evt::Frame { jpeg } => {
                    self.update_canvas_from_jpeg(&jpeg, ctx, false);
                }
                Evt::FrameDelta { screen_w, screen_h, blocks, .. } => {
                    self.apply_delta(screen_w, screen_h, &blocks, ctx, false);
                }
                Evt::FileList { folder, items } => {
                    self.file_folder = folder; self.file_items = items; self.file_status = String::new();
                }
                Evt::FileDone { filename, bytes } => {
                    self.file_status = format!("OK: {} ({} bytes)", filename, bytes);
                    self.drag_status = format!("Enviado: {}", filename);
                    self.send_cmd(Cmd::FileList);
                }
                Evt::FileError { reason } => { self.file_status = format!("Erro: {}", reason); }
                Evt::Clipboard { text } => { ctx.output_mut(|o| o.copied_text = text); }
                Evt::Error { reason } => {
                    self.conn_error = reason; self.connecting = false;
                    self.screen = Screen::Main; self.cmd_tx = None; self.evt_rx = None;
                }
                Evt::Disconnected => {
                    self.screen = Screen::Main; self.cmd_tx = None; self.evt_rx = None;
                    self.frame_tex = None; self.monitors = Vec::new(); self.active_monitor = 0;
                    self.show_monitor2 = false;
                    self.cmd_tx2 = None; self.evt_rx2 = None; self.frame_tex2 = None;
                    self.canvas2 = None; self.canvas2_w = 0; self.canvas2_h = 0;
                }
            }
        }

        // Sessão secundária
        let events2: Vec<Evt> = {
            let Some(rx) = self.evt_rx2.as_mut() else { return };
            let mut evs = Vec::new();
            while let Ok(e) = rx.try_recv() { evs.push(e); }
            evs
        };
        for evt in events2 {
            match evt {
                Evt::Connected { screen_w, screen_h, .. } => {
                    self.server_w2 = screen_w; self.server_h2 = screen_h;
                }
                Evt::Frame { jpeg } => {
                    self.update_canvas_from_jpeg(&jpeg, ctx, true);
                }
                Evt::FrameDelta { screen_w, screen_h, blocks, .. } => {
                    self.apply_delta(screen_w, screen_h, &blocks, ctx, true);
                }
                Evt::Disconnected | Evt::Error { .. } => {
                    self.cmd_tx2 = None; self.evt_rx2 = None;
                    self.frame_tex2 = None; self.show_monitor2 = false;
                    self.canvas2 = None; self.canvas2_w = 0; self.canvas2_h = 0;
                }
                _ => {}
            }
        }
    }

    fn update_canvas_from_jpeg(&mut self, jpeg: &[u8], ctx: &egui::Context, secondary: bool) {
        if let Ok(img) = image::load_from_memory(jpeg) {
            let rgba = img.to_rgba8();
            let (w, h) = (rgba.width(), rgba.height());
            let data = rgba.into_raw();
            if secondary {
                self.canvas2 = Some(data.clone());
                self.canvas2_w = w; self.canvas2_h = h;
                let ci = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &data);
                self.frame_tex2 = Some(ctx.load_texture("frame2", ci, egui::TextureOptions::LINEAR));
            } else {
                self.canvas = Some(data.clone());
                self.canvas_w = w; self.canvas_h = h;
                let ci = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &data);
                self.frame_tex = Some(ctx.load_texture("frame", ci, egui::TextureOptions::LINEAR));
            }
        }
        self.tick_fps(ctx);
    }

    fn apply_delta(&mut self, screen_w: u32, screen_h: u32,
        blocks: &[(protocol::BlockInfo, Vec<u8>)], ctx: &egui::Context, secondary: bool)
    {
        let (canvas, cw, ch, tex_name) = if secondary {
            (&mut self.canvas2, &mut self.canvas2_w, &mut self.canvas2_h, "frame2")
        } else {
            (&mut self.canvas, &mut self.canvas_w, &mut self.canvas_h, "frame")
        };

        if canvas.is_none() || *cw != screen_w || *ch != screen_h {
            *canvas = Some(vec![0u8; (screen_w * screen_h * 4) as usize]);
            *cw = screen_w; *ch = screen_h;
        }
        let buf = canvas.as_mut().unwrap();
        for (block, jpeg) in blocks {
            if let Ok(img) = image::load_from_memory(jpeg) {
                let rgba = img.to_rgba8();
                for y in 0..block.h { for x in 0..block.w {
                    let px = rgba.get_pixel(x, y);
                    let idx = (((block.y + y) * screen_w + (block.x + x)) * 4) as usize;
                    if idx + 3 < buf.len() {
                        buf[idx] = px[0]; buf[idx+1] = px[1];
                        buf[idx+2] = px[2]; buf[idx+3] = px[3];
                    }
                }}
            }
        }
        let ci = egui::ColorImage::from_rgba_unmultiplied([screen_w as usize, screen_h as usize], buf);
        let tex = ctx.load_texture(tex_name, ci, egui::TextureOptions::LINEAR);
        if secondary { self.frame_tex2 = Some(tex); } else { self.frame_tex = Some(tex); }
        self.tick_fps(ctx);
    }

    fn tick_fps(&mut self, ctx: &egui::Context) {
        self.fps_count += 1;
        let elapsed = self.fps_last.elapsed().as_secs_f32();
        if elapsed >= 1.0 {
            self.fps_display = self.fps_count as f32 / elapsed;
            self.fps_count = 0; self.fps_last = std::time::Instant::now();
        }
        ctx.request_repaint();
    }

    fn send_cmd(&self, cmd: Cmd) {
        if let Some(tx) = &self.cmd_tx { let _ = tx.try_send(cmd); }
    }

    fn send_cmd2(&self, cmd: Cmd) {
        if let Some(tx) = &self.cmd_tx2 { let _ = tx.try_send(cmd); }
    }

    fn do_connect(&mut self) {
        self.do_connect_monitor(None);
    }

    fn do_connect_monitor(&mut self, monitor_index: Option<u8>) {
        let remote_id = match monitor_index {
            None    => self.config.remote_id.trim().to_string(),
            Some(i) => format!("{}-{}", self.config.remote_id.trim(), i),
        };
        let pass = if self.pass_buf.is_empty() { self.config.password.clone() } else { self.pass_buf.clone() };
        let relay = if self.config.use_relay && !self.config.relay_host.trim().is_empty() {
            Some((self.config.relay_host.trim().to_string(), self.config.relay_port))
        } else {
            None
        };

        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>(64);
        let (evt_tx, evt_rx) = mpsc::channel::<Evt>(256);

        if monitor_index.is_none() {
            // Conexão principal
            self.cmd_tx = Some(cmd_tx); self.evt_rx = Some(evt_rx);
            self.connecting = true; self.conn_error = String::new();
            self.config.save();
        } else {
            // Conexão secundária
            self.cmd_tx2 = Some(cmd_tx); self.evt_rx2 = Some(evt_rx);
        }

        let host = String::new();
        let port = self.config.port;
        self.rt.spawn(client::connect(host, port, pass, relay, remote_id, monitor_index, cmd_rx, evt_tx));
    }

    fn disconnect(&mut self) {
        self.send_cmd(Cmd::Disconnect);
        self.send_cmd2(Cmd::Disconnect);
        self.cmd_tx = None; self.evt_rx = None;
        self.cmd_tx2 = None; self.evt_rx2 = None;
        self.screen = Screen::Main; self.frame_tex = None; self.frame_tex2 = None;
        self.show_monitor2 = false;
        self.canvas2 = None; self.canvas2_w = 0; self.canvas2_h = 0;
    }

    fn next_monitor_for_secondary(&self) -> u8 {
        // Pega o menor monitor_id que não é o ativo
        self.monitors.iter()
            .filter(|m| m.index != self.active_monitor)
            .map(|m| m.index)
            .min()
            .unwrap_or(1)
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_events(ctx);

        // Redimensiona janela APENAS quando a tela muda
        if self.screen != self.last_screen {
            let target_size = match self.screen {
                Screen::Settings => Vec2::new(460.0, 500.0),
                Screen::Remote | Screen::Files => Vec2::new(900.0, 600.0),
                _ => Vec2::new(420.0, 310.0),
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(target_size));
            self.last_screen = self.screen.clone();
        }

        // Drag and drop
        let dropped: Vec<egui::DroppedFile> = ctx.input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            if let Some(path) = &file.path {
                let src = path.to_string_lossy().to_string();
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                if self.cmd_tx.is_some() {
                    self.drag_status = format!("Enviando {}...", name);
                    self.send_cmd(Cmd::FileUpload { src });
                } else {
                    self.drag_status = "Conecte-se a um PC remoto primeiro.".to_string();
                }
            }
        }
        let is_hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if is_hovering && self.cmd_tx.is_some() {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.label(egui::RichText::new("Solte para enviar ao PC remoto")
                        .size(18.0).color(egui::Color32::from_rgb(0, 212, 255)));
                });
            });
            return;
        }

        match self.screen.clone() {
            Screen::FirstRun => self.ui_first_run(ctx),
            Screen::Main     => self.ui_main(ctx),
            Screen::Remote   => self.ui_remote(ctx),
            Screen::Files    => self.ui_files(ctx),
            Screen::Settings => self.ui_settings(ctx),
        }
        if self.cmd_tx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.send_cmd(Cmd::Disconnect);
        self.send_cmd2(Cmd::Disconnect);
    }
}

impl App {
    fn ui_first_run(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.label(RichText::new("RemoteLink").size(28.0).color(Color32::from_rgb(0, 212, 255)));
                ui.add_space(4.0);
                ui.label(RichText::new("Primeira execucao — configure sua senha").size(13.0).color(Color32::GRAY));
                ui.add_space(32.0);
                egui::Frame::none().fill(Color32::from_rgb(19,19,26)).rounding(12.0).inner_margin(egui::Margin::same(28.0)).show(ui, |ui| {
                    ui.set_width(340.0);
                    ui.label(RichText::new("Senha").size(11.0).color(Color32::GRAY));
                    ui.add_space(4.0);
                    ui.add(egui::TextEdit::singleline(&mut self.fr_pass1).password(true).desired_width(f32::INFINITY).hint_text("Digite uma senha"));
                    ui.add_space(12.0);
                    ui.label(RichText::new("Confirmar senha").size(11.0).color(Color32::GRAY));
                    ui.add_space(4.0);
                    ui.add(egui::TextEdit::singleline(&mut self.fr_pass2).password(true).desired_width(f32::INFINITY).hint_text("Repita a senha"));
                    ui.add_space(16.0);
                    if !self.fr_error.is_empty() {
                        ui.label(RichText::new(&self.fr_error).color(Color32::from_rgb(255,80,80)).size(12.0));
                        ui.add_space(8.0);
                    }
                    ui.label(RichText::new("Usada para autenticar quem conectar neste PC.").size(11.0).color(Color32::DARK_GRAY));
                    ui.add_space(16.0);
                    let btn = egui::Button::new(RichText::new("Salvar e continuar").size(14.0))
                        .fill(Color32::from_rgb(0,180,220)).min_size(Vec2::new(f32::INFINITY, 36.0));
                    if ui.add(btn).clicked() {
                        if self.fr_pass1.is_empty() { self.fr_error = "Senha nao pode ser vazia.".into(); }
                        else if self.fr_pass1 != self.fr_pass2 { self.fr_error = "Senhas nao coincidem.".into(); }
                        else {
                            self.config.password = self.fr_pass1.clone();
                            self.config.first_run = false;
                            self.config.save();
                            self.cfg_pass = self.config.password.clone();
                            self.screen = Screen::Main;
                        }
                    }
                });
            });
        });
    }

    fn ui_main(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("topbar")
            .frame(egui::Frame::none().fill(Color32::from_rgb(19,19,26)).inner_margin(egui::Margin::symmetric(12.0,8.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("RemoteLink").size(15.0).color(Color32::from_rgb(0,212,255)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Config").clicked() { self.screen = Screen::Settings; }
                    });
                });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(12.0);
            egui::Frame::none().fill(Color32::from_rgb(19,19,26)).rounding(10.0).inner_margin(egui::Margin::same(12.0)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(Color32::from_rgb(34,197,94)).size(13.0));
                    ui.label(RichText::new("Servidor ativo").size(12.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!(":{}", self.config.port)).size(11.0).color(Color32::GRAY));
                    });
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Seu ID:").size(11.0).color(Color32::GRAY));
                    ui.label(RichText::new(&self.config.machine_id).size(13.0).color(Color32::from_rgb(0,212,255)).strong());
                    if ui.small_button("copiar").clicked() {
                        ctx.output_mut(|o| o.copied_text = self.config.machine_id.clone());
                    }
                });
                if self.config.use_relay {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("●").color(Color32::from_rgb(0,212,255)).size(11.0));
                        ui.label(RichText::new(format!("Relay: {}", self.config.relay_host)).size(10.0).color(Color32::GRAY));
                    });
                }
            });

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(10.0);
            ui.label(RichText::new("Conectar em outro PC").size(13.0).strong());
            ui.add_space(8.0);

            egui::Frame::none().fill(Color32::from_rgb(19,19,26)).rounding(10.0).inner_margin(egui::Margin::same(14.0)).show(ui, |ui| {
                egui::Grid::new("conn_grid").num_columns(2).spacing([8.0,8.0]).show(ui, |ui| {
                    ui.label(RichText::new("ID do host remoto").size(11.0).color(Color32::GRAY));
                    ui.add(egui::TextEdit::singleline(&mut self.config.remote_id)
                        .desired_width(f32::INFINITY).hint_text("ex: A7X-92K"));
                    ui.end_row();
                    ui.label(RichText::new("Senha").size(11.0).color(Color32::GRAY));
                    ui.add(egui::TextEdit::singleline(&mut self.pass_buf)
                        .password(true).desired_width(f32::INFINITY).hint_text("senha do peer remoto"));
                    ui.end_row();
                });
                ui.add_space(6.0);
                if !self.drag_status.is_empty() {
                    let color = if self.drag_status.starts_with("Enviando") {
                        egui::Color32::from_rgb(0,212,255)
                    } else {
                        egui::Color32::from_rgb(255,80,80)
                    };
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new(&self.drag_status).size(11.0).color(color));
                    });
                    ui.add_space(4.0);
                }
                if !self.conn_error.is_empty() {
                    ui.label(RichText::new(&self.conn_error).color(Color32::from_rgb(255,80,80)).size(11.0));
                    ui.add_space(4.0);
                }
                ui.vertical_centered(|ui| {
                    let label = if self.connecting { "Conectando..." } else { "Conectar" };
                    let btn = egui::Button::new(RichText::new(label).size(13.0))
                        .fill(if self.connecting { Color32::DARK_GRAY } else { Color32::from_rgb(0,180,220) })
                        .min_size(Vec2::new(160.0, 32.0));
                    if ui.add_enabled(!self.connecting && !self.config.remote_id.trim().is_empty(), btn).clicked() {
                        self.do_connect();
                    }
                });
            });
        });
    }

    fn ui_remote(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("remote_bar")
            .frame(egui::Frame::none().fill(Color32::from_rgb(19,19,26)).inner_margin(egui::Margin::symmetric(8.0,6.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} {}x{}", self.peer_platform, self.server_w, self.server_h)).size(11.0).color(Color32::from_rgb(0,212,255)));
                    ui.separator();
                    if ui.small_button("Arquivos").clicked() { self.send_cmd(Cmd::FileList); self.screen = Screen::Files; }
                    if ui.small_button("Clipboard").clicked() {
                        if let Ok(mut c) = arboard::Clipboard::new() {
                            if let Ok(t) = c.get_text() { self.send_cmd(Cmd::Clipboard(t)); }
                        }
                    }
                    if ui.small_button("CAD").clicked() {
                        for k in &["ctrl","alt","delete"] { self.send_cmd(Cmd::Input(InputEvent::KeyDown { key: k.to_string() })); }
                        for k in &["delete","alt","ctrl"] { self.send_cmd(Cmd::Input(InputEvent::KeyUp   { key: k.to_string() })); }
                    }

                    // Ícones de monitor numerados — troca na mesma sessão
                    if self.monitors.len() > 1 {
                        ui.separator();
                        for m in self.monitors.clone() {
                            let label = format!("🖥{}", m.index + 1);
                            let is_active = m.index == self.active_monitor;
                            let color = if is_active { Color32::from_rgb(0,212,255) } else { Color32::GRAY };
                            if ui.add(egui::Button::new(RichText::new(&label).color(color).size(11.0))
                                .frame(is_active)).clicked()
                            {
                                self.active_monitor = m.index;
                                self.send_cmd(Cmd::SwitchMonitor { index: m.index });
                                // Reseta canvas para evitar mouse desalinhado
                                self.canvas = None; self.canvas_w = 0; self.canvas_h = 0;
                                self.frame_tex = None;
                            }
                        }
                        // Botão monitor+ — abre segunda conexão independente
                        let mon2_color = if self.show_monitor2 { Color32::from_rgb(0,212,255) } else { Color32::GRAY };
                        if ui.add(egui::Button::new(RichText::new("🖥+").color(mon2_color).size(11.0))
                            .frame(self.show_monitor2)).clicked()
                        {
                            if self.show_monitor2 {
                                self.show_monitor2 = false;
                                self.send_cmd2(Cmd::Disconnect);
                                self.cmd_tx2 = None; self.evt_rx2 = None;
                                self.frame_tex2 = None;
                                self.canvas2 = None; self.canvas2_w = 0; self.canvas2_h = 0;
                            } else {
                                let next_mon = self.next_monitor_for_secondary();
                                // Spawna novo processo independente
                                let exe = std::env::current_exe().unwrap_or_default();
                                let remote_id = self.config.remote_id.trim().to_string();
                                let _ = std::process::Command::new(&exe)
                                    .arg("--connect")
                                    .arg(&remote_id)
                                    .arg("--monitor")
                                    .arg(next_mon.to_string())
                                    .spawn();
                                self.show_monitor2 = true;
                            }
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(RichText::new("Desconectar").color(Color32::from_rgb(255,80,80))).clicked() { self.disconnect(); }
                        ui.label(RichText::new(format!("FPS {:.0}", self.fps_display)).size(10.0).color(Color32::DARK_GRAY));
                    });
                });
            });

        egui::CentralPanel::default().frame(egui::Frame::none().fill(Color32::BLACK)).show(ctx, |ui| {
            let available = ui.available_size();
            if let Some(tex) = &self.frame_tex {
                let resp = ui.add(egui::Image::new(tex).fit_to_exact_size(available).sense(egui::Sense::click_and_drag()));
                let rect = resp.rect;
                let sw = if self.canvas_w > 0 { self.canvas_w } else { self.server_w };
                let sh = if self.canvas_h > 0 { self.canvas_h } else { self.server_h };
                let sx = sw as f32 / rect.width();
                let sy = sh as f32 / rect.height();
                let to_srv = |p: egui::Pos2| -> (i32,i32) {
                    (((p.x - rect.left()) * sx) as i32, ((p.y - rect.top()) * sy) as i32)
                };
                if let Some(pos) = resp.hover_pos() {
                    let (x,y) = to_srv(pos);
                    self.send_cmd(Cmd::Input(InputEvent::MouseMove { x, y }));
                }
                if resp.clicked() {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let (x,y) = to_srv(pos);
                        self.send_cmd(Cmd::Input(InputEvent::MouseDown { x, y, button: MouseBtn::Left }));
                        self.send_cmd(Cmd::Input(InputEvent::MouseUp   { x, y, button: MouseBtn::Left }));
                    }
                }
                if resp.drag_started_by(egui::PointerButton::Primary) {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let (x,y) = to_srv(pos);
                        self.send_cmd(Cmd::Input(InputEvent::MouseDown { x, y, button: MouseBtn::Left }));
                    }
                }
                if resp.drag_stopped_by(egui::PointerButton::Primary) {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let (x,y) = to_srv(pos);
                        self.send_cmd(Cmd::Input(InputEvent::MouseUp { x, y, button: MouseBtn::Left }));
                    }
                }
                if resp.secondary_clicked() {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let (x,y) = to_srv(pos);
                        self.send_cmd(Cmd::Input(InputEvent::MouseDown { x, y, button: MouseBtn::Right }));
                        self.send_cmd(Cmd::Input(InputEvent::MouseUp   { x, y, button: MouseBtn::Right }));
                    }
                }
                let scroll = ui.input(|i| i.raw_scroll_delta);
                if scroll.y != 0.0 {
                    if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                        let (x,y) = to_srv(pos);
                        self.send_cmd(Cmd::Input(InputEvent::Scroll { x, y, dy: scroll.y.signum() as i32 * 3 }));
                    }
                }
            } else {
                ui.centered_and_justified(|ui| { ui.label(RichText::new("Aguardando frame...").color(Color32::GRAY)); });
            }
            ctx.input(|i| {
                for ev in &i.events {
                    match ev {
                        egui::Event::Key { key, pressed, .. } => {
                            if let Some(k) = egui_key_str(*key) {
                                let cmd = if *pressed { InputEvent::KeyDown { key: k } } else { InputEvent::KeyUp { key: k } };
                                self.send_cmd(Cmd::Input(cmd));
                            }
                        }
                        egui::Event::Text(text) => { self.send_cmd(Cmd::Input(InputEvent::TypeText { text: text.clone() })); }
                        _ => {}
                    }
                }
            });
        });
    }

    fn ui_files(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("files_bar")
            .frame(egui::Frame::none().fill(Color32::from_rgb(19,19,26)).inner_margin(egui::Margin::symmetric(8.0,6.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Arquivos").size(13.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Voltar").clicked() { self.screen = Screen::Remote; }
                    });
                });
            });
        egui::TopBottomPanel::bottom("files_status")
            .frame(egui::Frame::none().fill(Color32::from_rgb(19,19,26)).inner_margin(egui::Margin::symmetric(12.0,5.0)))
            .show(ctx, |ui| {
                let color = if self.file_status.starts_with("Erro") { Color32::from_rgb(255,80,80) } else { Color32::from_rgb(34,211,165) };
                ui.label(RichText::new(&self.file_status).size(11.0).color(color));
            });
        egui::SidePanel::right("upload_panel").resizable(false).default_width(180.0)
            .frame(egui::Frame::none().fill(Color32::from_rgb(19,19,26)).inner_margin(egui::Margin::same(12.0)))
            .show(ctx, |ui| {
                ui.label(RichText::new("Enviar pro servidor").size(11.0).color(Color32::GRAY));
                ui.add_space(8.0);
                if ui.button("Enviar arquivo...").clicked() {}
                ui.add_space(8.0);
                if ui.button("Atualizar lista").clicked() { self.send_cmd(Cmd::FileList); }
                ui.add_space(6.0);
                ui.label(RichText::new(format!("{}", self.file_folder)).size(9.0).color(Color32::DARK_GRAY));
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (i, item) in self.file_items.iter().enumerate() {
                    let icon = if item.kind == "dir" { "[D]" } else { "[F]" };
                    let selected = self.file_selected == Some(i);
                    let size_str = if item.kind == "file" { format_bytes(item.size) } else { "pasta".into() };
                    let resp = ui.selectable_label(selected, RichText::new(format!("{} {}  ({})", icon, item.name, size_str)).size(12.0));
                    if resp.clicked() { self.file_selected = Some(i); }
                    if resp.double_clicked() && item.kind == "file" {
                        self.file_status = format!("Baixando {}...", item.name);
                        self.send_cmd(Cmd::FileDownload { filename: item.name.clone(), path: item.path.clone() });
                    }
                }
            });
            if let Some(i) = self.file_selected {
                if let Some(item) = self.file_items.get(i) {
                    if item.kind == "file" {
                        ui.add_space(8.0);
                        if ui.button("Baixar selecionado").clicked() {
                            self.file_status = format!("Baixando {}...", item.name);
                            self.send_cmd(Cmd::FileDownload { filename: item.name.clone(), path: item.path.clone() });
                        }
                    }
                }
            }
        });
    }

    fn ui_settings(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("cfg_bar")
            .frame(egui::Frame::none().fill(Color32::from_rgb(19,19,26)).inner_margin(egui::Margin::symmetric(8.0,6.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Configuracoes").size(13.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Voltar").clicked() { self.screen = Screen::Main; }
                    });
                });
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(10.0);
            egui::Frame::none().fill(Color32::from_rgb(19,19,26)).rounding(8.0).inner_margin(egui::Margin::same(12.0)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Seu ID:").size(11.0).color(Color32::GRAY));
                    ui.label(RichText::new(&self.config.machine_id).size(14.0).color(Color32::from_rgb(0,212,255)).strong());
                    if ui.small_button("copiar").clicked() {
                        ctx.output_mut(|o| o.copied_text = self.config.machine_id.clone());
                    }
                });
                ui.label(RichText::new("Compartilhe este ID com quem quiser te acessar.").size(10.0).color(Color32::DARK_GRAY));
            });
            ui.add_space(10.0);
            ui.label(RichText::new("Servidor (este PC)").size(11.0).color(Color32::GRAY));
            ui.add_space(6.0);
            egui::Grid::new("cfg_grid").num_columns(2).spacing([10.0,8.0]).show(ui, |ui| {
                ui.label(RichText::new("Senha").size(11.0).color(Color32::GRAY));
                ui.add(egui::TextEdit::singleline(&mut self.cfg_pass).password(true).desired_width(200.0));
                ui.end_row();
                ui.label(RichText::new("Porta").size(11.0).color(Color32::GRAY));
                ui.add(egui::TextEdit::singleline(&mut self.cfg_port).desired_width(70.0));
                ui.end_row();
                ui.label(RichText::new("FPS").size(11.0).color(Color32::GRAY));
                ui.add(egui::TextEdit::singleline(&mut self.cfg_fps).desired_width(50.0));
                ui.end_row();
                ui.label(RichText::new("Qualidade JPEG").size(11.0).color(Color32::GRAY));
                ui.add(egui::TextEdit::singleline(&mut self.cfg_quality).desired_width(50.0));
                ui.end_row();
                ui.label(RichText::new("Pasta compartilhada").size(11.0).color(Color32::GRAY));
                ui.add(egui::TextEdit::singleline(&mut self.cfg_folder).desired_width(260.0));
                ui.end_row();
            });
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Relay (internet sem VPN)").size(11.0).color(Color32::GRAY));
                let label = if self.config.use_relay {
                    RichText::new("● Ativo").color(Color32::from_rgb(0,212,255)).size(11.0)
                } else {
                    RichText::new("○ Inativo").color(Color32::DARK_GRAY).size(11.0)
                };
                if ui.button(label).clicked() { self.config.use_relay = !self.config.use_relay; }
            });
            if self.config.use_relay {
                ui.add_space(6.0);
                egui::Grid::new("relay_cfg").num_columns(2).spacing([10.0,8.0]).show(ui, |ui| {
                    ui.label(RichText::new("Relay host").size(11.0).color(Color32::GRAY));
                    ui.add(egui::TextEdit::singleline(&mut self.cfg_relay_host).desired_width(220.0).hint_text("IP ou dominio do relay"));
                    ui.end_row();
                    ui.label(RichText::new("Relay porta").size(11.0).color(Color32::GRAY));
                    ui.add(egui::TextEdit::singleline(&mut self.cfg_relay_port).desired_width(70.0).hint_text("7891"));
                    ui.end_row();
                });
            }
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                let btn = egui::Button::new(RichText::new("Salvar").size(13.0))
                    .fill(Color32::from_rgb(0,180,220)).min_size(Vec2::new(140.0,32.0));
                if ui.add(btn).clicked() {
                    self.config.password      = self.cfg_pass.clone();
                    self.config.port          = self.cfg_port.parse().unwrap_or(7890);
                    self.config.fps           = self.cfg_fps.parse().unwrap_or(15);
                    self.config.jpeg_quality  = self.cfg_quality.parse::<u8>().unwrap_or(55).clamp(10,95);
                    self.config.shared_folder = self.cfg_folder.clone();
                    self.config.relay_host    = self.cfg_relay_host.clone();
                    self.config.relay_port    = self.cfg_relay_port.parse().unwrap_or(7891);
                    self.config.save();
                    self.cfg_saved = true;
                }
            });
            if self.cfg_saved {
                ui.add_space(4.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("Salvo! Reinicie para aplicar mudanca de porta.").size(11.0).color(Color32::from_rgb(34,211,165)));
                });
            }
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(RichText::new(format!("Config: {}/.remote-link.json",
                dirs::home_dir().unwrap_or_default().display())).size(9.0).color(Color32::DARK_GRAY));
        });
    }
}

fn get_local_ip() -> String {
    use std::net::UdpSocket;
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| { s.connect("8.8.8.8:80")?; s.local_addr() })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".into())
}

fn format_bytes(b: u64) -> String {
    if b < 1024 { format!("{} B", b) }
    else if b < 1_048_576 { format!("{:.1} KB", b as f64 / 1024.0) }
    else { format!("{:.1} MB", b as f64 / 1_048_576.0) }
}

fn egui_key_str(key: egui::Key) -> Option<String> {
    Some(match key {
        egui::Key::Enter      => "enter",    egui::Key::Backspace  => "backspace",
        egui::Key::Tab        => "tab",      egui::Key::Escape     => "escape",
        egui::Key::Delete     => "delete",   egui::Key::Home       => "home",
        egui::Key::End        => "end",      egui::Key::PageUp     => "pageup",
        egui::Key::PageDown   => "pagedown", egui::Key::ArrowUp    => "up",
        egui::Key::ArrowDown  => "down",     egui::Key::ArrowLeft  => "left",
        egui::Key::ArrowRight => "right",
        egui::Key::F1  => "f1",  egui::Key::F2  => "f2",  egui::Key::F3  => "f3",
        egui::Key::F4  => "f4",  egui::Key::F5  => "f5",  egui::Key::F6  => "f6",
        egui::Key::F7  => "f7",  egui::Key::F8  => "f8",  egui::Key::F9  => "f9",
        egui::Key::F10 => "f10", egui::Key::F11 => "f11", egui::Key::F12 => "f12",
        _ => return None,
    }.to_string())
}

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt().with_env_filter("remote_link=info").init();

    // Parsing de argumentos de linha de comando
    // Uso: remote-link.exe --connect ID --monitor N
    let args: Vec<String> = std::env::args().collect();
    let mut auto_connect_id: Option<String> = None;
    let mut auto_monitor: Option<u8> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--connect" => { i += 1; if i < args.len() { auto_connect_id = Some(args[i].clone()); } }
            "--monitor" => { i += 1; if i < args.len() { auto_monitor = args[i].parse().ok(); } }
            _ => {}
        }
        i += 1;
    }

    let icon = egui::IconData {
        rgba:   icon::ICON_RGBA.to_vec(),
        width:  icon::ICON_WIDTH,
        height: icon::ICON_HEIGHT,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RemoteLink")
            .with_inner_size([420.0, 310.0])
            .with_min_inner_size([380.0, 280.0])
            .with_icon(std::sync::Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native("RemoteLink", options, Box::new(|cc| {
        let mut app = App::new(cc);
        // Se recebeu --connect, conecta automaticamente ao monitor especificado
        if let Some(id) = auto_connect_id {
            app.config.remote_id = id.clone();
            app.do_connect_monitor(auto_monitor);
            // Marca que é uma janela secundária para não registrar relay duplo
            app.is_secondary = true;
        }
        Box::new(app)
    }))
}
