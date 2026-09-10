//! GUI와 CLI에서 MIDI와 MMI 파일을 공통으로 읽습니다.
use std::path::Path;

use anyhow::{Context, Result};

use crate::{Score, core, midi};

/// 파일 확장자가 MIDI 입력인지 확인합니다.
pub fn is_midi_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("mid") || extension.eq_ignore_ascii_case("midi")
        })
}

/// 확장자에 맞는 파서로 악보 파일을 읽습니다.
pub fn load_score(path: &Path) -> Result<Score> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("악보를 읽을 수 없습니다: {}", path.display()))?;
    if is_midi_path(path) || bytes.starts_with(b"MThd") {
        midi::parse_midi(&bytes)
            .with_context(|| format!("MIDI를 읽을 수 없습니다: {}", path.display()))
    } else {
        let text = std::str::from_utf8(&bytes)
            .with_context(|| format!("MMI는 UTF-8 텍스트여야 합니다: {}", path.display()))?;
        core::parse_mmi(text).with_context(|| format!("MMI를 읽을 수 없습니다: {}", path.display()))
    }
}
