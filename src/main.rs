//! FST — point it at a file, get an HTTPS link, the file streams off this PC.
//!
//! No console in release builds; debug builds keep one so `eprintln!` is visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod icon;

use fst::{server, share, tunnel};

fn main() -> eframe::Result {
    let store = share::new_store();
    let log = server::new_log();

    let port = match server::spawn(store.clone(), log.clone()) {
        Ok(p) => p,
        Err(e) => {
            rfd::MessageDialog::new()
                .set_title("FST could not start")
                .set_description(format!("Could not open a local port: {e}"))
                .set_level(rfd::MessageLevel::Error)
                .show();
            std::process::exit(1);
        }
    };
    server::note(&log, format!("serving on 127.0.0.1:{port} (loopback only)"));

    let tunnel = tunnel::Tunnel::start(port);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("FST")
            .with_inner_size([620.0, 680.0])
            .with_min_inner_size([480.0, 420.0])
            .with_icon(icon::window_icon()),
        ..Default::default()
    };

    let tunnel_for_exit = tunnel.clone();
    let result = eframe::run_native(
        "FST",
        options,
        Box::new(move |cc| Ok(Box::new(app::Fst::new(cc, store, log, tunnel)))),
    );

    // Whatever happens, do not leave a tunnel pointing at this machine.
    tunnel_for_exit.shutdown();
    result
}
