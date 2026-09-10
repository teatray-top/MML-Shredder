# 개발

MML 세단기는 Rust로 작성한 Windows 데스크톱 앱입니다. GUI는 `eframe/egui`, 오디오 출력은 `cpal`, SoundFont 재생은 `rustysynth`를 사용합니다. 프로젝트 코드는 [MIT 라이선스](../LICENSE)이며, 내장 글꼴과 음원에는 각각의 라이선스가 적용됩니다.

## 빌드 준비

Rust 툴체인과 Windows용 C++ 빌드 도구가 필요합니다. 소스 저장소에는 실행 파일, 빌드 캐시, 사용자 악보, 음원·폰트 원본과 학습 가중치를 포함하지 않습니다. 새 체크아웃에는 다음 파일을 별도로 준비해야 합니다.

| 로컬 파일 | 용도 |
|---|---|
| `assets/fonts/Pretendard-Regular.ttf` | 내장 한국어 글꼴 |
| `assets/soundfonts/YDPGrand/YDP-GrandPiano-20160804.sf2` | 기본 피아노 |
| `assets/soundfonts/FluidR3/FluidR3_GM.sf2` | 나머지 미리듣기 악기 |
| `assets/melody/core-v2.bin` | 핵심 성부 추론 가중치 |
| `assets/melody/core-v2.json` | 모델 테스트용 메타데이터 |

글꼴·음원의 출처와 해시는 [글꼴 안내](../assets/fonts/README.md), [음원 안내](../assets/soundfonts/README.md)에 있습니다. 모델 학습 도구와 원본 악보·정답 자료는 저장소에 포함하지 않습니다.

저장소 루트에서 실행합니다.

```powershell
cargo build --release --locked --bins
.\target\release\mmlfold-gui.exe
.\target\release\mmlfold-gui.exe ".\music.mmi"
```

Cargo 대상 이름은 `mmlfold-gui`와 `mmlfold`입니다. 배포할 때 GUI는 `MML 세단기.exe`, CLI는 `MML 세단기 CLI.exe`로 이름을 바꿉니다. 배포 실행 파일에는 음원·글꼴·학습 모델이 내장되므로 사용자가 별도로 설치할 필요가 없습니다. 자원 출처와 라이선스도 배포본에 동봉합니다.

개발 중에는 바로 실행할 수 있습니다.

```powershell
cargo run --locked --bin mmlfold-gui
cargo run --locked --bin mmlfold -- inspect ".\music.mmi"
```

`build.rs`는 `assets/branding/app-icon.svg`에서 창 아이콘과 Windows 실행 파일 리소스를 생성합니다. 글꼴을 바꿔 확인하려면 `MMLFOLD_FONT`에 로컬 글꼴 경로를 지정합니다.

## 검사

```powershell
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

전체 테스트에는 위 내장 자원 외에 루트의 `Op39No11_full_6part.mmi`, `Op39No11_solo_3part.mmi`, `Op39No11_second_3part.mmi`와 `scrolls_ensemble/`, `scrolls_solo/`, `scrolls_second/` 자료가 필요합니다. 이 악보들은 저장소에서 제외되어 있습니다. 일부 테스트는 `include_str!`로 악보를 읽으므로, 자료가 없는 체크아웃에서는 전체 테스트와 `--all-targets` 검사를 컴파일할 수 없습니다.

회귀 테스트는 MML 파싱과 재출력, 템포·음가, MIDI 시간과 페달 처리, 편곡의 동시발음 제한, 분할 글자수와 파일 저장을 확인합니다. 과거 Python 대조 결과는 [비교 기록](python-rust-comparison.md)에 보관합니다. 현재 편곡의 파트별 문자열이 Python과 완전히 같다는 뜻은 아닙니다.

빌드 산출물은 `target/`, 로컬 검사 결과와 화면 캡처는 `test-output/`에 둡니다. 성능 측정은 별도로 실행합니다.

```powershell
cargo test --release --locked --lib performance_tests::benchmark_pipeline -- --ignored --nocapture
```

기본 입력은 `Op39No11_full_6part.mmi`입니다. 다른 악보를 쓰려면 환경 변수 `MMLFOLD_BENCH_INPUT`에 경로를 지정합니다.

## 모델

앱은 미리 학습한 가중치를 Rust에서 추론합니다. 정답 구성, 학습 방법과 모델 적용 범위는 [핵심 성부 학습과 배치](core-model.md)에 기록되어 있습니다.

## 코드 구성

| 경로 | 역할 |
|---|---|
| `src/main.rs`, `src/gui.rs` | 창과 사용자 조작 |
| `src/bin/cli.rs` | [CLI 명령](cli.md) |
| `src/input.rs`, `src/midi.rs` | MMI·MIDI 입력과 변환 |
| `src/core.rs`, `src/lib.rs` | 악보 자료형과 MML 인코딩 |
| `src/fold.rs`, `src/core_selection.rs`, `src/attack_selection.rs` | 동시발음 축소와 성부 배치 |
| `src/salience.rs` | 내장 신경망 추론 |
| `src/split.rs`, `src/workflow.rs` | 시간축 분할과 파일 저장 |
| `src/synth.rs`, `src/playback.rs` | 재생 시계와 오디오 출력 |
| `src/piano.rs`, `src/instruments.rs` | 내장 음원 재생 |
| `src/parallel.rs` | 독립 작업 병렬 처리 |
| `tests/` | 회귀·통합 테스트 |

`Timeline`의 음표·재생 길이·전체 틱은 `notes()`, `duration_seconds()`, `total_ticks()`로 읽습니다. 재생 데이터를 바꿀 때는 악보를 수정한 뒤 `Timeline::from_score`로 다시 생성합니다. 내부 도우미는 비공개 또는 `pub(crate)`이며, GUI·CLI가 사용하는 자료형은 공개합니다.

16,384음 이상인 작업에서는 독립적인 신경망 입력 행과 각 분할 후보 안의 파트 인코딩을 Rayon으로 병렬 처리합니다. 전용 풀은 최대 8개 스레드를 사용하고, 호출자의 Rayon 풀이 있으면 재사용합니다. 작은 작업과 스레드 생성 실패 시에는 순차 실행합니다. 특징 추출, 음 배치, 최소 비용 흐름, 분할 후보 선택의 순서는 유지합니다.
