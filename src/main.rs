//! 네이티브 창과 초기 파일 경로를 준비해 앱을 시작한다.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod gui;
mod typography;

/// 초기 파일 인자와 네이티브 창을 준비해 앱을 실행한다.
fn main() -> eframe::Result {
    let initial = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 900.0])
            .with_min_inner_size([1024.0, 760.0])
            .with_icon(std::sync::Arc::new(window_icon()))
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "MML 세단기",
        options,
        Box::new(move |cc| Ok(Box::new(gui::MmlApp::new(cc, initial)))),
    )
}

/// 빌드 시 생성한 256픽셀 RGBA를 창 아이콘으로 반환한다.
fn window_icon() -> eframe::egui::IconData {
    let rgba: &[u8; 256 * 256 * 4] = include_bytes!(concat!(env!("OUT_DIR"), "/app-icon.rgba"));
    eframe::egui::IconData {
        rgba: rgba.to_vec(),
        width: 256,
        height: 256,
    }
}
