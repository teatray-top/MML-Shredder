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
    parse_score_bytes(path, &bytes)
}

/// 파일명과 바이트로 MIDI 또는 MMI 악보를 읽습니다.
pub fn parse_score_bytes(path: &Path, bytes: &[u8]) -> Result<Score> {
    if is_midi_path(path) || bytes.starts_with(b"MThd") {
        midi::parse_midi(bytes)
            .with_context(|| format!("MIDI를 읽을 수 없습니다: {}", path.display()))
    } else {
        let text = std::str::from_utf8(bytes)
            .with_context(|| format!("MMI는 UTF-8 텍스트여야 합니다: {}", path.display()))?;
        core::parse_mmi(text).with_context(|| format!("MMI를 읽을 수 없습니다: {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 브라우저 바이트 입력에서 파일명과 MIDI 헤더 감지가 같은 악보를 만드는지 검사합니다.
    fn byte_input_preserves_midi_detection_and_mmi_metadata() {
        let bytes =
            b"MThd\0\0\0\x06\0\0\0\x01\0\x60MTrk\0\0\0\x0c\0\x90\x3c\x64\x60\x80\x3c\0\0\xff\x2f\0";
        let expected = midi::parse_midi(bytes).unwrap();
        for name in ["music.mid", "music.MIDI", "music.bin"] {
            let score = parse_score_bytes(Path::new(name), bytes).unwrap();
            assert_eq!(score.all_notes(), expected.all_notes());
            assert_eq!(core::serialize_mmi(&score), core::serialize_mmi(&expected));
        }

        let text = "[mml-score]\ntempo=0T120,192T80\nmml-track=MML@c1,e1,g1;\nname=연습곡\nvisible=true\n[time-signature]\n0=3/4\n";
        let expected = core::parse_mmi(text).unwrap();
        let score = parse_score_bytes(Path::new("연습곡.mmi"), text.as_bytes()).unwrap();
        assert_eq!(score.all_notes(), expected.all_notes());
        assert_eq!(score.tempos(), expected.tempos());
        assert_eq!(core::serialize_mmi(&score), core::serialize_mmi(&expected));
    }

    #[test]
    /// 잘못된 입력 형식의 오류에 사용자가 선택한 파일명을 보존하는지 검사합니다.
    fn byte_input_errors_identify_the_selected_file() {
        let midi_error = parse_score_bytes(Path::new("broken.mid"), b"not midi").unwrap_err();
        assert!(format!("{midi_error:#}").contains("broken.mid"));
        let text_error = parse_score_bytes(Path::new("broken.mmi"), &[0xff]).unwrap_err();
        assert!(format!("{text_error:#}").contains("MMI는 UTF-8"));
        assert!(format!("{text_error:#}").contains("broken.mmi"));
    }
}
