//! GUI 설정·결과 캐시·미리듣기 상태 전환을 검사합니다.

use super::*;

/// 같은 이름이 반복되는 6트랙 원본 악보를 만듭니다.
fn source_with_six_tracks() -> Loaded {
    // Match the reported structure: repeated track names do not merge tracks.
    let mut text = "[mml-score]\nversion=1\ntempo=0T120\n".to_owned();
    for index in 0..6 {
        text.push_str(&format!(
            "mml-track=MML@t120o{}l16{},o3l8{},o2l4{};\nname=Track{}\nvisible=true\n",
            3 + index % 3,
            "cdef".repeat(24),
            "ga".repeat(24),
            "ce".repeat(12),
            1 + index / 3,
        ));
    }
    let score = core::parse_mmi(&text).unwrap();
    let notes = score.all_notes();
    Loaded {
        path: PathBuf::from("six-track-source.mmi"),
        notes: notes.len(),
        polyphony: verify::max_polyphony(&notes),
        score: Arc::new(score),
    }
}

/// 테스트용 GUI 상태를 만듭니다.
fn app() -> (egui::Context, MmlApp) {
    let ctx = egui::Context::default();
    let mut app = MmlApp::with_context(&ctx, None);
    app.accept_source(source_with_six_tracks());
    app.fold.layout = Layout::Roles;
    app.split.limit = 48;
    (ctx, app)
}

/// 편곡·분할 작업을 실행하고 완료를 기다립니다.
fn run_job(app: &mut MmlApp, ctx: &egui::Context) {
    app.run(ctx);
    wait_job(app);
}

/// 비동기 GUI 작업이 완료될 때까지 확인합니다.
fn wait_job(app: &mut MmlApp) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while app.busy() {
        assert!(std::time::Instant::now() < deadline, "GUI job timed out");
        std::thread::sleep(Duration::from_millis(1));
        app.poll();
    }
    assert!(app.error.is_none(), "GUI job failed: {:?}", app.error);
}

struct MidiFixture {
    directory: PathBuf,
    path: PathBuf,
    bytes: Vec<u8>,
}

impl MidiFixture {
    /// 직접 저장을 검사할 MIDI 파일을 임시 폴더에 만듭니다.
    fn new() -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("mmlfold-gui-midi-{}-{unique}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("direct-input.MID");
        // Format 0, 480 PPQ, tempo 120, C4 from tick 0 to 480.
        let mut bytes = b"MThd\x00\x00\x00\x06\x00\x00\x00\x01\x01\xe0".to_vec();
        bytes.extend_from_slice(b"MTrk\x00\x00\x00\x14");
        bytes.extend_from_slice(&[
            0x00, 0xff, 0x51, 0x03, 0x07, 0xa1, 0x20, 0x00, 0x90, 0x3c, 0x64, 0x83, 0x60, 0x80,
            0x3c, 0x00, 0x00, 0xff, 0x2f, 0x00,
        ]);
        std::fs::write(&path, &bytes).unwrap();
        Self {
            directory,
            path,
            bytes,
        }
    }
}

impl Drop for MidiFixture {
    /// 테스트에서 만든 임시 폴더를 정리합니다.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
/// MIDI를 편곡 없이 MMI로 저장하는 GUI 흐름을 검사합니다.
fn opening_midi_allows_direct_mmi_export_without_creating_an_arrangement() {
    let fixture = MidiFixture::new();
    let (ctx, mut app) = app();
    run_job(&mut app, &ctx);
    assert!(app.fold_result.is_some());
    app.load(&ctx, fixture.path.clone());
    wait_job(&mut app);
    app.sync_playback();
    assert!(app.can_save());
    assert!(app.result.is_none());
    assert!(app.fold_result.is_none());
    assert!(app.split_result.is_none());
    assert!(app.folded_source.is_none());
    assert!(app.displayed_result().is_none());
    let original = app.source.as_ref().unwrap().score.clone();
    assert_eq!(original.all_notes().len(), 1);
    assert_eq!(original.all_notes()[0].pitch, 60);
    let revision = app.score_revision;
    let timeline = app.timeline.clone().unwrap();
    app.player.seek(0.25);
    app.player.set_part_instrument(0, 0, Instrument::Flute);
    app.follow_playhead = false;
    let exported = app.export_result().unwrap().unwrap().into_owned();
    assert!(exported.kind == WorkKind::Import);
    assert_eq!(exported.artifacts.len(), 1);
    assert_eq!(exported.artifacts[0].name, "direct-input.mmi");
    let folder = workflow::export_new_folder(&exported, &fixture.directory, &app.stem()).unwrap();
    let saved = core::load_mmi(&folder.join(&exported.artifacts[0].name)).unwrap();
    assert_eq!(saved.all_notes(), original.all_notes());
    assert_eq!(saved.tempos(), original.tempos());
    assert_eq!(std::fs::read(&fixture.path).unwrap(), fixture.bytes);
    assert_eq!(app.score_revision, revision);
    assert!(Arc::ptr_eq(app.timeline.as_ref().unwrap(), &timeline));
    assert_eq!(app.player.snapshot().position_seconds, 0.25);
    assert_eq!(app.player.part_instrument(0, 0), Instrument::Flute);
    assert!(!app.follow_playhead);
    assert!(app.result.is_none());

    app.tab = Tab::Mml;
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| app.artifacts(ui));
    });
    assert!(app.result.is_none());
    assert_eq!(app.score_revision, revision);
    app.set_mode(Mode::Split);
    assert!(app.can_save());
    assert!(app.export_result().unwrap().unwrap().kind == WorkKind::Import);
    assert!(Arc::ptr_eq(&app.input_source().unwrap().score, &original));
}

#[test]
/// 편곡·분할 결과가 직접 MIDI 변환보다 우선 저장되는지 검사합니다.
fn active_fold_and_split_outputs_take_precedence_over_direct_midi_export() {
    let (ctx, mut app) = app();
    let mut source = source_with_six_tracks();
    source.path.set_extension("midi");
    app.accept_source(source);
    assert!(app.export_result().unwrap().unwrap().kind == WorkKind::Import);
    run_job(&mut app, &ctx);
    let folded = app.result.as_ref().unwrap().artifacts[0].text.clone();
    app.show_original = true;
    let exported = app.export_result().unwrap().unwrap();
    assert!(exported.kind == WorkKind::Fold);
    assert_eq!(exported.artifacts[0].text, folded);
    drop(exported);

    app.set_mode(Mode::Split);
    assert!(app.split_uses_folded);
    assert!(!app.can_save());
    assert!(app.export_result().unwrap().is_none());
    run_job(&mut app, &ctx);
    assert_split_tracks(&app, 2);
    assert!(app.export_result().unwrap().unwrap().kind == WorkKind::Split);

    app.set_split_source(false);
    assert!(app.result.is_none());
    let exported = app.export_result().unwrap().unwrap();
    assert!(exported.kind == WorkKind::Import);
    assert_eq!(exported.score.tracks.len(), 6);
    drop(exported);
    app.set_mode(Mode::Fold);
    assert_eq!(
        app.export_result().unwrap().unwrap().artifacts[0].text,
        folded
    );
    assert!(app.result.as_ref().unwrap().kind == WorkKind::Fold);

    app.accept_source(source_with_six_tracks());
    assert!(!app.can_save());
    assert!(app.export_result().unwrap().is_none());
}

/// 분할 결과의 모든 장에서 트랙 수를 검사합니다.
fn assert_split_tracks(app: &MmlApp, count: usize) {
    let result = app.result.as_ref().unwrap();
    assert!(result.kind == WorkKind::Split);
    assert_eq!(result.score.tracks.len(), count);
    assert!(result.split.as_ref().unwrap().chunks.len() > 1);
    for artifact in result.artifacts.iter().filter(|a| a.name.ends_with(".mmi")) {
        let score = core::parse_mmi(&artifact.text).unwrap();
        assert_eq!(score.tracks.len(), count, "{}", artifact.name);
        assert_eq!(artifact.game_parts.len(), count);
        for parts in &artifact.game_parts {
            assert_eq!(parts.len(), 3);
            assert!(parts.iter().all(|part| part.len() <= app.split.limit));
        }
    }
}

#[test]
/// 편곡 탭 전환에 이전 6트랙 분할이 남지 않는지 검사합니다.
fn switching_to_fold_never_displays_the_previous_six_track_split() {
    let (ctx, mut app) = app();
    app.set_mode(Mode::Split);
    run_job(&mut app, &ctx);
    assert_split_tracks(&app, 6);
    app.set_mode(Mode::Fold);
    assert!(app.result.is_none());
    assert!(app.tab == Tab::Preview);
    assert_eq!(app.input_source().unwrap().score.tracks.len(), 6);
    app.set_mode(Mode::Split);
    assert_split_tracks(&app, 6);
}

#[test]
/// 분할 탭이 2트랙 편곡을 사용하고 원본을 보존하는지 검사합니다.
fn ordinary_split_tab_uses_the_two_track_fold_and_keeps_original() {
    let (ctx, mut app) = app();
    let original = app.source.as_ref().unwrap().score.clone();
    let original_path = app.source.as_ref().unwrap().path.clone();
    run_job(&mut app, &ctx);
    let folded_text = app.result.as_ref().unwrap().artifacts[0].text.clone();
    assert_eq!(app.result.as_ref().unwrap().score.tracks.len(), 2);
    app.set_mode(Mode::Split);
    assert!(app.split_uses_folded);
    assert_eq!(app.input_source().unwrap().score.tracks.len(), 2);
    assert!(app.result.is_none());
    run_job(&mut app, &ctx);
    assert_split_tracks(&app, 2);
    app.set_mode(Mode::Fold);
    assert!(app.result.as_ref().unwrap().kind == WorkKind::Fold);
    assert_eq!(app.result.as_ref().unwrap().artifacts[0].text, folded_text);
    assert!(Arc::ptr_eq(&app.source.as_ref().unwrap().score, &original));
    assert_eq!(app.source.as_ref().unwrap().path, original_path);
}

#[test]
/// 결과 분할 버튼이 원본과 편곡 결과를 모두 보존하는지 검사합니다.
fn split_result_button_preserves_both_original_and_folded_result() {
    let (ctx, mut app) = app();
    let original = app.source.as_ref().unwrap().score.clone();
    run_job(&mut app, &ctx);
    app.prepare_split();
    assert!(app.mode == Mode::Split);
    assert_eq!(app.input_source().unwrap().score.tracks.len(), 2);
    assert!(app.fold_result.is_some());
    assert!(Arc::ptr_eq(&app.source.as_ref().unwrap().score, &original));
    app.set_mode(Mode::Fold);
    assert_eq!(app.result.as_ref().unwrap().score.tracks.len(), 2);
}

#[test]
/// 분할 대상 변경 시 다른 트랙 수의 결과를 재사용하지 않는지 검사합니다.
fn changing_split_input_cannot_reuse_results_for_another_track_count() {
    let (ctx, mut app) = app();
    run_job(&mut app, &ctx);
    app.set_mode(Mode::Split);
    run_job(&mut app, &ctx);
    assert_split_tracks(&app, 2);
    app.set_split_source(false);
    assert!(app.result.is_none());
    assert!(app.fold_result.is_some());
    assert_eq!(app.input_source().unwrap().score.tracks.len(), 6);
    run_job(&mut app, &ctx);
    assert_split_tracks(&app, 6);
    app.set_split_source(true);
    assert!(app.result.is_none());
    assert_eq!(app.input_source().unwrap().score.tracks.len(), 2);
}

#[test]
/// 편곡 설정 변경의 결과 무효화와 분할 입력 교체를 검사합니다.
fn edited_fold_settings_clear_old_outputs_and_new_fold_replaces_split_input() {
    let (ctx, mut app) = app();
    run_job(&mut app, &ctx);
    app.set_mode(Mode::Split);
    run_job(&mut app, &ctx);
    app.set_mode(Mode::Fold);
    app.fold.tracks = 3;
    app.invalidate_fold();
    assert!(app.result.is_none());
    assert!(app.fold_result.is_none());
    assert!(app.split_result.is_none());
    assert!(app.folded_source.is_none());
    assert_eq!(app.source.as_ref().unwrap().score.tracks.len(), 6);
    run_job(&mut app, &ctx);
    assert_eq!(app.result.as_ref().unwrap().score.tracks.len(), 3);
    app.set_mode(Mode::Split);
    assert_eq!(app.input_source().unwrap().score.tracks.len(), 3);
    run_job(&mut app, &ctx);
    assert_split_tracks(&app, 3);
}

/// 편곡 설정 UI의 한 프레임을 그립니다.
fn fold_options_frame(
    ctx: &egui::Context,
    app: &mut MmlApp,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(304.0, 900.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.fold_options(ui));
        },
    )
}

/// 그려진 도형에서 표시 문자열을 모읍니다.
fn painted_text(output: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
    /// 중첩 도형의 텍스트를 재귀적으로 모읍니다.
    fn collect(shape: &egui::Shape, text: &mut Vec<(String, egui::Rect)>) {
        match shape {
            egui::Shape::Text(shape) => {
                text.push((shape.galley.text().to_owned(), shape.visual_bounding_rect()));
            }
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect(shape, text);
                }
            }
            _ => {}
        }
    }
    let mut text = Vec::new();
    for shape in &output.shapes {
        collect(&shape.shape, &mut text);
    }
    text
}

/// 편곡 설정에 마우스 클릭 입력을 보냅니다.
fn click_fold_option(ctx: &egui::Context, app: &mut MmlApp, label: &str, number: bool) {
    let output = fold_options_frame(ctx, app, Vec::new());
    let text = painted_text(&output);
    let label_rect = text.iter().find(|(s, _)| s == label).unwrap().1;
    let point = if number {
        text.iter()
            .filter(|(s, rect)| {
                s.parse::<i32>().is_ok()
                    && rect.left() > label_rect.right()
                    && (rect.center().y - label_rect.center().y).abs() < 10.0
            })
            .min_by(|a, b| a.1.left().total_cmp(&b.1.left()))
            .unwrap()
            .1
            .center()
    } else {
        label_rect.center()
    };
    for pressed in [true, false] {
        fold_options_frame(
            ctx,
            app,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}

/// DR 압축 경계 값에 키보드 입력을 보냅니다.
fn enter_volume_bound(ctx: &egui::Context, app: &mut MmlApp, label: &str, text: &str) {
    click_fold_option(ctx, app, label, true);
    fold_options_frame(ctx, app, vec![egui::Event::Text(text.to_owned())]);
    fold_options_frame(
        ctx,
        app,
        vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    fold_options_frame(ctx, app, Vec::new());
}

#[test]
/// DR 설정이 저장 결과를 무효화하고 작업에 전달되는지 검사합니다.
fn dr_compression_controls_invalidate_exports_and_reach_the_fold_job() {
    let (ctx, mut app) = app();
    assert!(app.fold.volume_range.is_none());
    app.gain_mode = 1;
    run_job(&mut app, &ctx);
    assert!(
        app.result
            .as_ref()
            .unwrap()
            .score
            .all_notes()
            .iter()
            .all(|n| n.vel == 8)
    );
    app.prepare_split();
    run_job(&mut app, &ctx);
    app.set_mode(Mode::Fold);
    let original = app.source.as_ref().unwrap().score.clone();

    click_fold_option(&ctx, &mut app, "DR 압축", false);

    assert_eq!(
        app.fold.volume_range,
        Some(VolumeRange { min: 11, max: 15 })
    );
    assert_eq!(app.gain_mode, 1);
    assert!(app.result.is_none());
    assert!(app.fold_result.is_none());
    assert!(app.split_result.is_none());
    assert!(app.folded_source.is_none());
    assert!(!app.split_uses_folded);
    assert!(Arc::ptr_eq(&app.source.as_ref().unwrap().score, &original));
    run_job(&mut app, &ctx);
    assert!(
        app.result
            .as_ref()
            .unwrap()
            .score
            .all_notes()
            .iter()
            .all(|n| n.vel == 13)
    );

    enter_volume_bound(&ctx, &mut app, "최소", "99");
    assert_eq!(
        app.fold.volume_range,
        Some(VolumeRange { min: 15, max: 15 })
    );
    assert!(app.result.is_none());
    assert!(app.fold_result.is_none());
    run_job(&mut app, &ctx);
    assert!(
        app.result
            .as_ref()
            .unwrap()
            .score
            .all_notes()
            .iter()
            .all(|n| n.vel == 15)
    );

    // 최솟값으로 제한되어 실제 값이 그대로면 저장 결과도 유지합니다.
    let revision = app.score_revision;
    enter_volume_bound(&ctx, &mut app, "최대", "0");
    assert_eq!(
        app.fold.volume_range,
        Some(VolumeRange { min: 15, max: 15 })
    );
    assert_eq!(app.score_revision, revision);
    assert!(app.result.is_some());

    enter_volume_bound(&ctx, &mut app, "최소", "-99");
    enter_volume_bound(&ctx, &mut app, "최대", "0");
    assert_eq!(app.fold.volume_range, Some(VolumeRange { min: 1, max: 1 }));
    click_fold_option(&ctx, &mut app, "DR 압축", false);
    assert!(app.fold.volume_range.is_none());
    click_fold_option(&ctx, &mut app, "DR 압축", false);
    assert_eq!(app.fold.volume_range, Some(VolumeRange { min: 1, max: 1 }));
    assert_eq!(app.gain_mode, 1);
}

#[test]
/// DR 설정이 사이드바 폭 안에 배치되는지 검사합니다.
fn dr_compression_controls_remain_compact_at_sidebar_widths() {
    let (ctx, mut app) = app();
    for width in [224.0, 260.0, 304.0] {
        for range in [None, Some(VolumeRange { min: 11, max: 15 })] {
            app.fold.volume_range = range;
            let mut height = 0.0;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width + 16.0, 200.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let right = ui.max_rect().right();
                        let controls = ui.vertical(|ui| {
                            assert!(!app.volume_range_options(ui));
                        });
                        height = controls.response.rect.height();
                        assert!(controls.response.rect.right() <= right + 1.0);
                    });
                },
            );
            let text = painted_text(&output);
            assert!(text.iter().any(|(s, _)| s == "DR 압축"));
            assert_eq!(text.iter().any(|(s, _)| s == "최소"), range.is_some());
            assert_eq!(text.iter().any(|(s, _)| s == "최대"), range.is_some());
            assert!(height <= if range.is_some() { 70.0 } else { 34.0 });
            assert_eq!(app.fold.volume_range, range);
        }
    }
}

#[test]
/// 새 악보를 열 때 결과 캐시와 분할 입력을 비우는지 검사합니다.
fn opening_another_score_clears_cached_results_and_split_input() {
    let (ctx, mut app) = app();
    run_job(&mut app, &ctx);
    app.prepare_split();
    run_job(&mut app, &ctx);
    app.follow_playhead = false;
    app.accept_source(source_with_six_tracks());
    assert!(app.result.is_none());
    assert!(app.fold_result.is_none());
    assert!(app.split_result.is_none());
    assert!(app.folded_source.is_none());
    assert!(!app.split_uses_folded);
    assert!(app.follow_playhead);
    assert_eq!(app.input_source().unwrap().score.tracks.len(), 6);
}

#[test]
/// 원본 미리듣기 정보가 2트랙 결과처럼 표시되지 않는지 검사합니다.
fn source_preview_metadata_is_not_presented_as_the_two_track_output() {
    let (ctx, mut app) = app();
    run_job(&mut app, &ctx);
    assert_eq!(app.displayed_result().unwrap().score.tracks.len(), 2);
    app.show_original = true;
    assert!(app.displayed_result().is_none());
    assert_eq!(app.input_source().unwrap().score.tracks.len(), 6);
    app.tab = Tab::Mml;
    assert_eq!(app.displayed_result().unwrap().score.tracks.len(), 2);
}

#[test]
/// 노트롤 장 선택이 재생 탐색 없이 파일 정보를 바꾸는지 검사합니다.
fn selecting_piano_scroll_changes_the_file_details_without_seeking_playback() {
    let (ctx, mut app) = app();
    run_job(&mut app, &ctx);
    app.prepare_split();
    run_job(&mut app, &ctx);
    app.sync_playback();
    app.player.seek(1.0);
    let playback = app.player.snapshot();
    let timeline = app.timeline.clone().unwrap();
    let first_parts = app.result.as_ref().unwrap().artifacts[0].game_parts.clone();

    app.select_split(1);

    let result = app.result.as_ref().unwrap();
    let selected_file = &result.artifacts[app.selected_artifact];
    assert!(selected_file.name.ends_with("_02.mmi"));
    assert_ne!(selected_file.game_parts, first_parts);
    assert_eq!(app.selected_split, 1);
    let chunk = &result.split.as_ref().unwrap().chunks[1];
    assert_eq!(
        app.start_tick,
        chunk
            .start
            .min((result.score.total_ticks() - app.beats * 96).max(0))
    );
    assert!(!app.follow_playhead);
    assert!(Arc::ptr_eq(app.timeline.as_ref().unwrap(), &timeline));
    assert_eq!(
        app.player.snapshot().position_seconds,
        playback.position_seconds
    );
    assert_eq!(app.player.snapshot().state, playback.state);
}

#[test]
/// 출력 파일의 장 선택이 노트롤 구간을 옮기는지 검사합니다.
fn output_scroll_selection_moves_the_piano() {
    let (ctx, mut app) = app();
    run_job(&mut app, &ctx);
    app.prepare_split();
    run_job(&mut app, &ctx);
    let artifacts = &app.result.as_ref().unwrap().artifacts;
    let second_score = artifacts
        .iter()
        .position(|a| a.name.ends_with("_02.mmi"))
        .unwrap();

    app.select_artifact(second_score);
    assert_eq!(app.selected_split, 1);
    assert_eq!(app.selected_artifact, second_score);

    app.select_split(0);
    assert_eq!(app.selected_split, 0);
    assert!(
        app.result.as_ref().unwrap().artifacts[app.selected_artifact]
            .name
            .ends_with("_01.mmi")
    );
}

#[test]
/// 미리듣기 선택이 분할 위치와 저장 악보를 보존하는지 검사합니다.
fn preview_sound_selections_preserve_split_position_and_exported_scores() {
    let (ctx, mut app) = app();
    run_job(&mut app, &ctx);
    app.prepare_split();
    run_job(&mut app, &ctx);
    app.sync_playback();
    let texts = app
        .result
        .as_ref()
        .unwrap()
        .artifacts
        .iter()
        .map(|a| a.text.clone())
        .collect::<Vec<_>>();
    let timeline = app.timeline.clone().unwrap();
    let revision = app.score_revision;
    app.player.seek(1.0);
    app.start_tick = 96;
    app.follow_playhead = true;
    let beats = app.beats;
    app.player.set_part_instrument(0, 0, Instrument::Flute);
    app.player.set_part_instrument(1, 2, Instrument::Cello);
    assert_eq!(app.start_tick, 96);
    assert_eq!(app.beats, beats);
    assert!(app.follow_playhead);
    app.player.set_part_enabled(0, 1, false);
    app.player.set_track_enabled(0, false);
    app.player.set_track_enabled(0, true);
    app.player.set_track_enabled(1, false);
    app.select_split(1);
    app.tab = Tab::Mml;
    app.sync_playback();
    app.tab = Tab::Preview;
    app.sync_playback();

    assert!(app.player.track_enabled(0));
    assert!(!app.player.part_enabled(0, 1));
    assert!(!app.player.track_enabled(1));
    assert!(app.player.part_enabled(1, 0));
    assert_eq!(app.player.part_instrument(0, 0), Instrument::Flute);
    assert_eq!(app.player.part_instrument(1, 2), Instrument::Cello);
    assert_eq!(app.player.part_instrument(0, 1), Instrument::Piano);
    assert_eq!(app.player.snapshot().position_seconds, 1.0);
    assert_eq!(app.player.snapshot().state, PlaybackState::Stopped);
    assert_eq!(app.score_revision, revision);
    assert!(Arc::ptr_eq(app.timeline.as_ref().unwrap(), &timeline));
    assert_eq!(
        app.result
            .as_ref()
            .unwrap()
            .artifacts
            .iter()
            .map(|a| a.text.clone())
            .collect::<Vec<_>>(),
        texts
    );
    app.player.stop();
    app.sync_playback();
    assert!(!app.player.part_enabled(0, 1));
    assert!(!app.player.track_enabled(1));
    assert_eq!(app.player.part_instrument(0, 0), Instrument::Flute);
    assert_eq!(app.player.part_instrument(1, 2), Instrument::Cello);
}

#[test]
/// 원본·결과 전환 시 해당 악보의 미리듣기 선택을 초기화하는지 검사합니다.
fn switching_between_original_and_result_resets_sound_selections_for_that_score() {
    let (ctx, mut app) = app();
    app.sync_playback();
    app.player.set_track_enabled(5, false);
    app.player.set_part_enabled(0, 2, false);
    app.player.set_part_instrument(0, 2, Instrument::Violin);
    run_job(&mut app, &ctx);
    app.sync_playback();
    for ti in 0..2 {
        assert!(app.player.track_enabled(ti));
        for pi in 0..3 {
            assert!(app.player.part_enabled(ti, pi));
            assert_eq!(app.player.part_instrument(ti, pi), Instrument::Piano);
        }
    }
    app.player.set_track_enabled(0, false);
    app.player.set_part_instrument(0, 0, Instrument::Flute);
    app.show_original = true;
    app.sync_playback();
    for ti in 0..6 {
        assert!(app.player.track_enabled(ti));
        for pi in 0..3 {
            assert!(app.player.part_enabled(ti, pi));
            assert_eq!(app.player.part_instrument(ti, pi), Instrument::Piano);
        }
    }
    app.player.set_part_enabled(1, 1, false);
    app.player.set_part_instrument(1, 1, Instrument::Trumpet);
    app.show_original = false;
    app.sync_playback();
    assert!(app.player.track_enabled(0));
    assert!(app.player.part_enabled(1, 1));
    assert_eq!(app.player.part_instrument(1, 1), Instrument::Piano);
}

#[test]
/// 좁은 미리듣기 영역에서 악기 조작부가 줄바꿈되는지 검사합니다.
fn instrument_controls_wrap_within_the_preview_at_narrow_widths() {
    let (ctx, mut app) = app();
    app.sync_playback();
    let score = app.source.as_ref().unwrap().score.clone();
    app.player
        .set_part_instrument(0, 0, Instrument::ElectricPiano);
    for width in [600.0, 900.0, 1400.0] {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 1200.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let right = ui.max_rect().right();
                    track_controls(ui, &mut app.player, &score, None);
                    assert!(
                        ui.min_rect().right() <= right + 1.0,
                        "instrument controls overflow a {width}px preview"
                    );
                });
            },
        );
    }
}
