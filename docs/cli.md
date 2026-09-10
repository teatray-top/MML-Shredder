# 명령줄 사용법

배포 파일을 푼 폴더에서 PowerShell로 실행합니다. 실행 파일 이름에 공백이 있으므로 `&`와 따옴표를 사용합니다. 입력으로 `.mmi`, `.mid`, `.midi`를 받습니다.

```powershell
& '.\MML 세단기 CLI.exe' --help
& '.\MML 세단기 CLI.exe' fold --help
```

## MIDI 변환

성부를 줄이지 않고 MMI로 저장합니다.

```powershell
& '.\MML 세단기 CLI.exe' import '.\music.mid' -o '.\output\music.mmi'
```

MIDI Type 0·1의 PPQN·SMPTE 시간을 읽으며, 겹치는 음은 단선율 파트로 나눕니다. 템포는 정수 BPM에 맞춰 경과 시간을 보정하고, MML로 표현할 수 없는 짧은 시간 경계는 조정합니다. 피치 벤드·익스프레션·이펙트·음색 변경은 MML로 옮기지 않습니다.

## 편곡

```powershell
# 핵심 성부를 첫 트랙에 배치: 2트랙 × 3파트
& '.\MML 세단기 CLI.exe' fold '.\music.mmi' -o '.\output\full.mmi' --layout learned

# 1트랙 × 3파트, 원래 음량 유지
& '.\MML 세단기 CLI.exe' fold '.\music.mmi' -o '.\output\solo.mmi' --tracks 1 --parts 3 --layout hands --gain none

# 트랙별 MMI도 함께 저장
& '.\MML 세단기 CLI.exe' fold '.\music.mmi' -o '.\output\full.mmi' --track-files

# 양수 음량을 v11–v15로 압축
& '.\MML 세단기 CLI.exe' fold '.\music.mmi' -o '.\output\compressed.mmi' --gain none --volume-range 11:15
```

| 옵션 | 기본값 | 설명 |
|---|---|---|
| `-o PATH`, `--out PATH` | 필수 | 출력 MMI 파일 |
| `--tracks N` | `2` | 트랙 수, `1..16` |
| `--parts N` | `3` | 트랙당 파트 수, `1..3` |
| `--layout LAYOUT` | `hands` | 아래 배치 방식 중 선택 |
| `--gain auto` | `auto` | 가장 큰 음량이 `v15`가 되도록 일괄 이동 |
| `--gain none` | — | 원래 음량 유지 |
| `--gain N` | — | 정수를 더한 뒤 `v0..v15`로 제한. 예: `--gain -2` |
| `--volume-range MIN:MAX` | 꺼짐 | gain 적용 후 양수 음량 압축 |
| `--lead-pitch W` | `0.6` | 선율 추출의 음역 가중치 |
| `--lead-continuity W` | `0.7` | 선율 도약의 벌점 |
| `--track-files` | 꺼짐 | 전체 파일과 함께 `_t1.mmi`, `_t2.mmi` 등 저장 |
| `--report PATH` | 없음 | 개발 진단용 보고서 저장 |
| `--force` | 꺼짐 | 기존 출력 파일 덮어쓰기 |

배치 방식은 다음과 같습니다. CLI의 기본값은 `hands`이며 GUI의 기본값인 `learned`와 다릅니다.

| 값 | 배치 방식 |
|---|---|
| `hands` | 손별 음역을 바탕으로 트랙마다 오른손·중앙부·왼손 배치 |
| `voices` | 원본 성부와 음역을 기준으로 배치 |
| `roles` | 주선율·속성부·베이스 역할을 기준으로 배치 |
| `learned` | 학습 점수와 음악적 규칙으로 핵심 성부를 첫 트랙에 배치 |

동시발음 한도를 넘으면 일부 음을 생략하거나 이미 시작된 음의 끝을 줄입니다. `learned`는 오래 울리는 꼬리보다 새 공격을 우선할 수 있습니다. 자세한 선택 기준은 [핵심 성부 배치](core-model.md)에 있습니다.

### DR 압축

`--volume-range 11:15`는 gain 적용 후의 `v1..v15`를 `v11..v15`로 선형 변환하고 정수로 반올림합니다. `--gain none`과 함께 쓰면 `v1→v11`, `v8→v13`, `v15→v15`입니다.

범위는 `1≤최소≤최대≤15`입니다. `12:12`처럼 같게 지정하면 양수 음량을 모두 그 값으로 맞춥니다. gain과 `v0..v15` 제한을 먼저 적용하며, 그 단계에서 `v0`인 음은 압축 후에도 `v0`으로 남습니다. 옵션을 생략하면 gain만 적용합니다.

## 분할

```powershell
& '.\MML 세단기 CLI.exe' split '.\output\full.mmi' --outdir '.\output\scrolls' --limit 2400
```

| 옵션 | 기본값 | 설명 |
|---|---|---|
| `-d PATH`, `--outdir PATH` | 필수 | 출력 폴더 |
| `--limit N` | `2400` | 각 파트의 실제 MML 글자수 한도 |
| `--min-gap N` | `24` | 절단 후보로 인정할 최소 쉼표 길이, 틱 단위 |
| `--big-gap N` | `96` | 앞쪽 절단점을 선택할 만큼 긴 쉼표의 기준 |
| `--force` | 꺼짐 | 기존 출력 파일 덮어쓰기 |

모든 트랙·파트를 유지한 채 시간축의 같은 지점에서 자르고 장마다 MMI를 저장합니다. 각 장의 템포와 박자표는 시작점을 기준으로 다시 계산합니다. 합주용 악보는 전체 트랙을 한 번에 분할하세요.

글자수 한도 근처의 쉼표를 우선하며, 쉼표가 없으면 이어지는 음을 자를 수 있습니다. Python 호환 절단 방식은 경계에 생긴 6틱 미만의 음표 조각을 생략합니다. 이 방식으로 인코딩할 수 없는 입력은 조각을 보존하는 Rust 방식으로 다시 시도합니다. 최소 장수를 보장하지는 않습니다.

## 정보와 비교

```powershell
& '.\MML 세단기 CLI.exe' inspect '.\output\full.mmi'
& '.\MML 세단기 CLI.exe' verify '.\music.mmi' '.\output\full.mmi'
```

`inspect`는 트랙·파트·음표 수, 전체 틱, 최대 동시발음 수, 템포 이벤트 수를 표시합니다. `verify`는 두 악보의 음표·동시발음·템포 비교 결과를 출력합니다.

`import`, `fold`, `split`은 기존 파일을 기본적으로 덮어쓰지 않습니다. 갱신하려는 출력에만 `--force`를 사용하세요. 입력 악보 자체를 출력 경로로 지정할 수는 없습니다.
