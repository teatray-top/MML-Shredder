//! 내장 Yamaha 피아노 녹음을 음표의 경과시간에 맞춰 재생한다.

use anyhow::Result;
use rustysynth::SoundFont;
use std::{io::Cursor, sync::OnceLock};

#[cfg(not(target_arch = "wasm32"))]
const PIANO: &[u8] = include_bytes!("../assets/soundfonts/YDPGrand/YDP-GrandPiano-20160804.sf2");
pub(super) const MAX_RELEASE_SECONDS: f64 = 0.81;
static BANK: OnceLock<SoundFont> = OnceLock::new();

/// 검증된 내장 피아노 음원을 한 번만 읽어 공유한다.
pub(super) fn load() -> Result<&'static SoundFont> {
    #[cfg(not(target_arch = "wasm32"))]
    let bank = BANK.get_or_init(|| {
        SoundFont::new(&mut Cursor::new(PIANO))
            .expect("the embedded and tested YDP Grand Piano bank must be valid")
    });
    #[cfg(target_arch = "wasm32")]
    let bank = BANK
        .get()
        .ok_or_else(|| anyhow::anyhow!("피아노를 먼저 불러와 주세요."))?;
    Ok(bank)
}

/// 내려받은 피아노 음원의 샘플 범위와 건반 대응을 확인해 공유한다.
#[cfg(target_arch = "wasm32")]
pub(crate) fn install(bytes: &[u8]) -> Result<()> {
    if BANK.get().is_some() {
        return Ok(());
    }
    let bank = SoundFont::new(&mut Cursor::new(bytes))?;
    let preset = bank
        .get_presets()
        .first()
        .ok_or_else(|| anyhow::anyhow!("피아노 프리셋이 없습니다."))?;
    for key in 20..=109 {
        for velocity in 1..=127 {
            let preset_region = preset
                .get_regions()
                .iter()
                .find(|region| region.contains(key, velocity))
                .ok_or_else(|| anyhow::anyhow!("피아노의 건반 정보가 없습니다."))?;
            let instrument = bank
                .get_instruments()
                .get(preset_region.get_instrument_id())
                .ok_or_else(|| anyhow::anyhow!("피아노의 악기 정보가 잘못되었습니다."))?;
            let region = instrument
                .get_regions()
                .iter()
                .find(|region| region.contains(key, velocity))
                .ok_or_else(|| anyhow::anyhow!("피아노의 샘플 정보가 없습니다."))?;
            anyhow::ensure!(
                region.get_sample_start() >= 0
                    && region.get_sample_start() < region.get_sample_end()
                    && region.get_sample_end() as usize <= bank.get_wave_data().len()
                    && region.get_sample_id() < bank.get_sample_headers().len(),
                "피아노의 샘플 범위가 잘못되었습니다."
            );
        }
    }
    let _ = BANK.set(bank);
    Ok(())
}

/// 브라우저의 피아노 음원이 준비되었는지 확인한다.
#[cfg(target_arch = "wasm32")]
pub(crate) fn ready() -> bool {
    BANK.get().is_some()
}

pub(super) struct Tone {
    wave: &'static [i16],
    samples_per_second: f64,
    attenuation: f64,
    delay: f64,
    attack: f64,
    hold: f64,
    decay: f64,
    sustain: f64,
    release: f64,
}

impl Tone {
    /// 음높이와 세기에 맞는 녹음층·튜닝·엔벌로프를 선택한다.
    pub(super) fn new(pitch: i32, velocity: i32) -> Option<Self> {
        if !(0..=127).contains(&pitch) || velocity <= 0 {
            return None;
        }
        let bank = load().ok()?;
        let key = pitch.clamp(20, 109);
        // YDP는 preset 영역에서 세기층을 고르므로 instrument 0으로 고정하면 안 된다.
        let preset = bank.get_presets()[0]
            .get_regions()
            .iter()
            .find(|r| r.contains(key, velocity))?;
        let region = bank.get_instruments()[preset.get_instrument_id()]
            .get_regions()
            .iter()
            .find(|r| r.contains(key, velocity))?;
        let sample = &bank.get_sample_headers()[region.get_sample_id()];
        let start = region.get_sample_start() as usize;
        let end = region.get_sample_end() as usize;
        let cents = f64::from(
            (pitch - region.get_root_key()) * region.get_scale_tuning()
                + (region.get_coarse_tune() + preset.get_coarse_tune()) * 100
                + region.get_fine_tune()
                + preset.get_fine_tune(),
        );
        Some(Self {
            wave: &bank.get_wave_data()[start..end],
            samples_per_second: f64::from(sample.get_sample_rate()) * (cents / 1200.0).exp2(),
            attenuation: 10.0_f64.powf(
                -f64::from(region.get_initial_attenuation() + preset.get_initial_attenuation())
                    / 20.0,
            ),
            delay: f64::from(
                region.get_delay_volume_envelope() * preset.get_delay_volume_envelope(),
            ),
            attack: f64::from(
                region.get_attack_volume_envelope() * preset.get_attack_volume_envelope(),
            ),
            hold: f64::from(region.get_hold_volume_envelope() * preset.get_hold_volume_envelope()),
            decay: f64::from(
                region.get_decay_volume_envelope() * preset.get_decay_volume_envelope(),
            ),
            sustain: 10.0_f64.powf(
                -f64::from(
                    region.get_sustain_volume_envelope() + preset.get_sustain_volume_envelope(),
                ) / 20.0,
            ),
            release: f64::from(
                region.get_release_volume_envelope() * preset.get_release_volume_envelope(),
            ),
        })
    }

    /// 건반을 누른 뒤 경과시간에 해당하는 음량을 계산한다.
    fn held_envelope(&self, age: f64) -> f64 {
        let age = age - self.delay;
        if age <= 0.0 {
            0.0
        } else if age < self.attack {
            age / self.attack
        } else if age < self.attack + self.hold {
            1.0
        } else {
            (-9.226 * (age - self.attack - self.hold) / self.decay)
                .exp()
                .max(self.sustain)
        }
    }

    /// 절대 샘플 위치를 보간하며 녹음이 끝나면 무음을 반환한다.
    fn pcm(&self, age: f64) -> f64 {
        let cursor = age.max(0.0) * self.samples_per_second;
        let index = cursor as usize;
        if index >= self.wave.len() {
            return 0.0;
        }
        // 3차 보간으로 타격음을 보존하고 녹음 바깥을 0으로 채워 꼬리 반복을 막는다.
        let at = |offset: isize| {
            index
                .checked_add_signed(offset)
                .and_then(|i| self.wave.get(i))
                .map_or(0.0, |v| f64::from(*v))
        };
        let (a, b, c, d) = (at(-1), at(0), at(1), at(2));
        let t = cursor - index as f64;
        let value = b + 0.5
            * t
            * (c - a + t * (2.0 * a - 5.0 * b + 4.0 * c - d + t * (3.0 * (b - c) + d - a)));
        value * (self.attenuation / 32768.0)
    }

    /// 원래 경과시간과 건반 유지 길이로 피아노 샘플을 계산한다.
    pub(super) fn sample(&self, age: f64, held_duration: f64) -> f64 {
        let envelope = if age <= held_duration {
            self.held_envelope(age)
        } else {
            self.held_envelope(held_duration)
                * (-9.226 * (age - held_duration) / self.release).exp()
        };
        self.pcm(age) * envelope
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustysynth::LoopMode;

    #[test]
    /// 내장 음원의 다섯 세기층과 반복 없는 꼬리·릴리스 범위를 확인한다.
    fn bundled_grand_has_five_real_layers_and_no_looped_tails() {
        let bank = load().unwrap();
        assert!(std::ptr::eq(bank, load().unwrap()));
        assert_eq!(bank.get_sample_headers().len(), 121);
        assert_eq!(bank.get_presets()[0].get_regions().len(), 5);
        for key in 20..=109 {
            for velocity in 1..=127 {
                let presets: Vec<_> = bank.get_presets()[0]
                    .get_regions()
                    .iter()
                    .filter(|r| r.contains(key, velocity))
                    .collect();
                assert_eq!(presets.len(), 1);
                let regions: Vec<_> = bank.get_instruments()[presets[0].get_instrument_id()]
                    .get_regions()
                    .iter()
                    .filter(|r| r.contains(key, velocity))
                    .collect();
                assert_eq!(regions.len(), 1);
                let region = regions[0];
                assert!(region.get_sample_modes() == LoopMode::NoLoop);
                assert_eq!(
                    bank.get_sample_headers()[region.get_sample_id()].get_sample_type(),
                    1
                );
                assert_eq!(region.get_modulation_lfo_to_pitch(), 0);
                assert_eq!(region.get_modulation_envelope_to_pitch(), 0);
                assert_eq!(region.get_initial_filter_q(), 0.0);
                assert!(region.get_initial_filter_cutoff_frequency() > 19_000.0);
                assert_eq!(region.get_sustain_volume_envelope(), 0.0);
                assert!(f64::from(region.get_release_volume_envelope()) < MAX_RELEASE_SECONDS);
            }
        }
        let soft = Tone::new(60, 50).unwrap();
        let loud = Tone::new(60, 120).unwrap();
        assert!(
            !std::ptr::eq(soft.wave.as_ptr(), loud.wave.as_ptr()),
            "velocity must select a different recording"
        );
    }

    #[test]
    /// 탐색 시 녹음 위치와 음원 릴리스가 보존되는지 확인한다.
    fn recorded_pcm_and_bank_release_are_preserved_when_seeking() {
        let tone = Tone::new(60, 100).unwrap();
        let expected = f64::from(tone.wave[5_000]) / 32768.0;
        assert!((tone.pcm(5_000.0 / tone.samples_per_second) - expected).abs() < 1.0e-9);
        assert!(tone.sample(14.0 * 60.0, 15.0 * 60.0).abs() < 1.0e-12);
        assert!((0.79..0.81).contains(&tone.release));
        let (age, held) = (0.9, 0.5);
        let expected =
            tone.pcm(age) * tone.held_envelope(held) * (-9.226 * (age - held) / tone.release).exp();
        assert!((tone.sample(age, held) - expected).abs() < 1.0e-12);
    }
}
