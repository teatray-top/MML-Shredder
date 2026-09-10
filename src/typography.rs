//! 한글 UI 글꼴과 픽셀 정렬을 설정한다.

use std::{path::PathBuf, sync::Arc};

use eframe::egui;

/// 내장 한글 글꼴과 글리프·벡터의 픽셀 정렬을 설정한다.
pub fn configure(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let font = std::env::var_os("MMLFOLD_FONT")
        .map(PathBuf::from)
        .and_then(|path| std::fs::read(path).ok())
        .map(egui::FontData::from_owned)
        .unwrap_or_else(|| {
            egui::FontData::from_static(include_bytes!("../assets/fonts/Pretendard-Regular.ttf"))
        });

    let name = "korean-ui".to_owned();
    fonts.font_data.insert(name.clone(), Arc::new(font));
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, name.clone());
    // 고정폭 표시에서도 한글 대체 글꼴을 유지한다.
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push(name);
    ctx.set_fonts(fonts);

    ctx.tessellation_options_mut(|options| {
        // 글리프 AA는 egui가 처리하며, 픽셀 정렬은 글꼴 아틀라스의 재보간을 막는다.
        options.round_text_to_pixels = true;
        // feathering은 글리프가 아닌 벡터 컨트롤의 1픽셀 AA를 유지한다.
        options.feathering = true;
        options.feathering_size_in_pixels = 1.0;
    });
}
