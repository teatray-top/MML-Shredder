//! 미리듣기 악기 목록과 파트별 FluidR3 신시사이저를 제공한다.

use std::{
    io::Cursor,
    sync::{Arc, OnceLock},
};

use anyhow::Result;
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};

/// 악보의 MML 악기 메타데이터와 독립적인 미리듣기 음색이다.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Instrument {
    #[default]
    Piano,
    ElectricPiano,
    Harpsichord,
    Organ,
    Guitar,
    Bass,
    Strings,
    Violin,
    Cello,
    Flute,
    Clarinet,
    Trumpet,
    Xylophone,
    Harmonica,
    Harp,
    Glockenspiel,
    Tuba,
}

impl Instrument {
    pub const ALL: [Self; 17] = [
        Self::Piano,
        Self::ElectricPiano,
        Self::Harpsichord,
        Self::Organ,
        Self::Guitar,
        Self::Bass,
        Self::Strings,
        Self::Violin,
        Self::Cello,
        Self::Flute,
        Self::Clarinet,
        Self::Trumpet,
        Self::Xylophone,
        Self::Harmonica,
        Self::Harp,
        Self::Glockenspiel,
        Self::Tuba,
    ];

    /// 화면에 표시할 악기 이름을 반환한다.
    pub fn name(self) -> &'static str {
        match self {
            Self::Piano => "피아노",
            Self::ElectricPiano => "일렉트릭 피아노",
            Self::Harpsichord => "하프시코드",
            Self::Organ => "오르간",
            Self::Guitar => "기타",
            Self::Bass => "베이스",
            Self::Strings => "현악 앙상블",
            Self::Violin => "바이올린",
            Self::Cello => "첼로",
            Self::Flute => "플루트",
            Self::Clarinet => "클라리넷",
            Self::Trumpet => "트럼펫",
            Self::Xylophone => "실로폰",
            Self::Harmonica => "하모니카",
            Self::Harp => "하프",
            Self::Glockenspiel => "글로켄슈필",
            Self::Tuba => "튜바",
        }
    }

    /// bank 0에서 사용할 0부터 시작하는 GM 프로그램 번호를 반환한다.
    fn program(self) -> i32 {
        match self {
            Self::Piano => 0,
            Self::ElectricPiano => 4,
            Self::Harpsichord => 6,
            Self::Organ => 19,
            Self::Guitar => 24,
            Self::Bass => 32,
            Self::Strings => 48,
            Self::Violin => 40,
            Self::Cello => 42,
            Self::Flute => 73,
            Self::Clarinet => 71,
            Self::Trumpet => 56,
            Self::Xylophone => 13,
            Self::Harmonica => 22,
            Self::Harp => 46,
            Self::Glockenspiel => 9,
            Self::Tuba => 58,
        }
    }

    /// 각 악기의 SF2 엔벌로프를 끝까지 재생할 릴리스 상한을 반환한다.
    pub(crate) fn release_seconds(self) -> f64 {
        match self {
            Self::Xylophone => 13.0,   // 음원 최대 12.000312초.
            Self::Harp => 10.0,        // 음원 최대 9.601989초.
            Self::Glockenspiel => 9.0, // 음원 최대 8.402586초.
            _ => 4.0,                  // 나머지 악기 최대 2.904588초.
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
const SF2: &[u8] = include_bytes!("../assets/soundfonts/FluidR3/FluidR3_GM.sf2");
static BANK: OnceLock<Arc<SoundFont>> = OnceLock::new();
const BLOCK_SIZE: usize = 8;
const MASTER_VOLUME: f32 = 0.25;

/// 오디오 스트림을 열기 전에 공유 음원과 악기 존재 여부를 확인한다.
fn load() -> Result<&'static Arc<SoundFont>> {
    #[cfg(not(target_arch = "wasm32"))]
    let bank = BANK.get_or_init(|| {
        let bank = SoundFont::new(&mut Cursor::new(SF2))
            .expect("the embedded and tested FluidR3 bank must be valid");
        for instrument in Instrument::ALL {
            assert!(
                bank.get_presets().iter().any(|preset| {
                    preset.get_bank_number() == 0
                        && preset.get_patch_number() == instrument.program()
                }),
                "the embedded FluidR3 bank must contain every preview instrument"
            );
        }
        Arc::new(bank)
    });
    #[cfg(target_arch = "wasm32")]
    let bank = BANK
        .get()
        .ok_or_else(|| anyhow::anyhow!("가상 악기를 먼저 불러와 주세요."))?;
    Ok(bank)
}

/// 내려받은 음원에 모든 미리듣기 악기가 있는지 확인해 공유한다.
#[cfg(target_arch = "wasm32")]
pub(crate) fn install(bytes: &[u8]) -> Result<()> {
    if BANK.get().is_some() {
        return Ok(());
    }
    let bank = SoundFont::new(&mut Cursor::new(bytes))?;
    for instrument in Instrument::ALL {
        anyhow::ensure!(
            bank.get_presets().iter().any(|preset| {
                preset.get_bank_number() == 0 && preset.get_patch_number() == instrument.program()
            }),
            "{} 가상 악기를 찾을 수 없습니다.",
            instrument.name()
        );
    }
    let _ = BANK.set(Arc::new(bank));
    Ok(())
}

/// 브라우저의 악기 음원이 준비되었는지 확인한다.
#[cfg(target_arch = "wasm32")]
pub(crate) fn ready() -> bool {
    BANK.get().is_some()
}

/// 파트별 엔진으로 동음 note-off를 격리하며 평상시에는 고정 버퍼로 렌더링한다.
/// 생성과 긴 탐색 복원은 오디오 콜백 밖에서 수행한다.
pub(crate) struct PartSynth {
    synth: Synthesizer,
    instrument: Instrument,
    left: [f32; BLOCK_SIZE],
    right: [f32; BLOCK_SIZE],
    read: usize,
}

impl PartSynth {
    /// 오디오 콜백 밖에서 파트 전용 신시사이저와 고정 버퍼를 만든다.
    pub(crate) fn new(sample_rate: u32) -> Result<Self> {
        let mut settings = SynthesizerSettings::new(i32::try_from(sample_rate)?);
        settings.block_size = BLOCK_SIZE;
        // 파트 수와 별개로 겹치는 샘플층과 릴리스 꼬리의 음성을 미리 확보한다.
        settings.maximum_polyphony = 128;
        settings.enable_reverb_and_chorus = false;
        let synth = Synthesizer::new(load()?, &settings)?;
        let mut part = Self {
            synth,
            instrument: Instrument::default(),
            left: [0.0; BLOCK_SIZE],
            right: [0.0; BLOCK_SIZE],
            read: BLOCK_SIZE,
        };
        part.configure();
        Ok(part)
    }

    /// 음량·팬·선택 악기의 초기 MIDI 상태를 적용한다.
    fn configure(&mut self) {
        self.synth.set_master_volume(MASTER_VOLUME);
        self.synth.process_midi_message(0, 0xB0, 0, 0);
        self.synth.process_midi_message(0, 0xB0, 7, 127);
        self.synth.process_midi_message(0, 0xB0, 11, 127);
        self.synth.process_midi_message(0, 0xB0, 10, 64);
        self.synth
            .process_midi_message(0, 0xC0, self.instrument.program(), 0);
    }

    /// 현재 음색은 유지하고 이후 타격부터 새 악기를 적용한다.
    pub(crate) fn set_instrument(&mut self, instrument: Instrument) {
        self.instrument = instrument;
        self.synth
            .process_midi_message(0, 0xC0, instrument.program(), 0);
    }

    /// 유효한 MIDI 음높이와 세기로 새 음을 시작한다.
    pub(crate) fn note_on(&mut self, pitch: i32, velocity: i32) {
        if (0..=127).contains(&pitch) {
            self.synth.note_on(0, pitch, velocity.clamp(0, 127));
        }
    }

    /// 해당 파트에서 눌린 음의 릴리스를 시작한다.
    pub(crate) fn note_off(&mut self, pitch: i32) {
        if (0..=127).contains(&pitch) {
            self.synth.note_off(0, pitch);
        }
    }

    /// 선택 악기를 유지하며 진행 중인 음과 버퍼를 비운다.
    pub(crate) fn reset(&mut self) {
        self.synth.reset();
        self.read = BLOCK_SIZE;
        self.configure();
    }

    /// 스테레오 SF2 출력을 모노로 합쳐 한 샘플을 반환한다.
    pub(crate) fn next_sample(&mut self) -> f64 {
        if self.read == BLOCK_SIZE {
            self.synth.render(&mut self.left, &mut self.right);
            self.read = 0;
        }
        let sample = 0.5 * (f64::from(self.left[self.read]) + f64::from(self.right[self.read]));
        self.read += 1;
        sample
    }

    /// 부분 블록까지 next_sample과 동일하게 진행하며 필터·루프·엔벌로프를 복원한다.
    /// 호출자가 구간 사이의 MIDI 사건을 재생하며, 곡 전체 PCM 버퍼는 만들지 않는다.
    pub(crate) fn render_discard(&mut self, mut frames: u64) {
        let buffered = frames.min((BLOCK_SIZE - self.read) as u64) as usize;
        self.read += buffered;
        frames -= buffered as u64;
        let mut left = [0.0; 4096];
        let mut right = [0.0; 4096];
        while frames >= BLOCK_SIZE as u64 {
            let count =
                (frames / BLOCK_SIZE as u64 * BLOCK_SIZE as u64).min(left.len() as u64) as usize;
            self.synth.render(&mut left[..count], &mut right[..count]);
            frames -= count as u64;
        }
        for _ in 0..frames {
            self.next_sample();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 지정 악기를 선택한 테스트 신시사이저를 만든다.
    fn configured(instrument: Instrument) -> PartSynth {
        let mut synth = PartSynth::new(16_000).unwrap();
        synth.set_instrument(instrument);
        synth
    }

    /// 신시사이저에서 지정 개수의 샘플을 수집한다.
    fn samples(synth: &mut PartSynth, count: usize) -> Vec<f64> {
        (0..count).map(|_| synth.next_sample()).collect()
    }

    #[test]
    /// 모든 악기의 존재·출력·릴리스 상한을 확인한다.
    fn every_selected_patch_is_present_audible_and_has_a_bounded_release() {
        let bank = load().unwrap();
        let mut signatures = Vec::new();
        for instrument in Instrument::ALL {
            let preset = bank
                .get_presets()
                .iter()
                .find(|preset| {
                    preset.get_bank_number() == 0
                        && preset.get_patch_number() == instrument.program()
                })
                .unwrap();
            let mut maximum_release = 0.0_f32;
            let mut maximum_layers = 0;
            for key in 0..128 {
                for velocity in 1..128 {
                    let mut layers = 0;
                    for preset_region in preset.get_regions() {
                        if !preset_region.contains(key, velocity) {
                            continue;
                        }
                        for region in
                            bank.get_instruments()[preset_region.get_instrument_id()].get_regions()
                        {
                            if region.contains(key, velocity) {
                                layers += 1;
                                maximum_release = maximum_release.max(
                                    preset_region.get_release_volume_envelope()
                                        * region.get_release_volume_envelope(),
                                );
                            }
                        }
                    }
                    maximum_layers = maximum_layers.max(layers);
                }
            }
            let mut synth = configured(instrument);
            synth.note_on(60, 127);
            let attack = samples(&mut synth, 8_000);
            let rms = (attack.iter().map(|value| value * value).sum::<f64>() / attack.len() as f64)
                .sqrt();
            let peak = attack.iter().map(|value| value.abs()).fold(0.0, f64::max);
            println!(
                "{instrument:?}: preset={}, layers={maximum_layers}, release={maximum_release:.6}s, C4 RMS={rms:.6}, peak={peak:.6}",
                preset.get_name()
            );
            assert!(attack.iter().all(|value| value.is_finite()));
            assert!(rms > 0.0001, "{instrument:?} is silent");
            assert!(peak < 1.0, "{instrument:?} clips before mixing");
            assert!(maximum_layers > 0 && maximum_layers <= 8);
            assert!(f64::from(maximum_release) < instrument.release_seconds());
            synth.note_off(60);
            synth.render_discard((16_000.0 * instrument.release_seconds()) as u64);
            assert!(
                samples(&mut synth, 256).iter().all(|value| *value == 0.0),
                "{instrument:?} exceeds the release budget"
            );
            signatures.push(attack);
        }
        for a in 0..signatures.len() {
            for b in 0..a {
                assert_ne!(
                    signatures[a], signatures[b],
                    "different patches share a waveform"
                );
            }
        }
    }

    #[test]
    /// 버린 프레임이 부분 블록의 필터·엔벌로프 상태를 보존하는지 확인한다.
    fn discarded_frames_preserve_filter_and_envelope_state_at_partial_blocks() {
        let mut advanced = configured(Instrument::Strings);
        let mut reference = configured(Instrument::Strings);
        advanced.note_on(60, 99);
        reference.note_on(60, 99);
        for count in [3, 17, 4_103, 0, 15_001] {
            advanced.render_discard(count);
            for _ in 0..count {
                reference.next_sample();
            }
            assert_eq!(samples(&mut advanced, 19), samples(&mut reference, 19));
        }
        advanced.note_off(60);
        reference.note_off(60);
        advanced.render_discard(4_107);
        for _ in 0..4_107 {
            reference.next_sample();
        }
        assert_eq!(
            samples(&mut advanced, 2_003),
            samples(&mut reference, 2_003)
        );
    }

    #[test]
    /// 악기 변경이 현재 음을 보존하고 다음 타격에만 적용되는지 확인한다.
    fn program_change_preserves_existing_sound_and_changes_future_attacks() {
        let mut changed = configured(Instrument::Flute);
        let mut unchanged = configured(Instrument::Flute);
        changed.note_on(72, 104);
        unchanged.note_on(72, 104);
        assert_eq!(samples(&mut changed, 3_001), samples(&mut unchanged, 3_001));
        changed.set_instrument(Instrument::Harpsichord);
        assert_eq!(samples(&mut changed, 2_997), samples(&mut unchanged, 2_997));
        changed.note_on(60, 104);
        unchanged.note_on(60, 104);
        assert_ne!(samples(&mut changed, 2_001), samples(&mut unchanged, 2_001));
    }

    #[test]
    /// 동음의 종료가 다른 파트의 음을 끊지 않는지 확인한다.
    fn same_pitch_note_off_is_isolated_between_parts() {
        let mut released = configured(Instrument::Organ);
        let mut held = configured(Instrument::Organ);
        let mut reference = configured(Instrument::Organ);
        for part in [&mut released, &mut held, &mut reference] {
            part.note_on(60, 110);
            part.render_discard(5_003);
        }
        released.note_off(60);
        let released_samples = samples(&mut released, 8_000);
        let held_samples = samples(&mut held, 8_000);
        assert_eq!(held_samples, samples(&mut reference, 8_000));
        assert_ne!(released_samples, held_samples);
    }

    #[test]
    /// 초기화가 버퍼를 비우면서 선택 악기는 유지하는지 확인한다.
    fn reset_clears_buffered_audio_and_keeps_the_selected_instrument() {
        let mut reset = configured(Instrument::Clarinet);
        reset.note_on(55, 120);
        reset.render_discard(3_005);
        reset.set_instrument(Instrument::Cello);
        reset.reset();
        assert!(samples(&mut reset, 17).iter().all(|value| *value == 0.0));
        let mut fresh = configured(Instrument::Cello);
        fresh.render_discard(17);
        reset.note_on(48, 96);
        fresh.note_on(48, 96);
        assert_eq!(samples(&mut reset, 8_000), samples(&mut fresh, 8_000));
    }
}
