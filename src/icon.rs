//! The app icon, compiled in as raw RGBA so there is no image decoder in the binary.
//! Regenerate `assets/icon.rgba` with the snippet in `assets/README.txt`.

use std::sync::Arc;

const SIDE: u32 = 64;
static RGBA: &[u8] = include_bytes!("../assets/icon.rgba");

pub fn window_icon() -> Arc<egui::IconData> {
    Arc::new(egui::IconData {
        rgba: RGBA.to_vec(),
        width: SIDE,
        height: SIDE,
    })
}

pub fn tray_icon() -> Option<tray_icon::Icon> {
    tray_icon::Icon::from_rgba(RGBA.to_vec(), SIDE, SIDE).ok()
}
