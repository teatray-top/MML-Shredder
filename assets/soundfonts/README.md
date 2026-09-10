# MML 세단기 · 내장 음원

배포 실행 파일은 다음 두 SoundFont를 내장합니다. 별도 음원이나 VST 설치 없이
파트별로 17종의 악기를 선택해 모노로 미리 들을 수 있습니다. 음원 원본은
소스 저장소에서 추적하지 않으므로 소스 빌드에는 아래 로컬 파일이 필요합니다.

| 파일 | 용도 | 출처·라이선스 |
|---|---|---|
| `YDPGrand/YDP-GrandPiano-20160804.sf2` | 기본 피아노 | [YDP Grand Piano 안내](YDPGrand/README.md), CC BY 3.0 |
| `FluidR3/FluidR3_GM.sf2` | 나머지 16종 악기 | [FluidR3 GM 안내](FluidR3/README.md), MIT |

배포본에는 각각 `docs/piano`와 `docs/instruments`에 원본 설명과 라이선스를
동봉합니다. 미리듣기 악기 선택은 저장하는 MMI에 영향을 주지 않습니다.

## 이전 평가용 음원

`UprightPianoKW-small-20190703.sf2` is the unmodified **Upright piano KW (small)**
SoundFont, version **2019-07-03**, from the FreePats project.

- Project page: <https://freepats.zenvoid.org/Piano/acoustic-grand-piano.html>
- Download: <https://freepats.zenvoid.org/Piano/UprightPianoKW/UprightPianoKW-small-SF2-20190703.7z>
- Recorded by Gonzalo and Roberto in January 2017; edited and processed by Roberto.
- License: **CC0 1.0 Universal**. The original `cc0.txt` and `readme.txt` are included unchanged.
- SF2 size: **9,456,310 bytes**.
- SF2 SHA-256: `cf2a98eb38a32c4954b4b6e2caae4112d62dd8e892eceefdd7942b0e7d01ac2f`.

The bank contains one acoustic piano preset (bank 0, program 0), covering MIDI
keys 21–108 with 26 mono, 44.1 kHz samples. It is retained locally as an earlier
evaluation bank and is not embedded in the current release.
