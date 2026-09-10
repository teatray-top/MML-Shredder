//! 분할 결과를 기존 Python 출력 파일과 대조합니다.

use std::path::Path;

use mmlfold::{core, split::SplitOptions, workflow};

#[test]
/// 기존 Python 분할 파일과 모든 파트 문자열을 대조합니다.
fn default_scroll_workflow_matches_all_existing_python_scrolls() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut checked_parts = 0;
    for (input, directory, stem, count) in [
        ("Op39No11_solo_3part.mmi", "scrolls_solo", "duet_t1", 7),
        ("Op39No11_full_6part.mmi", "scrolls_ensemble", "duet", 7),
        ("Op39No11_second_3part.mmi", "scrolls_second", "duet_t2", 5),
    ] {
        let source = core::load_mmi(&root.join(input)).unwrap();
        let output = workflow::scrolls(&source, &SplitOptions::default(), stem).unwrap();
        assert_eq!(output.split.as_ref().unwrap().chunks.len(), count);
        assert_eq!(output.artifacts.len(), count);
        assert!(
            output
                .artifacts
                .iter()
                .all(|artifact| artifact.name.ends_with(".mmi"))
        );
        for i in 0..count {
            let name = format!("{stem}_{:02}", i + 1);
            let reference_path = root.join(directory).join(format!("{name}.mmi"));
            let reference = std::fs::read_to_string(&reference_path)
                .unwrap()
                .replace("\r\n", "\n");
            let mmi = &output.artifacts[i];
            assert_eq!(mmi.name, format!("{name}.mmi"));
            assert_eq!(mmi.text, reference, "{} differs", reference_path.display());
            let reference_score = core::parse_mmi(&reference).unwrap();
            let expected = workflow::game_export(&reference_score).unwrap().parts;
            assert_eq!(
                mmi.game_parts, expected,
                "{name} stored part metadata differs"
            );
            let reference_txt =
                std::fs::read_to_string(root.join(directory).join(format!("{name}.txt"))).unwrap();
            let expected_payloads: Vec<_> = reference_txt.lines().skip(1).step_by(2).collect();
            let stored_payloads = mmi
                .game_parts
                .iter()
                .flatten()
                .map(String::as_str)
                .collect::<Vec<_>>();
            assert_eq!(
                stored_payloads, expected_payloads,
                "{name} MMI parts differ from known-working Python TXT"
            );
            assert!(
                mmi.game_parts
                    .iter()
                    .flatten()
                    .all(|part| part.len() <= 2400)
            );
            checked_parts += expected_payloads.len();
        }
    }
    assert_eq!(checked_parts, 78);
}
