//! 명확한 동시 화음에서 서로 다른 구성음을 남길 가치를 계산합니다.

#[derive(Clone, Copy, Debug)]
pub(crate) struct Harmony {
    root: u8,
    mask: u16,
    kind: ChordKind,
}

#[derive(Clone, Copy, Debug)]
enum ChordKind {
    Major,
    Minor,
    Diminished,
    Augmented,
    DominantSeventh,
    MajorSeventh,
    MinorSeventh,
    MinorMajorSeventh,
    HalfDiminishedSeventh,
    DiminishedSeventh,
    MajorSixth,
    MinorSixth,
    SuspendedSecond,
    SuspendedFourth,
}

const KINDS: [ChordKind; 14] = [
    ChordKind::Major,
    ChordKind::Minor,
    ChordKind::Diminished,
    ChordKind::Augmented,
    ChordKind::DominantSeventh,
    ChordKind::MajorSeventh,
    ChordKind::MinorSeventh,
    ChordKind::MinorMajorSeventh,
    ChordKind::HalfDiminishedSeventh,
    ChordKind::DiminishedSeventh,
    ChordKind::MajorSixth,
    ChordKind::MinorSixth,
    ChordKind::SuspendedSecond,
    ChordKind::SuspendedFourth,
];

impl ChordKind {
    /// 근음으로부터 각 구성음까지의 반음 간격을 반환합니다.
    fn intervals(self) -> &'static [u8] {
        match self {
            Self::Major => &[0, 4, 7],
            Self::Minor => &[0, 3, 7],
            Self::Diminished => &[0, 3, 6],
            Self::Augmented => &[0, 4, 8],
            Self::DominantSeventh => &[0, 4, 7, 10],
            Self::MajorSeventh => &[0, 4, 7, 11],
            Self::MinorSeventh => &[0, 3, 7, 10],
            Self::MinorMajorSeventh => &[0, 3, 7, 11],
            Self::HalfDiminishedSeventh => &[0, 3, 6, 10],
            Self::DiminishedSeventh => &[0, 3, 6, 9],
            Self::MajorSixth => &[0, 4, 7, 9],
            Self::MinorSixth => &[0, 3, 7, 9],
            Self::SuspendedSecond => &[0, 2, 7],
            Self::SuspendedFourth => &[0, 5, 7],
        }
    }

    /// 모호성 비교용 화음을 점수 부여 대상에서 제외합니다.
    fn can_score(self) -> bool {
        !matches!(
            self,
            Self::MajorSixth | Self::MinorSixth | Self::SuspendedSecond | Self::SuspendedFourth
        )
    }
}

impl Harmony {
    /// 최저음과 독립적으로 추정한 근음의 음급을 반환합니다.
    pub(crate) fn root(self) -> u8 {
        self.root
    }

    /// 화음을 구성하는 절대 음급의 비트마스크를 반환합니다.
    pub(crate) fn mask(self) -> u16 {
        self.mask
    }

    /// 해당 구성음을 처음 확보할 때의 가치를 반환하며 비화성음은 감점하지 않습니다.
    pub(crate) fn weight(self, pitch: i32) -> f64 {
        let interval = (pitch.rem_euclid(12) as u8 + 12 - self.root) % 12;
        let intervals = self.kind.intervals();
        if interval == 0 {
            0.65
        } else if interval == intervals[1] || intervals.get(3) == Some(&interval) {
            1.0
        } else if interval == intervals[2] {
            if interval == 7 { 0.25 } else { 1.0 }
        } else {
            0.0
        }
    }

    /// 선택된 음급 집합의 가치를 합산하며 옥타브 중복은 한 번만 셉니다.
    pub(crate) fn coverage(self, mask: u16) -> f64 {
        let selected = mask & self.mask;
        (0..12)
            .filter(|&pitch| selected & (1 << pitch) != 0)
            .map(|pitch| self.weight(pitch))
            .sum()
    }
}

/// 세 개 또는 네 개 음급이 단 하나의 화음과 정확히 일치할 때만 해석합니다.
pub(crate) fn analyze(pitches: impl IntoIterator<Item = i32>) -> Option<Harmony> {
    let mask = pitches
        .into_iter()
        .fold(0u16, |mask, pitch| mask | (1 << pitch.rem_euclid(12)));
    if !(3..=4).contains(&mask.count_ones()) {
        return None;
    }
    let mut candidate = None;
    for root in 0..12 {
        for kind in KINDS {
            let expected = kind.intervals().iter().fold(0u16, |mask, interval| {
                mask | (1 << ((root + interval) % 12))
            });
            if expected != mask {
                continue;
            }
            if candidate.is_some() {
                return None;
            }
            candidate = Some(Harmony { root, mask, kind });
        }
    }
    candidate.filter(|harmony| harmony.kind.can_score())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 테스트 음표를 옥타브 중복 없는 비트마스크로 바꿉니다.
    fn mask(pitches: &[i32]) -> u16 {
        pitches
            .iter()
            .fold(0u16, |mask, pitch| mask | (1 << pitch.rem_euclid(12)))
    }

    #[test]
    /// 대표적인 명확한 화음에서 3음·7음과 변형된 5음을 구별합니다.
    fn clear_chords_reward_characteristic_members() {
        let dominant = analyze([48, 52, 55, 58]).unwrap();
        assert_eq!(dominant.root, 0);
        assert_eq!(dominant.mask, mask(&[0, 4, 7, 10]));
        assert_eq!(dominant.weight(48), 0.65);
        assert_eq!(dominant.weight(52), 1.0);
        assert_eq!(dominant.weight(55), 0.25);
        assert_eq!(dominant.weight(58), 1.0);
        assert_eq!(dominant.weight(54), 0.0);
        let diminished = analyze([60, 63, 66]).unwrap();
        assert_eq!(diminished.weight(66), 1.0);
        assert_eq!(diminished.weight(67), 0.0);
        let major_seventh = analyze([60, 64, 67, 71]).unwrap();
        assert_eq!(major_seventh.weight(71), 1.0);
        let minor_major_seventh = analyze([60, 63, 67, 71]).unwrap();
        assert_eq!(minor_major_seventh.weight(63), 1.0);
        assert_eq!(minor_major_seventh.weight(71), 1.0);
    }

    #[test]
    /// 전위·옥타브 배치·입력 순서가 근음과 구성음의 가치를 바꾸지 않습니다.
    fn inversions_keep_the_root_independent_of_the_bass() {
        let reference = analyze([48, 52, 55, 58]).unwrap();
        for pitches in [[52, 55, 58, 60], [55, 58, 60, 64], [70, 64, 55, 60]] {
            let inverted = analyze(pitches).unwrap();
            assert_eq!(inverted.root, reference.root);
            assert_eq!(inverted.mask, reference.mask);
            for pitch in 0..128 {
                assert_eq!(inverted.weight(pitch), reference.weight(pitch));
            }
        }
    }

    #[test]
    /// 모든 반음 이조에서 화음 판정과 구성음의 가치가 유지됩니다.
    fn transposition_preserves_roles_and_coverage() {
        let reference = analyze([48, 52, 55, 58]).unwrap();
        for shift in -12..=12 {
            let shifted = analyze([48, 52, 55, 58].map(|pitch| pitch + shift)).unwrap();
            assert_eq!(shifted.root, shift.rem_euclid(12) as u8);
            for pitch in 36..84 {
                assert_eq!(shifted.weight(pitch + shift), reference.weight(pitch));
            }
            assert!((shifted.coverage(shifted.mask) - 2.9).abs() < 1e-12);
        }
    }

    #[test]
    /// 같은 음급을 거듭 넣어도 해석과 선택 집합의 보상이 증가하지 않습니다.
    fn duplicated_octaves_do_not_add_coverage() {
        let harmony = analyze([48, 52, 55, 58, 60, 64, 67, 70]).unwrap();
        let selected = mask(&[48, 52]);
        assert_eq!(
            harmony.coverage(selected),
            harmony.coverage(mask(&[48, 52, 60, 64]))
        );
        assert_eq!(
            harmony.coverage(selected | mask(&[72])) - harmony.coverage(selected),
            0.0
        );
        assert!(
            (harmony.coverage(selected | mask(&[58])) - harmony.coverage(selected) - 1.0).abs()
                < 1e-12
        );
        assert_eq!(harmony.coverage(mask(&[49, 54])), 0.0);
        assert_eq!(harmony.coverage(u16::MAX), harmony.coverage(harmony.mask));
    }

    #[test]
    /// 대칭 화음과 6화음·7화음 및 계류 화음의 중복 해석을 보류합니다.
    fn ambiguous_chords_abstain_in_every_transposition() {
        for chord in [
            &[60, 64, 68][..],
            &[60, 63, 66, 69],
            &[60, 64, 67, 69],
            &[60, 63, 67, 69],
            &[60, 63, 67, 70],
            &[60, 63, 66, 70],
            &[60, 62, 67],
            &[60, 65, 67],
        ] {
            for shift in 0..12 {
                assert!(analyze(chord.iter().map(|pitch| pitch + shift)).is_none());
            }
        }
    }

    #[test]
    /// 성긴 음군·반음계 음군·추가 음이 섞인 화음에는 보너스를 만들지 않습니다.
    fn sparse_or_nonmatching_pitch_sets_abstain() {
        for pitches in [
            &[][..],
            &[60],
            &[60, 67, 72],
            &[60, 61, 62],
            &[60, 61, 64, 67],
            &[60, 64, 67, 70, 74],
        ] {
            assert!(analyze(pitches.iter().copied()).is_none());
        }
    }
}
