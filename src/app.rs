//! The window and the tray icon.
//!
//! Tray menu clicks arrive on a global channel which a small thread drains into
//! `actions`, then wakes the egui loop. `App::logic` is called even while the
//! window is hidden, which is what lets tray Exit work when nothing is on screen.

use eframe::egui::{self, Color32, RichText};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};
use tray_icon::{
    TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
};

use crate::icon;
use fst::{
    server::{self, Log},
    share::{self, Share, Status, Store, human_left, human_size},
    tunnel::{self, Tunnel},
};

const BLUE: Color32 = Color32::from_rgb(47, 129, 247);
const DIM: Color32 = Color32::from_rgb(139, 148, 158);
const GOOD: Color32 = Color32::from_rgb(63, 185, 80);
const WARN: Color32 = Color32::from_rgb(210, 153, 34);
const BAD: Color32 = Color32::from_rgb(248, 81, 73);

// ------------------------------------------------------------------- expiry

#[derive(Clone, Copy, PartialEq, Eq)]
enum Expiry {
    H1,
    H6,
    H24,
    D7,
    Never,
}

impl Expiry {
    const ALL: [Self; 5] = [Self::H1, Self::H6, Self::H24, Self::D7, Self::Never];

    fn label(self) -> &'static str {
        match self {
            Self::H1 => "1 hour",
            Self::H6 => "6 hours",
            Self::H24 => "24 hours",
            Self::D7 => "7 days",
            Self::Never => "Never",
        }
    }

    fn duration(self) -> Option<Duration> {
        match self {
            Self::H1 => Some(Duration::from_secs(3600)),
            Self::H6 => Some(Duration::from_secs(6 * 3600)),
            Self::H24 => Some(Duration::from_secs(24 * 3600)),
            Self::D7 => Some(Duration::from_secs(7 * 86_400)),
            Self::Never => None,
        }
    }
}

// --------------------------------------------------------------------- app

enum TrayAction {
    Show,
    Exit,
}

pub struct Fst {
    store: Store,
    log: Log,
    tunnel: Arc<Tunnel>,

    // Held so the tray icon stays alive for the life of the app.
    _tray: Option<TrayIcon>,
    actions: Arc<Mutex<VecDeque<TrayAction>>>,
    exiting: Arc<AtomicBool>,
    hidden: bool,
    tooltip_shown: usize,

    // form
    picked: Option<PathBuf>,
    expiry: Expiry,
    cap_on: bool,
    cap: u32,

    // ui state
    status: Option<(String, bool)>,
    qr_for: Option<String>,
    qr_tex: Option<(String, egui::TextureHandle)>,
    last_epoch: u64,
    stale: bool,
}

impl Fst {
    pub fn new(cc: &eframe::CreationContext<'_>, store: Store, log: Log, tunnel: Arc<Tunnel>) -> Self {
        let actions: Arc<Mutex<VecDeque<TrayAction>>> = Arc::new(Mutex::new(VecDeque::new()));
        let exiting = Arc::new(AtomicBool::new(false));

        // The tray icon must be built on the thread that owns the event loop.
        let show = MenuItem::new("Show FST", true, None);
        let quit = MenuItem::new("Exit", true, None);
        let show_id = show.id().clone();
        let quit_id = quit.id().clone();

        let menu = Menu::new();
        let _ = menu.append_items(&[&show, &PredefinedMenuItem::separator(), &quit]);

        let tray = icon::tray_icon().and_then(|ico| {
            TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_icon(ico)
                .with_tooltip("FST")
                .with_menu_on_left_click(false)
                .build()
                .ok()
        });

        spawn_tray_pump(cc.egui_ctx.clone(), actions.clone(), show_id, quit_id);

        Self {
            store,
            log,
            tunnel,
            _tray: tray,
            actions,
            exiting,
            hidden: false,
            tooltip_shown: usize::MAX,
            picked: None,
            expiry: Expiry::H24,
            cap_on: false,
            cap: 1,
            status: None,
            qr_for: None,
            qr_tex: None,
            last_epoch: 0,
            stale: false,
        }
    }

    fn hide(&mut self, ctx: &egui::Context) {
        self.hidden = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        trim_working_set();
    }

    fn show(&mut self, ctx: &egui::Context) {
        self.hidden = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    fn live_count(&self) -> usize {
        self.store
            .read()
            .unwrap()
            .values()
            .filter(|s| s.is_live())
            .count()
    }

    fn browse(&mut self) {
        if let Some(p) = rfd::FileDialog::new().set_title("Pick a file to send").pick_file() {
            self.picked = Some(p);
            self.status = None;
        }
    }

    fn copy(&mut self, text: &str) {
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_owned())) {
            Ok(()) => self.status = Some(("Link copied to clipboard".into(), false)),
            Err(e) => self.status = Some((format!("Could not copy: {e}"), true)),
        }
    }

    fn create(&mut self) {
        let Some(picked) = self.picked.clone() else { return };

        let meta = match std::fs::metadata(&picked) {
            Ok(m) => m,
            Err(e) => {
                self.status = Some((format!("Cannot read that file: {e}"), true));
                return;
            }
        };
        if !meta.is_file() {
            self.status = Some(("That is a folder. Folders aren't supported yet.".into(), true));
            return;
        }

        // Resolve once, here. This is the only path the server will ever open.
        let path = std::fs::canonicalize(&picked).unwrap_or(picked);
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "download".to_string());

        let token = share::new_token();
        let now = SystemTime::now();
        let entry = Share {
            token: token.clone(),
            path,
            name: name.clone(),
            size: meta.len(),
            created: now,
            expires: self.expiry.duration().map(|d| now + d),
            max_downloads: if self.cap_on { Some(self.cap.max(1)) } else { None },
            downloads: 0,
            revoked: false,
        };

        self.store.write().unwrap().insert(token.clone(), entry);
        server::note(
            &self.log,
            format!("shared {name} ({})", human_size(meta.len())),
        );

        if let Some(base) = self.tunnel.base() {
            let link = format!("{base}/d/{token}");
            self.copy(&link);
        } else {
            self.status = Some(("Shared. Waiting for the tunnel to come up.".into(), false));
        }

        self.picked = None;
        self.qr_for = None;
    }

    fn link_for(&self, token: &str) -> Option<String> {
        self.tunnel.base().map(|b| format!("{b}/d/{token}"))
    }
}

fn spawn_tray_pump(
    ctx: egui::Context,
    actions: Arc<Mutex<VecDeque<TrayAction>>>,
    show_id: MenuId,
    quit_id: MenuId,
) {
    std::thread::Builder::new()
        .name("fst-tray".into())
        .spawn(move || {
            let menu_rx = MenuEvent::receiver();
            let tray_rx = TrayIconEvent::receiver();
            loop {
                let mut woke = false;

                while let Ok(ev) = menu_rx.try_recv() {
                    if ev.id == show_id {
                        actions.lock().unwrap().push_back(TrayAction::Show);
                        woke = true;
                    } else if ev.id == quit_id {
                        actions.lock().unwrap().push_back(TrayAction::Exit);
                        woke = true;
                    }
                }

                while let Ok(ev) = tray_rx.try_recv() {
                    if let TrayIconEvent::Click {
                        button: tray_icon::MouseButton::Left,
                        button_state: tray_icon::MouseButtonState::Up,
                        ..
                    } = ev
                    {
                        actions.lock().unwrap().push_back(TrayAction::Show);
                        woke = true;
                    }
                }

                if woke {
                    // Wakes `App::logic`, which runs even with the window hidden.
                    ctx.request_repaint();
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
        .expect("spawn tray pump");
}

// --------------------------------------------------------------- eframe impl

impl eframe::App for Fst {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Tray actions first: Exit has to work while hidden.
        loop {
            let next = self.actions.lock().unwrap().pop_front();
            match next {
                Some(TrayAction::Show) => self.show(ctx),
                Some(TrayAction::Exit) => {
                    self.exiting.store(true, Ordering::SeqCst);
                    self.tunnel.shutdown();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                None => break,
            }
        }

        // The window's X button hides to tray; only tray Exit really quits.
        if ctx.input(|i| i.viewport().close_requested()) && !self.exiting.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide(ctx);
        }

        if !self.hidden {
            // Minimising also means "get out of the way but keep serving".
            if ctx.input(|i| i.viewport().minimized == Some(true)) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                self.hide(ctx);
            }

            let dropped: Vec<PathBuf> = ctx.input(|i| {
                i.raw
                    .dropped_files
                    .iter()
                    .map(|f| f.path().to_path_buf())
                    .collect()
            });
            if let Some(p) = dropped.into_iter().find(|p| p.is_file()) {
                self.picked = Some(p);
                self.status = None;
            }

            // Keep countdowns and transfer counts moving.
            ctx.request_repaint_after(Duration::from_secs(1));
        }

        // A new tunnel hostname invalidates every link handed out under the old one.
        let epoch = self.tunnel.epoch();
        if epoch != 0 {
            if self.last_epoch != 0 && epoch != self.last_epoch {
                self.stale = true;
            }
            self.last_epoch = epoch;
        }

        let live = self.live_count();
        if live != self.tooltip_shown {
            self.tooltip_shown = live;
            if let Some(t) = self._tray.as_ref() {
                let tip = match live {
                    0 => "FST — no active links".to_string(),
                    1 => "FST — 1 active link".to_string(),
                    n => format!("FST — {n} active links"),
                };
                let _ = t.set_tooltip(Some(tip));
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Frame::new()
            .inner_margin(egui::Margin::same(16))
            .show(ui, |ui| {
                self.header(ui);
                ui.add_space(14.0);
                self.picker(ui);
                ui.add_space(12.0);
                self.options(ui);
                ui.add_space(12.0);
                self.shares(ui);
                ui.add_space(8.0);
                self.activity(ui);
            });
    }
}

// ------------------------------------------------------------------ ui parts

impl Fst {
    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("FST").size(22.0).strong().color(BLUE));
            ui.label(RichText::new("send a file straight off this PC").color(DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (text, colour) = match self.tunnel.state() {
                    tunnel::State::Up(_) => ("● online", GOOD),
                    tunnel::State::Starting => ("● connecting", WARN),
                    tunnel::State::Restarting => ("● reconnecting", WARN),
                    tunnel::State::Failed(_) => ("● offline", BAD),
                };
                ui.label(RichText::new(text).color(colour).size(13.0));
            });
        });

        if let tunnel::State::Failed(e) = self.tunnel.state() {
            ui.add_space(6.0);
            ui.label(RichText::new(e).color(BAD).size(12.0));
        }

        if self.stale {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(
                        "The tunnel restarted, so links copied earlier no longer work. Re-copy them.",
                    )
                    .color(WARN)
                    .size(12.0),
                );
                if ui.small_button("Dismiss").clicked() {
                    self.stale = false;
                }
            });
        }
    }

    fn picker(&mut self, ui: &mut egui::Ui) {
        let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        let mut frame = egui::Frame::group(ui.style());
        if hovering {
            frame = frame.stroke(egui::Stroke::new(1.5, BLUE));
        }
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.vertical_centered(|ui| {
                ui.add_space(6.0);
                match self.picked.clone() {
                    Some(p) => {
                        let name = p
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                        ui.label(RichText::new(name).strong());
                        ui.label(RichText::new(human_size(size)).color(DIM).size(12.0));
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::top_down(egui::Align::Center),
                                |ui| {
                                    if ui.button("Pick a different file").clicked() {
                                        self.browse();
                                    }
                                },
                            );
                        });
                    }
                    None => {
                        ui.label(
                            RichText::new(if hovering {
                                "Drop it anywhere"
                            } else {
                                "Drop a file anywhere on this window"
                            })
                            .color(DIM),
                        );
                        ui.add_space(6.0);
                        if ui.button("Browse…").clicked() {
                            self.browse();
                        }
                    }
                }
                ui.add_space(6.0);
            });
        });
    }

    fn options(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Expires:");
            egui::ComboBox::from_id_salt("expiry")
                .selected_text(self.expiry.label())
                .show_ui(ui, |ui| {
                    for e in Expiry::ALL {
                        ui.selectable_value(&mut self.expiry, e, e.label());
                    }
                });

            ui.add_space(12.0);
            ui.checkbox(&mut self.cap_on, "Limit downloads");
            if self.cap_on {
                ui.add(egui::DragValue::new(&mut self.cap).range(1..=99).speed(0.2));
            }
        });

        if self.cap_on {
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "A resumed or retried transfer spends one of these, so leave room.",
                )
                .color(DIM)
                .size(11.0),
            );
        }

        ui.add_space(10.0);
        let ready = self.picked.is_some();
        if ui
            .add_enabled(
                ready,
                egui::Button::new(RichText::new("Create link").strong())
                    .fill(if ready { BLUE } else { Color32::DARK_GRAY })
                    .min_size(egui::vec2(ui.available_width(), 34.0)),
            )
            .clicked()
        {
            self.create();
        }

        if let Some((msg, is_err)) = self.status.clone() {
            ui.add_space(6.0);
            ui.label(
                RichText::new(msg)
                    .color(if is_err { BAD } else { GOOD })
                    .size(12.0),
            );
        }
    }

    fn shares(&mut self, ui: &mut egui::Ui) {
        let mut rows: Vec<Share> = self.store.read().unwrap().values().cloned().collect();
        if rows.is_empty() {
            return;
        }
        rows.sort_by_key(|s| std::cmp::Reverse(s.created));

        ui.separator();
        ui.label(RichText::new(format!("Links ({})", rows.len())).strong());
        ui.add_space(4.0);

        let mut revoke: Option<String> = None;
        let mut forget: Option<String> = None;
        let mut to_copy: Option<String> = None;
        let mut to_open: Option<String> = None;

        egui::ScrollArea::vertical()
            .id_salt("shares")
            .max_height(190.0)
            .show(ui, |ui| {
                for s in &rows {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&s.name).strong());
                            ui.label(RichText::new(human_size(s.size)).color(DIM).size(11.0));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let status = s.status();
                                    let colour = if status == Status::Live { GOOD } else { DIM };
                                    ui.label(
                                        RichText::new(status.label()).color(colour).size(11.0),
                                    );
                                },
                            );
                        });

                        let detail = {
                            let mut bits = Vec::new();
                            if let Some(d) = s.time_left() {
                                bits.push(format!("{} left", human_left(d)));
                            }
                            bits.push(match s.max_downloads {
                                Some(m) => format!("{}/{} downloads", s.downloads, m),
                                None => format!("{} downloads", s.downloads),
                            });
                            bits.join(" · ")
                        };
                        ui.label(RichText::new(detail).color(DIM).size(11.0));

                        if s.is_live() {
                            match self.link_for(&s.token) {
                                Some(link) => {
                                    ui.add_space(2.0);
                                    ui.add(
                                        egui::TextEdit::singleline(&mut link.clone())
                                            .font(egui::TextStyle::Monospace)
                                            .desired_width(ui.available_width()),
                                    );
                                    ui.horizontal(|ui| {
                                        if ui.button("Copy").clicked() {
                                            to_copy = Some(link.clone());
                                        }
                                        if ui.button("Open").clicked() {
                                            to_open = Some(link.clone());
                                        }
                                        let qr_on = self.qr_for.as_deref() == Some(&s.token);
                                        if ui.selectable_label(qr_on, "QR").clicked() {
                                            self.qr_for =
                                                if qr_on { None } else { Some(s.token.clone()) };
                                        }
                                        if ui.button("Revoke").clicked() {
                                            revoke = Some(s.token.clone());
                                        }
                                    });

                                    if self.qr_for.as_deref() == Some(&s.token) {
                                        self.draw_qr(ui, &s.token, &link);
                                    }
                                }
                                None => {
                                    ui.label(
                                        RichText::new("waiting for the tunnel…")
                                            .color(WARN)
                                            .size(11.0),
                                    );
                                }
                            }
                        } else if ui.button("Remove").clicked() {
                            forget = Some(s.token.clone());
                        }
                    });
                }
            });

        if let Some(t) = revoke {
            if let Some(s) = self.store.write().unwrap().get_mut(&t) {
                s.revoked = true;
                server::note(&self.log, format!("revoked {}", s.name));
            }
        }
        if let Some(t) = forget {
            self.store.write().unwrap().remove(&t);
        }
        if let Some(link) = to_copy {
            self.copy(&link);
        }
        if let Some(link) = to_open {
            let _ = open::that_detached(&link);
        }
    }

    fn draw_qr(&mut self, ui: &mut egui::Ui, token: &str, link: &str) {
        let stale = match &self.qr_tex {
            Some((k, _)) => k != link,
            None => true,
        };
        if stale {
            if let Some(img) = qr_image(link) {
                let tex = ui.ctx().load_texture(
                    format!("qr-{token}"),
                    img,
                    egui::TextureOptions::NEAREST,
                );
                self.qr_tex = Some((link.to_string(), tex));
            }
        }
        if let Some((_, tex)) = &self.qr_tex {
            ui.add_space(6.0);
            ui.vertical_centered(|ui| {
                ui.image(egui::load::SizedTexture::new(tex.id(), tex.size_vec2()));
            });
        }
    }

    fn activity(&mut self, ui: &mut egui::Ui) {
        let lines = self.log.read().unwrap().clone();
        if lines.is_empty() {
            return;
        }
        ui.separator();
        ui.label(RichText::new("Activity").strong());
        egui::ScrollArea::vertical()
            .id_salt("activity")
            .max_height(120.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for l in &lines {
                    ui.label(
                        RichText::new(format!("{}  {}", l.when.format("%H:%M:%S"), l.text))
                            .color(DIM)
                            .size(11.0)
                            .monospace(),
                    );
                }
            });
    }
}

/// Hand the working set back to Windows when we drop to the tray.
///
/// The GPU backend keeps a large resident footprint that is pure dead weight
/// while nothing is on screen, and the tray is where this app spends nearly all
/// of its life. Passing `-1` for both bounds asks Windows to trim the pages to
/// the standby list; they fault back in when the window is shown again.
#[cfg(windows)]
fn trim_working_set() {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
    // SAFETY: a pseudo-handle to our own process plus two documented sentinels.
    unsafe {
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

#[cfg(not(windows))]
fn trim_working_set() {}

/// Render a QR for `data` as an egui image, with the quiet zone QR scanners need.
fn qr_image(data: &str) -> Option<egui::ColorImage> {
    let code = qrcode::QrCode::new(data.as_bytes()).ok()?;
    let modules = code.to_colors();
    let w = (modules.len() as f64).sqrt().round() as usize;
    if w == 0 || w * w != modules.len() {
        return None;
    }

    const QUIET: usize = 4;
    const SCALE: usize = 4;
    let side = (w + QUIET * 2) * SCALE;

    let mut rgba = vec![255u8; side * side * 4];
    for y in 0..w {
        for x in 0..w {
            if modules[y * w + x] != qrcode::Color::Dark {
                continue;
            }
            for dy in 0..SCALE {
                for dx in 0..SCALE {
                    let py = (y + QUIET) * SCALE + dy;
                    let px = (x + QUIET) * SCALE + dx;
                    let i = (py * side + px) * 4;
                    rgba[i] = 0;
                    rgba[i + 1] = 0;
                    rgba[i + 2] = 0;
                }
            }
        }
    }

    Some(egui::ColorImage::from_rgba_unmultiplied([side, side], &rgba))
}
