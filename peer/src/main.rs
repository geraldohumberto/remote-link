// src/main.rs — RemoteLink peer
// egui self-contained + system tray + servidor TCP + cliente TCP

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod client;
mod config;
mod input;
mod protocol;
mod server;

use std::sync::{Arc, Mutex};
use eframe::egui;
use egui::{Color32, RichText, Vec2, Visuals};
use tokio::sync::mpsc;
use tokio::runtime::Runtime;
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use image::DynamicImage;

use client::{Cmd, Evt};
use config::Config;
use protocol::{InputEvent, MouseBtn};

// ── Telas do app ──────────────────────────────────────────────────────────
#[derive(PartialEq, Clone)]
enum Screen {
    FirstRun,   // primeira execução: definir senha
    Main,       // tela principal: status + conectar
    Remote,     // controle remoto ativo
    Files,      // gerenciador de arquivos
    Settings,   // configurações
}

// ── Estado global ─────────────────────────────────────────────────────────
struct App {
    config:     Config,
    screen:     Screen,
    rt:         Arc<Runtime>,

    // First run
    fr_pass1:   String,
    fr_pass2:   String,
    fr_error:   String,

    // Main / connect
    host_buf:   String,
    port_buf:   String,
    pass_buf:   String,
    conn_error: String,
    connecting: bool,

    // Sessão ativa
    cmd_tx:     Option<mpsc::Sender<Cmd>>,
    evt_rx:     Option<mpsc::Receiver<Evt>>,
    server_w:   u32,
    server_h:   u32,
    peer_platform: String,

    // Frame remoto
    frame_tex:  Option<egui::TextureHandle>,
    fps_count:  u32,
    fps_last:   std::time::Instant,
    fps_display: f32,

    // Arquivos
    file_items: Vec<protocol::FileItem>,
    file_folder: String,
    file_selected: Option<usize>,
    file_status: String,

    // Configurações (buffers editáveis)
    cfg_pass:   String,
    cfg_port:   String,
    cfg_fps:    String,
    cfg_quality: String,
    cfg_folder: String,
    cfg_saved:  bool,

    // IP local
    local_ip:   String,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Tema escuro customizado
        let mut vis = Visuals::dark();
        vis.panel_fill          = Color32::from_rgb(13, 13, 15);
        vis.window_fill         = Color32::from_rgb(19, 19, 26);
        vis.extreme_bg_color    = Color32::from_rgb(10, 10, 12);
        vis.widgets.noninteractive.bg_fill = Color32::from_rgb(19, 19, 26);
        vis.widgets.inactive.bg_fill       = Color32::from_rgb(25, 25, 35);
        vis.widgets.hovered.bg_fill        = Color32::from_rgb(35, 35, 50);
        vis.widgets.active.bg_fill         = Color32::from_rgb(0, 180, 220);
        cc.egui_ctx.set_visuals(vis);

        let config = Config::load();
        let screen = if config.first_run { Screen::FirstRun } else { Screen::Main };

        let cfg_pass    = config.password.clone();
        let cfg_port    = config.port.to_string();
        let cfg_fps     = config.fps.to_string();
        let cfg_quality = config.jpeg_quality.to_string();
        let cfg_folder  = config.shared_folder.clone();
        let host_buf    = config.last_host.clone();
        let port_buf    = config.last_port.to_string();
        let local_ip    = get_local_ip();

        let rt = Arc::new(Runtime::new().expect("tokio runtime"));

        // Inicia servidor TCP em background
        {
            let cfg_arc = Arc::new(config.clone());
            let rt2 = rt.clone();
            rt2.spawn(server::run(cfg_arc));
        }

        Self {
            config, screen, rt,
            fr_pass1: String::new(), fr_pass2: String::new(), fr_error: String::new(),
            host_buf, port_buf, pass_buf: String::new(), conn_error: String::new(), connecting: false,
            cmd_tx: None, evt_rx: None,
            server_w: 1920, server_h: 1080, peer_platform: String::new(),
            frame_tex: None, fps_count: 0, fps_last: std::time::Instant::now(), fps_display: 0.0,
            file_items: Vec::new(), file_folder: String::new(), file_selected: None,
            file_status: String::new(),
            cfg_pass, cfg_port, cfg_fps, cfg_quality, cfg_folder, cfg_saved: false,
            local_ip,
        }
    }

    // ── Processar eventos da sessão remota ───────────────────────────────
    fn poll_events(&mut self, ctx: &egui::Context) {
        let events: Vec<Evt> = {
            let Some(rx) = self.evt_rx.as_mut() else { return };
            let mut evs = Vec::new();
            while let Ok(e) = rx.try_recv() { evs.push(e); }
            evs
        };

        for evt in events {
            match evt {
                Evt::Connected { screen_w, screen_h, platform } => {
                    self.server_w       = screen_w;
                    self.server_h       = screen_h;
                    self.peer_platform  = platform;
                    self.screen         = Screen::Remote;
                    self.conn_error     = String::new();
                    self.connecting     = false;
                }
                Evt::Frame { jpeg } => {
                    if let Ok(img) = image::load_from_memory(&jpeg) {
                        let rgba = img.to_rgba8();
                        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
                        let ci = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
                        self.frame_tex = Some(ctx.load_texture("frame", ci, egui::TextureOptions::LINEAR));
                    }
                    // FPS
                    self.fps_count += 1;
                    let elapsed = self.fps_last.elapsed().as_secs_f32();
                    if elapsed >= 1.0 {
                        self.fps_display = self.fps_count as f32 / elapsed;
                        self.fps_count = 0;
                        self.fps_last = std::time::Instant::now();
                    }
                    ctx.request_repaint();
                }
                Evt::FileList { folder, items } => {
                    self.file_folder = folder;
                    self.file_items  = items;
                    self.file_status = String::new();
                }
                Evt::FileDone { filename, bytes } => {
                    self.file_status = format!("✓ {} ({} bytes)", filename, bytes);
                    self.send_cmd(Cmd::FileList);
                }
                Evt::FileError { reason } => {
                    self.file_status = format!("Erro: {}", reason);
                }
                Evt::Clipboard { text } => {
                    ctx.output_mut(|o| o.copied_text = text);
                }
                Evt::Error { reason } => {
                    self.conn_error = reason;
                    self.connecting = false;
                    self.screen     = Screen::Main;
                    self.cmd_tx     = None;
                    self.evt_rx     = None;
                }
                Evt::Disconnected => {
                    self.screen  = Screen::Main;
                    self.cmd_tx  = None;
                    self.evt_rx  = None;
                    self.frame_tex = None;
                }
                _ => {}
            }
        }
    }

    fn send_cmd(&self, cmd: Cmd) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.try_send(cmd);
        }
    }

    fn do_connect(&mut self) {
        let host = self.host_buf.trim().to_string();
        let port = self.port_buf.trim().parse::<u16>().unwrap_or(7890);
        let pass = if self.pass_buf.is_empty() {
            self.config.password.clone()
        } else {
            self.pass_buf.clone()
        };

        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>(64);
        let (evt_tx, evt_rx) = mpsc::channel::<Evt>(256);

        self.cmd_tx     = Some(cmd_tx);
        self.evt_rx     = Some(evt_rx);
        self.connecting = true;
        self.conn_error = String::new();

        // Salva last_host
        self.config.last_host = host.clone();
        self.config.last_port = port;
        self.config.save();

        self.rt.spawn(client::connect(host, port, pass, cmd_rx, evt_tx));
    }

    fn disconnect(&mut self) {
        self.send_cmd(Cmd::Disconnect);
        self.cmd_tx  = None;
        self.evt_rx  = None;
        self.screen  = Screen::Main;
        self.frame_tex = None;
    }
}

// ── egui App trait ────────────────────────────────────────────────────────
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_events(ctx);

        match self.screen.clone() {
            Screen::FirstRun => self.ui_first_run(ctx),
            Screen::Main     => self.ui_main(ctx),
            Screen::Remote   => self.ui_remote(ctx),
            Screen::Files    => self.ui_files(ctx),
            Screen::Settings => self.ui_settings(ctx),
        }

        // Repaint contínuo quando tem sessão ativa
        if self.cmd_tx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.send_cmd(Cmd::Disconnect);
    }
}

// ── Telas ─────────────────────────────────────────────────────────────────
impl App {
    fn ui_first_run(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.label(RichText::new("🔗 RemoteLink").size(28.0).color(Color32::from_rgb(0, 212, 255)));
                ui.add_space(4.0);
                ui.label(RichText::new("Primeira execução — configure sua senha").size(13.0).color(Color32::GRAY));
                ui.add_space(32.0);

                egui::Frame::none()
                    .fill(Color32::from_rgb(19, 19, 26))
                    .rounding(12.0)
                    .inner_margin(egui::Margin::same(28.0))
                    .show(ui, |ui| {
                        ui.set_width(340.0);
                        ui.label(RichText::new("Senha").size(11.0).color(Color32::GRAY));
                        ui.add_space(4.0);
                        ui.add(egui::TextEdit::singleline(&mut self.fr_pass1)
                            .password(true).desired_width(f32::INFINITY)
                            .hint_text("Digite uma senha"));
                        ui.add_space(12.0);
                        ui.label(RichText::new("Confirmar senha").size(11.0).color(Color32::GRAY));
                        ui.add_space(4.0);
                        ui.add(egui::TextEdit::singleline(&mut self.fr_pass2)
                            .password(true).desired_width(f32::INFINITY)
                            .hint_text("Repita a senha"));
                        ui.add_space(16.0);

                        if !self.fr_error.is_empty() {
                            ui.label(RichText::new(&self.fr_error).color(Color32::from_rgb(255, 80, 80)).size(12.0));
                            ui.add_space(8.0);
                        }

                        ui.label(RichText::new(
                            "A senha será usada para autenticar quem se conectar neste PC.\nVocê pode mudar depois em Configurações."
                        ).size(11.0).color(Color32::DARK_GRAY));
                        ui.add_space(16.0);

                        let btn = egui::Button::new(RichText::new("Salvar e continuar").size(14.0))
                            .fill(Color32::from_rgb(0, 180, 220))
                            .min_size(Vec2::new(f32::INFINITY, 36.0));
                        if ui.add(btn).clicked() {
                            if self.fr_pass1.is_empty() {
                                self.fr_error = "A senha não pode ser vazia.".into();
                            } else if self.fr_pass1 != self.fr_pass2 {
                                self.fr_error = "As senhas não coincidem.".into();
                            } else {
                                self.config.password  = self.fr_pass1.clone();
                                self.config.first_run = false;
                                self.config.save();
                                self.cfg_pass = self.config.password.clone();
                                self.screen   = Screen::Main;
                            }
                        }
                    });
            });
        });
    }

    fn ui_main(&mut self, ctx: &egui::Context) {
        // Topbar
        egui::TopBottomPanel::top("topbar")
            .frame(egui::Frame::none().fill(Color32::from_rgb(19, 19, 26)).inner_margin(egui::Margin::symmetric(12.0, 8.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("🔗 RemoteLink").size(15.0).color(Color32::from_rgb(0, 212, 255)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("⚙ Config").clicked() { self.screen = Screen::Settings; }
                    });
                });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(20.0);

            // Status do servidor local
            egui::Frame::none()
                .fill(Color32::from_rgb(19, 19, 26))
                .rounding(10.0)
                .inner_margin(egui::Margin::same(16.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("●").color(Color32::from_rgb(34, 197, 94)).size(14.0));
                        ui.label(RichText::new("Servidor ativo").size(13.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(format!("porta {}", self.config.port)).size(12.0).color(Color32::GRAY));
                        });
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Seu IP local:").size(12.0).color(Color32::GRAY));
                        ui.label(RichText::new(&self.local_ip).size(12.0).color(Color32::from_rgb(0, 212, 255)));
                        if ui.small_button("copiar").clicked() {
                            ctx.output_mut(|o| o.copied_text = self.local_ip.clone());
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(RichText::new("Passe este IP + senha pra quem vai se conectar aqui.").size(11.0).color(Color32::DARK_GRAY));
                });

            ui.add_space(20.0);
            ui.separator();
            ui.add_space(16.0);

            // Conectar em outro peer
            ui.label(RichText::new("Conectar em outro PC").size(14.0).strong());
            ui.add_space(10.0);

            egui::Frame::none()
                .fill(Color32::from_rgb(19, 19, 26))
                .rounding(10.0)
                .inner_margin(egui::Margin::same(16.0))
                .show(ui, |ui| {
                    egui::Grid::new("conn_grid").num_columns(2).spacing([8.0, 10.0]).show(ui, |ui| {
                        ui.label(RichText::new("Host / IP").size(12.0).color(Color32::GRAY));
                        ui.add(egui::TextEdit::singleline(&mut self.host_buf)
                            .desired_width(f32::INFINITY).hint_text("192.168.1.100"));
                        ui.end_row();

                        ui.label(RichText::new("Porta").size(12.0).color(Color32::GRAY));
                        ui.add(egui::TextEdit::singleline(&mut self.port_buf)
                            .desired_width(80.0).hint_text("7890"));
                        ui.end_row();

                        ui.label(RichText::new("Senha").size(12.0).color(Color32::GRAY));
                        ui.add(egui::TextEdit::singleline(&mut self.pass_buf)
                            .password(true).desired_width(f32::INFINITY).hint_text("senha do peer remoto"));
                        ui.end_row();
                    });

                    ui.add_space(12.0);

                    if !self.conn_error.is_empty() {
                        ui.label(RichText::new(&self.conn_error).color(Color32::from_rgb(255, 80, 80)).size(12.0));
                        ui.add_space(8.0);
                    }

                    let label = if self.connecting { "Conectando..." } else { "⚡ Conectar" };
                    let btn = egui::Button::new(RichText::new(label).size(14.0))
                        .fill(if self.connecting { Color32::DARK_GRAY } else { Color32::from_rgb(0, 180, 220) })
                        .min_size(Vec2::new(f32::INFINITY, 36.0));
                    if ui.add_enabled(!self.connecting && !self.host_buf.trim().is_empty(), btn).clicked() {
                        self.do_connect();
                    }
                });
        });
    }

    fn ui_remote(&mut self, ctx: &egui::Context) {
        // Toolbar
        egui::TopBottomPanel::top("remote_bar")
            .frame(egui::Frame::none().fill(Color32::from_rgb(19, 19, 26)).inner_margin(egui::Margin::symmetric(8.0, 6.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("● {} — {}×{}", self.peer_platform, self.server_w, self.server_h))
                        .size(12.0).color(Color32::from_rgb(0, 212, 255)));
                    ui.separator();
                    if ui.small_button("📁 Arquivos").clicked() {
                        self.send_cmd(Cmd::FileList);
                        self.screen = Screen::Files;
                    }
                    if ui.small_button("📋 Clipboard").clicked() {
                        if let Ok(mut c) = arboard::Clipboard::new() {
                            if let Ok(text) = c.get_text() {
                                self.send_cmd(Cmd::Clipboard(text));
                            }
                        }
                    }
                    if ui.small_button("Ctrl+Alt+Del").clicked() {
                        for k in &["ctrl", "alt", "delete"] {
                            self.send_cmd(Cmd::Input(InputEvent::KeyDown { key: k.to_string() }));
                        }
                        for k in &["delete", "alt", "ctrl"] {
                            self.send_cmd(Cmd::Input(InputEvent::KeyUp { key: k.to_string() }));
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(RichText::new("✕ Desconectar").color(Color32::from_rgb(255, 80, 80))).clicked() {
                            self.disconnect();
                        }
                        ui.label(RichText::new(format!("FPS {:.0}", self.fps_display)).size(11.0).color(Color32::DARK_GRAY));
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(Color32::BLACK))
            .show(ctx, |ui| {
                let available = ui.available_size();

                if let Some(tex) = &self.frame_tex {
                    let resp = ui.add(
                        egui::Image::new(tex)
                            .fit_to_exact_size(available)
                            .sense(egui::Sense::click_and_drag())
                    );

                    let rect = resp.rect;
                    let scale_x = self.server_w as f32 / rect.width();
                    let scale_y = self.server_h as f32 / rect.height();

                    let to_server = |p: egui::Pos2| -> (i32, i32) {
                        let rx = ((p.x - rect.left()) * scale_x) as i32;
                        let ry = ((p.y - rect.top())  * scale_y) as i32;
                        (rx, ry)
                    };

                    // Mouse move
                    if let Some(pos) = resp.hover_pos() {
                        let (x, y) = to_server(pos);
                        self.send_cmd(Cmd::Input(InputEvent::MouseMove { x, y }));
                    }

                    // Clicks
                    if resp.clicked() {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            let (x, y) = to_server(pos);
                            self.send_cmd(Cmd::Input(InputEvent::MouseDown { x, y, button: MouseBtn::Left }));
                            self.send_cmd(Cmd::Input(InputEvent::MouseUp   { x, y, button: MouseBtn::Left }));
                        }
                    }
                    if resp.secondary_clicked() {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            let (x, y) = to_server(pos);
                            self.send_cmd(Cmd::Input(InputEvent::MouseDown { x, y, button: MouseBtn::Right }));
                            self.send_cmd(Cmd::Input(InputEvent::MouseUp   { x, y, button: MouseBtn::Right }));
                        }
                    }

                    // Scroll
                    let scroll = ui.input(|i| i.raw_scroll_delta);
                    if scroll.y != 0.0 {
                        if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                            let (x, y) = to_server(pos);
                            self.send_cmd(Cmd::Input(InputEvent::Scroll { x, y, dy: scroll.y.signum() as i32 * 3 }));
                        }
                    }
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.label(RichText::new("Aguardando frame...").color(Color32::GRAY));
                    });
                }

                // Teclado (quando o painel está em foco)
                ctx.input(|i| {
                    for ev in &i.events {
                        match ev {
                            egui::Event::Key { key, pressed, .. } => {
                                let k = egui_key_to_str(*key);
                                if let Some(k) = k {
                                    let cmd = if *pressed { InputEvent::KeyDown { key: k } } else { InputEvent::KeyUp { key: k } };
                                    self.send_cmd(Cmd::Input(cmd));
                                }
                            }
                            egui::Event::Text(text) => {
                                self.send_cmd(Cmd::Input(InputEvent::TypeText { text: text.clone() }));
                            }
                            _ => {}
                        }
                    }
                });
            });
    }

    fn ui_files(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("files_bar")
            .frame(egui::Frame::none().fill(Color32::from_rgb(19, 19, 26)).inner_margin(egui::Margin::symmetric(8.0, 6.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("📁 Arquivos").size(14.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("← Voltar").clicked() { self.screen = Screen::Remote; }
                    });
                });
            });

        egui::TopBottomPanel::bottom("files_status")
            .frame(egui::Frame::none().fill(Color32::from_rgb(19, 19, 26)).inner_margin(egui::Margin::symmetric(12.0, 6.0)))
            .show(ctx, |ui| {
                ui.label(RichText::new(&self.file_status).size(11.0).color(
                    if self.file_status.starts_with("Erro") { Color32::from_rgb(255, 80, 80) }
                    else { Color32::from_rgb(34, 211, 165) }
                ));
            });

        egui::SidePanel::right("upload_panel")
            .resizable(false).default_width(200.0)
            .frame(egui::Frame::none().fill(Color32::from_rgb(19, 19, 26)).inner_margin(egui::Margin::same(12.0)))
            .show(ctx, |ui| {
                ui.label(RichText::new("Enviar para o servidor").size(12.0).color(Color32::GRAY));
                ui.add_space(8.0);
                if ui.button("⬆ Escolher arquivo...").clicked() {
                    if let Some(path) = rfd_pick_file() {
                        self.file_status = format!("Enviando {}...", std::path::Path::new(&path).file_name().unwrap_or_default().to_string_lossy());
                        self.send_cmd(Cmd::FileUpload { src: path });
                    }
                }
                ui.add_space(12.0);
                if ui.button("🔄 Atualizar lista").clicked() {
                    self.send_cmd(Cmd::FileList);
                }
                ui.add_space(4.0);
                ui.label(RichText::new(format!("Pasta: {}", self.file_folder)).size(10.0).color(Color32::DARK_GRAY));
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (i, item) in self.file_items.iter().enumerate() {
                    let icon     = if item.kind == "dir" { "📁" } else { "📄" };
                    let selected = self.file_selected == Some(i);
                    let label    = format!("{} {}  ({})", icon, item.name,
                        if item.kind == "file" { format_bytes(item.size) } else { "pasta".into() });

                    let resp = ui.selectable_label(selected, RichText::new(&label).size(13.0));
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
                        let btn = egui::Button::new("⬇ Baixar selecionado")
                            .fill(Color32::from_rgb(0, 130, 160));
                        if ui.add(btn).clicked() {
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
            .frame(egui::Frame::none().fill(Color32::from_rgb(19, 19, 26)).inner_margin(egui::Margin::symmetric(8.0, 6.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("⚙ Configurações").size(14.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("← Voltar").clicked() { self.screen = Screen::Main; }
                    });
                });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(16.0);
            egui::Grid::new("cfg_grid").num_columns(2).spacing([12.0, 12.0]).show(ui, |ui| {
                ui.label(RichText::new("Senha").size(12.0).color(Color32::GRAY));
                ui.add(egui::TextEdit::singleline(&mut self.cfg_pass).password(true).desired_width(200.0));
                ui.end_row();

                ui.label(RichText::new("Porta").size(12.0).color(Color32::GRAY));
                ui.add(egui::TextEdit::singleline(&mut self.cfg_port).desired_width(80.0));
                ui.end_row();

                ui.label(RichText::new("FPS").size(12.0).color(Color32::GRAY));
                ui.add(egui::TextEdit::singleline(&mut self.cfg_fps).desired_width(60.0));
                ui.end_row();

                ui.label(RichText::new("Qualidade JPEG (1-100)").size(12.0).color(Color32::GRAY));
                ui.add(egui::TextEdit::singleline(&mut self.cfg_quality).desired_width(60.0));
                ui.end_row();

                ui.label(RichText::new("Pasta compartilhada").size(12.0).color(Color32::GRAY));
                ui.add(egui::TextEdit::singleline(&mut self.cfg_folder).desired_width(280.0));
                ui.end_row();
            });

            ui.add_space(16.0);
            let btn = egui::Button::new(RichText::new("💾 Salvar").size(14.0))
                .fill(Color32::from_rgb(0, 180, 220))
                .min_size(Vec2::new(160.0, 34.0));
            if ui.add(btn).clicked() {
                self.config.password      = self.cfg_pass.clone();
                self.config.port          = self.cfg_port.parse().unwrap_or(7890);
                self.config.fps           = self.cfg_fps.parse().unwrap_or(15);
                self.config.jpeg_quality  = self.cfg_quality.parse::<u8>().unwrap_or(55).clamp(10, 95);
                self.config.shared_folder = self.cfg_folder.clone();
                self.config.save();
                self.cfg_saved = true;
            }
            if self.cfg_saved {
                ui.add_space(8.0);
                ui.label(RichText::new("✓ Salvo! Reinicie o app para aplicar mudança de porta.").size(12.0).color(Color32::from_rgb(34, 211, 165)));
            }

            ui.add_space(24.0);
            ui.separator();
            ui.add_space(12.0);
            ui.label(RichText::new(format!("Config salva em: {}/.remote-link.json",
                dirs::home_dir().unwrap_or_default().display())).size(10.0).color(Color32::DARK_GRAY));
        });
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────
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
    else if b < 1_073_741_824 { format!("{:.1} MB", b as f64 / 1_048_576.0) }
    else { format!("{:.2} GB", b as f64 / 1_073_741_824.0) }
}

fn rfd_pick_file() -> Option<String> {
    // rfd não está nas dependências pra manter self-contained
    // Em produção: adicionar rfd = "0.14" ao Cargo.toml
    // rfd::FileDialog::new().pick_file().map(|p| p.to_string_lossy().to_string())
    None
}

fn egui_key_to_str(key: egui::Key) -> Option<String> {
    Some(match key {
        egui::Key::Enter       => "enter",
        egui::Key::Backspace   => "backspace",
        egui::Key::Tab         => "tab",
        egui::Key::Escape      => "escape",
        egui::Key::Delete      => "delete",
        egui::Key::Home        => "home",
        egui::Key::End         => "end",
        egui::Key::PageUp      => "pageup",
        egui::Key::PageDown    => "pagedown",
        egui::Key::ArrowUp     => "up",
        egui::Key::ArrowDown   => "down",
        egui::Key::ArrowLeft   => "left",
        egui::Key::ArrowRight  => "right",
        egui::Key::F1  => "f1",  egui::Key::F2  => "f2",  egui::Key::F3  => "f3",
        egui::Key::F4  => "f4",  egui::Key::F5  => "f5",  egui::Key::F6  => "f6",
        egui::Key::F7  => "f7",  egui::Key::F8  => "f8",  egui::Key::F9  => "f9",
        egui::Key::F10 => "f10", egui::Key::F11 => "f11", egui::Key::F12 => "f12",
        _ => return None,
    }.to_string())
}

// ── Entry point ───────────────────────────────────────────────────────────
fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("remote_link=info")
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RemoteLink")
            .with_inner_size([480.0, 560.0])
            .with_min_inner_size([400.0, 400.0])
            .with_icon(load_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "RemoteLink",
        options,
        Box::new(|cc| Box::new(App::new(cc))),
    )
}

fn load_icon() -> egui::IconData {
    // Ícone embutido simples — 32x32 RGBA
    let size = 32usize;
    let mut pixels = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let i = (y * size + x) * 4;
            let cx = x as f32 - 16.0;
            let cy = y as f32 - 16.0;
            if (cx * cx + cy * cy).sqrt() < 14.0 {
                pixels[i]     = 0;    // R
                pixels[i + 1] = 180;  // G
                pixels[i + 2] = 220;  // B
                pixels[i + 3] = 255;  // A
            }
        }
    }
    egui::IconData { rgba: pixels, width: size as u32, height: size as u32 }
}
