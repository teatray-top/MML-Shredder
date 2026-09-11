# 개발

MML 세단기는 Rust로 작성한 Windows·브라우저 앱입니다. GUI는 `eframe/egui`, 오디오 출력은 `cpal`, SoundFont 재생은 `rustysynth`를 사용합니다. 프로젝트 코드는 [MIT 라이선스](../LICENSE)이며, 내장 글꼴과 음원에는 각각의 라이선스가 적용됩니다.

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

## 웹 빌드와 배포

브라우저에서도 같은 Rust 편곡·분할 코드를 사용합니다. 파일 읽기와 계산은 사용자의 브라우저에서 이루어지며, 서버는 정적 파일만 제공합니다. 별도 Web Worker에서 계산하므로 웹 빌드의 Rayon 병렬 처리는 사용하지 않습니다.

처음 한 번 웹 대상을 설치합니다. `wasm-bindgen-cli` 버전은 `Cargo.lock`의 `wasm-bindgen`과 맞춥니다. 현재 잠금 버전은 `0.2.128`입니다.

```powershell
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.128 --locked
```

저장소 루트에서 웹 실행 파일과 배포 폴더를 만듭니다. 위의 글꼴·모델이 빌드에 필요하며, 두 SoundFont는 실행 파일 밖에 두고 첫 미리듣기 때 내려받습니다.

```powershell
cargo rustc --release --locked --target wasm32-unknown-unknown --lib --crate-type cdylib
New-Item -ItemType Directory -Force dist/web/pkg, dist/web/audio | Out-Null
wasm-bindgen --target web --out-dir dist/web/pkg target/wasm32-unknown-unknown/release/mmlfold.wasm
Copy-Item web/* dist/web/
$webInputs = @('dist/web/pkg/mmlfold_bg.wasm', 'dist/web/pkg/mmlfold.js', 'web/app.js', 'web/worker.js', 'web/style.css', 'web/index.html')
$webHashes = @($webInputs | ForEach-Object { (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash.ToLowerInvariant() })
$webHasher = [Security.Cryptography.SHA256]::Create()
$webDigest = $webHasher.ComputeHash([Text.Encoding]::UTF8.GetBytes(($webHashes -join "`n")))
$webBuild = [BitConverter]::ToString($webDigest).Replace('-', '').Substring(0, 12).ToLowerInvariant()
$webHasher.Dispose()
$webIndex = (Get-Content web/index.html -Raw).Replace('?v=dev', "?v=$webBuild")
[IO.File]::WriteAllText((Join-Path (Get-Location) 'dist/web/index.html'), $webIndex, [Text.UTF8Encoding]::new($false))
Copy-Item assets/branding/app-icon.svg dist/web/icon.svg
Copy-Item assets/soundfonts/YDPGrand/YDP-GrandPiano-20160804.sf2 dist/web/audio/piano.sf2
Copy-Item assets/soundfonts/FluidR3/FluidR3_GM.sf2 dist/web/audio/instruments.sf2
```

라이선스 페이지에서 연결하는 원문도 같은 배포 폴더에 복사합니다.

```powershell
$webNotices = @(
    'LICENSE',
    'THIRD_PARTY.md',
    'licenses/Rust-dependencies.txt',
    'assets/fonts/OFL-Pretendard.txt',
    'assets/soundfonts/YDPGrand/CC-BY-3.0.txt',
    'assets/soundfonts/YDPGrand/YDP-GrandPiano-20160804.txt',
    'assets/soundfonts/FluidR3/LICENSE.txt',
    'assets/soundfonts/FluidR3/upstream-README.txt'
)
foreach ($notice in $webNotices) {
    $destination = Join-Path 'dist/web/docs/licenses' $notice
    New-Item -ItemType Directory -Force (Split-Path $destination) | Out-Null
    Copy-Item -LiteralPath $notice -Destination $destination
}
```

`dist/web/` 전체를 HTTPS 정적 사이트로 배포합니다. 서버에는 Rust·Node.js·Python 실행 환경이 필요하지 않습니다. Caddy의 `root`와 `file_server`를 사용하고, 하위 경로에 배포한다면 앱 주소 끝에 `/`를 유지합니다. 예를 들어 `/mml`은 `/mml/`로 이동시켜야 상대 경로의 작업자·음원이 올바르게 로드됩니다. `.wasm`은 `application/wasm`으로 제공해야 하며 Caddy는 이 형식을 기본 지원합니다.

재배포할 때는 새 폴더에 완성된 파일을 모두 올린 뒤 서비스 경로를 교체해 JS·WASM 버전이 섞이지 않게 합니다. 위 명령은 WASM·JS·CSS·HTML 원본의 해시를 합쳐 SHA256 앞 12자리를 배포 식별자로 넣으므로, 스크립트만 수정해도 값이 바뀝니다. 페이지에서 불러오는 앱·워커·JS·WASM이 같은 `?v=` 값을 사용하므로 CDN에 남아 있는 이전 스크립트를 피할 수 있습니다. HTML은 캐시를 재검증하도록 제공하고, 설정을 바꿨다면 Caddy 설정 검증 후 다시 불러옵니다. 배포 후 MIDI 열기, 편곡, 분할, MMI·ZIP 다운로드, 첫 음원 로딩과 재생을 확인합니다.

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
