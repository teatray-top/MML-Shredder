//! CLI의 DR 압축 옵션과 출력 음량을 검사합니다.

use mmlfold::{Score, core};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

struct TempDir(PathBuf);

impl TempDir {
    /// 테스트용 임시 폴더를 만듭니다.
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "mmlfold-volume-range-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    /// 테스트에서 만든 임시 폴더를 정리합니다.
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    dir: TempDir,
    source: PathBuf,
    text: String,
    score: Score,
}

impl Fixture {
    /// 지정한 음량의 테스트 MMI를 임시 폴더에 만듭니다.
    fn new(volumes: [i32; 4]) -> Self {
        let dir = TempDir::new();
        let source = dir.0.join("source.mmi");
        let text = format!(
            "[mml-score]\nversion=1\ntempo=0T120,192T144\n\
             mml-track=MML@t120o4l4v{}cv{}dt144v{}ev{}f;\n\
             name=Volume fixture\nprogram=0\nvisible=true\n\
             [time-signature]\n0=4/4\n",
            volumes[0], volumes[1], volumes[2], volumes[3]
        );
        fs::write(&source, &text).unwrap();
        let score = core::parse_mmi(&text).unwrap();
        Self {
            dir,
            source,
            text,
            score,
        }
    }

    /// CLI를 실행해 편곡 출력 파일을 읽습니다.
    fn fold(&self, name: &str, extra: &[&str]) -> Score {
        let out = self.dir.0.join(name);
        let result = Command::new(env!("CARGO_BIN_EXE_mmlfold"))
            .arg("fold")
            .arg(&self.source)
            .arg("--out")
            .arg(&out)
            .args(["--tracks", "1", "--parts", "1", "--layout", "voices"])
            .args(extra)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "args={extra:?}\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(fs::read_to_string(&self.source).unwrap(), self.text);
        core::load_mmi(&out).unwrap()
    }

    /// 음량 기대값과 음표 시각·템포 보존을 검사합니다.
    fn assert_volumes_and_timing(&self, output: &Score, expected: &[i32]) {
        let original = events(&self.score);
        let actual = events(output);
        assert_eq!(actual.len(), original.len());
        assert_eq!(
            actual.iter().map(|note| note.3).collect::<Vec<_>>(),
            expected
        );
        for (actual, original) in actual.iter().zip(&original) {
            assert_eq!(
                (actual.0, actual.1, actual.2),
                (original.0, original.1, original.2)
            );
        }
        assert_eq!(output.tempos(), self.score.tempos());
    }
}

/// 악보를 비교용 음표 목록으로 펼칩니다.
fn events(score: &Score) -> Vec<(i64, i64, i32, i32)> {
    let mut notes: Vec<_> = score
        .all_notes()
        .into_iter()
        .map(|note| (note.on, note.off, note.pitch, note.vel))
        .collect();
    notes.sort_unstable();
    notes
}

#[test]
/// CLI 도움말과 입력 로드 전 음량 범위 검증을 검사합니다.
fn cli_explains_volume_range_and_rejects_invalid_values_before_loading_input() {
    let help = Command::new(env!("CARGO_BIN_EXE_mmlfold"))
        .args(["fold", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("--volume-range <MIN:MAX>"));
    assert!(help.contains("--gain"));

    let dir = TempDir::new();
    let missing = dir.0.join("missing.mmi");
    let out = dir.0.join("output.mmi");
    for value in [
        "11",
        "11:",
        ":15",
        "1:2:3",
        "loud:15",
        "0:15",
        "1:16",
        "15:11",
        "-1:15",
        "99999999999999999999:15",
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_mmlfold"))
            .arg("fold")
            .arg(&missing)
            .arg("--out")
            .arg(&out)
            .args(["--volume-range", value])
            .output()
            .unwrap();
        let error = String::from_utf8_lossy(&result.stderr);
        assert_eq!(result.status.code(), Some(2), "{value}: {error}");
        assert!(error.contains("--volume-range"), "{value}: {error}");
        assert!(error.contains("1..15"), "{value}: {error}");
        assert!(!out.exists());
    }
}

#[test]
/// 압축 생략과 동일 범위 지정의 원래 음량 보존을 검사합니다.
fn omitted_compression_and_identity_range_preserve_original_volumes() {
    let fixture = Fixture::new([0, 1, 8, 15]);
    let default = fixture.fold("default.mmi", &["--gain", "none"]);
    let identity = fixture.fold(
        "identity.mmi",
        &["--gain", "none", "--volume-range", "1:15"],
    );
    fixture.assert_volumes_and_timing(&default, &[0, 1, 8, 15]);
    assert_eq!(
        core::serialize_mmi(&default),
        core::serialize_mmi(&identity)
    );
}

#[test]
/// CLI의 지정 범위·단일 음량 압축을 검사합니다.
fn cli_compresses_to_requested_range_and_accepts_a_constant_range() {
    let fixture = Fixture::new([0, 1, 8, 15]);
    for (name, range, expected) in [
        ("compressed.mmi", "11:15", [0, 11, 13, 15]),
        ("constant.mmi", "12:12", [0, 12, 12, 12]),
    ] {
        let result = fixture.fold(name, &["--gain", "none", "--volume-range", range]);
        fixture.assert_volumes_and_timing(&result, &expected);
    }
}

#[test]
/// gain 제한 후 음량 0만 그대로 유지하는지 검사합니다.
fn compression_follows_gain_clipping_and_preserves_only_stage_zero() {
    let fixture = Fixture::new([0, 1, 8, 15]);
    for (name, gain, expected) in [
        ("quieter.mmi", "-2", [0, 0, 12, 14]),
        ("louder.mmi", "2", [11, 12, 14, 15]),
    ] {
        let result = fixture.fold(name, &["--gain", gain, "--volume-range", "11:15"]);
        fixture.assert_volumes_and_timing(&result, &expected);
    }
    // gain이 v0을 양수로 올렸다면 압축 단계에서 다시 무음으로 만들지 않습니다.
    let automatic = Fixture::new([0, 1, 4, 8]);
    let result = automatic.fold("auto.mmi", &["--volume-range", "11:15"]);
    automatic.assert_volumes_and_timing(&result, &[13, 13, 14, 15]);
}
