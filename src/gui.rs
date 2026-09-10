//! 악보 편곡·분할·미리듣기의 화면과 사용자 조작을 관리한다.

use eframe::egui::{self, Color32, FontId, RichText, Stroke};
#[cfg(test)]
use mmlfold::core;
use mmlfold::{
    Score,
    fold::{FoldOptions, Gain, Layout, VolumeRange},
    input,
    instruments::Instrument,
    playback::{PlaybackState, Player},
    split::SplitOptions,
    synth::Timeline,
    verify,
    workflow::{self, WorkKind, WorkResult},
};
use std::{
    borrow::Cow,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Duration,
};

const BG: Color32 = Color32::from_rgb(255, 255, 255);
const PANEL: Color32 = Color32::from_rgb(250, 250, 250);
const BORDER: Color32 = Color32::from_rgb(228, 228, 231);
const INK: Color32 = Color32::from_rgb(24, 24, 27);
const MUTED: Color32 = Color32::from_rgb(113, 113, 122);
const TRACK_COLORS: [Color32; 16] = [
    Color32::from_rgb(37, 99, 235),
    Color32::from_rgb(234, 88, 12),
    Color32::from_rgb(13, 148, 136),
    Color32::from_rgb(147, 51, 234),
    Color32::from_rgb(220, 38, 38),
    Color32::from_rgb(161, 120, 0),
    Color32::from_rgb(2, 132, 199),
    Color32::from_rgb(219, 39, 119),
    Color32::from_rgb(77, 124, 15),
    Color32::from_rgb(79, 70, 229),
    Color32::from_rgb(146, 64, 14),
    Color32::from_rgb(8, 145, 178),
    Color32::from_rgb(190, 24, 93),
    Color32::from_rgb(21, 128, 61),
    Color32::from_rgb(134, 25, 143),
    Color32::from_rgb(71, 85, 105),
];

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Fold,
    Split,
}
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Preview,
    Mml,
}

struct Loaded {
    path: PathBuf,
    score: Arc<Score>,
    notes: usize,
    polyphony: usize,
}

enum JobDone {
    Loaded(Loaded),
    Worked(WorkResult),
    Saved(PathBuf),
}
type JobMessage = Result<JobDone, String>;

pub struct MmlApp {
    app_icon: egui::TextureHandle,
    source: Option<Loaded>,
    mode: Mode,
    fold: FoldOptions,
    split: SplitOptions,
    gain_mode: usize,
    gain_shift: i32,
    remembered_volume_range: VolumeRange,
    track_files: bool,
    result: Option<WorkResult>,
    fold_result: Option<WorkResult>,
    split_result: Option<WorkResult>,
    folded_source: Option<Loaded>,
    split_uses_folded: bool,
    result_polyphony: usize,
    result_notes: usize,
    receiver: Option<mpsc::Receiver<JobMessage>>,
    status: String,
    error: Option<String>,
    export_path: Option<PathBuf>,
    tab: Tab,
    show_original: bool,
    start_tick: i64,
    beats: i64,
    selected_artifact: usize,
    selected_split: usize,
    initial: Option<PathBuf>,
    player: Player,
    timeline: Option<Arc<Timeline>>,
    score_revision: u64,
    playback_revision: Option<(u64, bool)>,
    follow_playhead: bool,
    volume_percent: f32,
    zoom_target_beats: f64,
}

impl MmlApp {
    /// 생성 컨텍스트와 초기 파일 경로로 앱을 만든다.
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        Self::with_context(&cc.egui_ctx, initial)
    }

    /// 화면 설정과 공유 아이콘 텍스처를 초기화한다.
    fn with_context(ctx: &egui::Context, initial: Option<PathBuf>) -> Self {
        configure(ctx);
        let icon = crate::window_icon();
        let app_icon = ctx.load_texture(
            "app-icon",
            egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            ),
            egui::TextureOptions::LINEAR,
        );
        Self {
            app_icon,
            source: None,
            mode: Mode::Fold,
            fold: FoldOptions {
                layout: Layout::Learned,
                ..FoldOptions::default()
            },
            split: SplitOptions::default(),
            gain_mode: 0,
            gain_shift: 0,
            remembered_volume_range: VolumeRange { min: 11, max: 15 },
            track_files: false,
            result: None,
            fold_result: None,
            split_result: None,
            folded_source: None,
            split_uses_folded: false,
            result_polyphony: 0,
            result_notes: 0,
            receiver: None,
            status: String::new(),
            error: None,
            export_path: None,
            tab: Tab::Preview,
            show_original: false,
            start_tick: 0,
            beats: 32,
            selected_artifact: 0,
            selected_split: 0,
            initial,
            player: Player::default(),
            timeline: None,
            score_revision: 0,
            playback_revision: None,
            follow_playhead: true,
            volume_percent: 65.0,
            zoom_target_beats: 32.0,
        }
    }

    /// 백그라운드 작업이 진행 중인지 확인한다.
    fn busy(&self) -> bool {
        self.receiver.is_some()
    }

    /// 현재 모드의 작업 대상 원본 또는 편곡 결과를 반환한다.
    fn input_source(&self) -> Option<&Loaded> {
        if self.mode == Mode::Split && self.split_uses_folded {
            self.folded_source.as_ref()
        } else {
            self.source.as_ref()
        }
    }

    /// 현재 화면에 표시할 결과를 반환한다.
    fn displayed_result(&self) -> Option<&WorkResult> {
        self.result
            .as_ref()
            .filter(|_| self.tab != Tab::Preview || !self.show_original)
    }

    /// 저장 가능한 결과나 MIDI 원본이 있는지 확인한다.
    fn can_save(&self) -> bool {
        self.result.is_some()
            || self
                .input_source()
                .is_some_and(|source| input::is_midi_path(&source.path))
    }

    /// 기존 결과를 빌리거나 MIDI 원본의 MMI 저장 내용을 준비한다.
    fn export_result(&self) -> anyhow::Result<Option<Cow<'_, WorkResult>>> {
        if let Some(result) = &self.result {
            return Ok(Some(Cow::Borrowed(result)));
        }
        self.input_source()
            .filter(|source| input::is_midi_path(&source.path))
            .map(|source| workflow::source_export(&source.score, &self.stem()).map(Cow::Owned))
            .transpose()
    }

    /// 결과를 교체하고 선택·미리듣기 상태를 함께 초기화한다.
    fn apply_result(&mut self, result: Option<WorkResult>) {
        self.player.stop();
        self.score_revision = self.score_revision.wrapping_add(1);
        self.result_notes = 0;
        self.result_polyphony = 0;
        if let Some(result) = &result {
            let notes = result.score.all_notes();
            self.result_notes = notes.len();
            self.result_polyphony = verify::max_polyphony(&notes);
            if let Some(chunk) = result.split.as_ref().and_then(|s| s.chunks.first()) {
                self.beats = ((chunk.end - chunk.start + 95) / 96).clamp(4, 256);
                self.zoom_target_beats = self.beats as f64;
            }
        }
        self.result = result;
        self.export_path = None;
        self.show_original = false;
        self.selected_artifact = 0;
        self.selected_split = 0;
        self.start_tick = 0;
        self.follow_playhead = true;
        self.tab = Tab::Preview;
    }

    /// 모드별 결과를 복원하며 작업 화면을 전환한다.
    fn set_mode(&mut self, mode: Mode) {
        if self.busy() || self.mode == mode {
            return;
        }
        self.mode = mode;
        let result = match mode {
            Mode::Fold => self.fold_result.clone(),
            Mode::Split => self.split_result.clone(),
        };
        self.apply_result(result);
        self.status.clear();
    }

    /// 새 원본을 받아 이전 편곡·분할 결과를 초기화한다.
    fn accept_source(&mut self, source: Loaded) {
        self.status.clear();
        self.source = Some(source);
        self.fold_result = None;
        self.split_result = None;
        self.folded_source = None;
        self.split_uses_folded = false;
        self.apply_result(None);
    }

    /// 완료된 작업을 캐시하고 현재 모드에 맞는 결과를 표시한다.
    fn complete_result(&mut self, result: WorkResult) {
        let completed_mode = match result.kind {
            WorkKind::Fold => {
                let notes = result.score.all_notes();
                self.folded_source = Some(Loaded {
                    path: PathBuf::from(format!("{}_folded.mmi", self.stem())),
                    notes: notes.len(),
                    polyphony: verify::max_polyphony(&notes),
                    score: Arc::new(result.score.clone()),
                });
                self.fold_result = Some(result.clone());
                // 새 편곡을 분할 대상으로 삼고, 이전 결과로 만든 장은 폐기한다.
                self.split_result = None;
                self.split_uses_folded = true;
                Mode::Fold
            }
            WorkKind::Split => {
                self.split_result = Some(result.clone());
                Mode::Split
            }
            WorkKind::Import | WorkKind::Verify => return,
        };
        let artifact_count = result.artifacts.len();
        if self.mode == completed_mode {
            self.apply_result(Some(result));
        } else if self.mode == Mode::Split && completed_mode == Mode::Fold {
            self.apply_result(None);
        }
        self.status = format!("작업 완료 · {artifact_count}개 파일");
    }

    /// 분할 대상을 바꾸고 이전 분할 결과를 무효화한다.
    fn set_split_source(&mut self, folded: bool) {
        if self.busy()
            || self.split_uses_folded == folded
            || (folded && self.folded_source.is_none())
        {
            return;
        }
        self.split_uses_folded = folded;
        self.invalidate_split();
        self.status = "분할 대상이 변경되었습니다. 분할을 다시 실행하세요.".into();
    }

    /// 현재 편곡 결과를 대상으로 분할 화면을 연다.
    fn prepare_split(&mut self) {
        if self.busy() || self.folded_source.is_none() {
            return;
        }
        self.set_split_source(true);
        self.set_mode(Mode::Split);
    }

    /// 편곡 설정 변경에 따라 결과와 파생 분할을 무효화한다.
    fn invalidate_fold(&mut self) {
        let had_result = self.fold_result.is_some();
        self.fold_result = None;
        self.folded_source = None;
        if self.split_uses_folded {
            self.split_uses_folded = false;
            self.invalidate_split();
        }
        if self.mode == Mode::Fold {
            self.apply_result(None);
        }
        if had_result {
            self.status = "편곡 설정이 변경되었습니다. 편곡을 다시 실행하세요.".into();
        }
    }

    /// 분할 설정 변경에 따라 분할 결과를 무효화한다.
    fn invalidate_split(&mut self) {
        self.split_result = None;
        if self.mode == Mode::Split {
            self.apply_result(None);
        }
    }

    /// 악보의 장 선택과 출력 파일 선택을 동기화한다.
    fn select_split(&mut self, selected: usize) {
        let Some(result) = self.result.as_ref().filter(|r| r.kind == WorkKind::Split) else {
            return;
        };
        let Some(chunk) = result.split.as_ref().and_then(|s| s.chunks.get(selected)) else {
            return;
        };
        let Some((artifact_index, _)) = result
            .artifacts
            .iter()
            .enumerate()
            .filter(|(_, a)| a.name.ends_with(".mmi") && !a.game_parts.is_empty())
            .nth(selected)
        else {
            return;
        };
        self.selected_split = selected;
        self.selected_artifact = artifact_index;
        self.beats = ((chunk.end - chunk.start + 95) / 96).clamp(4, 256);
        self.zoom_target_beats = self.beats as f64;
        self.start_tick = chunk
            .start
            .min((result.score.total_ticks() - self.beats * 96).max(0));
        self.follow_playhead = false;
    }

    /// 출력 파일 선택을 해당 악보의 장에 반영한다.
    fn select_artifact(&mut self, selected: usize) {
        let Some(result) = &self.result else {
            return;
        };
        if selected >= result.artifacts.len() {
            return;
        }
        self.selected_artifact = selected;
        if result.kind == WorkKind::Split {
            let scroll = result
                .artifacts
                .iter()
                .enumerate()
                .filter(|(_, a)| a.name.ends_with(".mmi") && !a.game_parts.is_empty())
                .position(|(index, _)| index == selected);
            if let Some(scroll) = scroll {
                self.select_split(scroll);
            }
        }
    }

    /// 현재 표시 악보가 바뀌었으면 미리듣기 타임라인을 교체한다.
    fn sync_playback(&mut self) {
        let revision = (self.score_revision, self.show_original);
        if self.playback_revision == Some(revision) {
            return;
        }
        self.playback_revision = Some(revision);
        self.player.stop();
        self.timeline = None;
        self.start_tick = 0;
        self.follow_playhead = true;
        let score = if self.show_original {
            self.input_source().map(|s| s.score.as_ref())
        } else {
            self.result
                .as_ref()
                .map(|r| &r.score)
                .or_else(|| self.input_source().map(|s| s.score.as_ref()))
        };
        if let Some(score) = score {
            match Timeline::from_score(score) {
                Ok(timeline) => {
                    let timeline = Arc::new(timeline);
                    self.player.load(timeline.clone());
                    self.timeline = Some(timeline);
                }
                Err(error) => self.error = Some(format!("재생 준비 실패: {error:#}")),
            }
        }
    }

    /// 현재 상태에 따라 재생하거나 일시정지한다.
    fn toggle_playback(&mut self) {
        if self.player.snapshot().state == PlaybackState::Playing {
            self.player.pause();
        } else if self
            .timeline
            .as_ref()
            .is_some_and(|t| t.duration_seconds() > 0.0)
            && let Err(error) = self.player.play()
        {
            self.error = Some(format!("재생할 수 없습니다: {error:#}"));
        }
    }

    /// 재생 오류와 커서 추적을 화면에 반영한다.
    fn update_playback(&mut self, ctx: &egui::Context) {
        self.sync_playback();
        if let Some(error) = self.player.take_error() {
            self.error = Some(error);
        }
        let snapshot = self.player.snapshot();
        if snapshot.state == PlaybackState::Playing {
            ctx.request_repaint_after(Duration::from_millis(30));
            if self.follow_playhead
                && let Some(timeline) = &self.timeline
            {
                let tick = timeline.seconds_to_tick(snapshot.position_seconds) as i64;
                let span = self.beats * 96;
                if tick < self.start_tick || tick >= self.start_tick + span {
                    self.start_tick =
                        ((tick / span) * span).min((timeline.total_ticks() - span).max(0));
                }
            }
        }
    }

    /// 백그라운드 작업을 시작하고 완료 시 화면 갱신을 요청한다.
    fn start_job(
        &mut self,
        ctx: &egui::Context,
        status: &str,
        job: impl FnOnce() -> anyhow::Result<JobDone> + Send + 'static,
    ) {
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        self.status = status.into();
        self.error = None;
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job))
                .map_err(|_| {
                    "작업 중 내부 오류가 발생했습니다. 입력 악보와 옵션을 확인하세요.".to_owned()
                })
                .and_then(|r| r.map_err(|e| format!("{e:#}")));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    /// 선택한 악보 파일을 백그라운드에서 읽는다.
    fn load(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.busy() {
            return;
        }
        self.player.stop();
        self.start_job(ctx, "악보를 읽고 있습니다…", move || {
            let score = input::load_score(&path)?;
            let notes = score.all_notes();
            let loaded = Loaded {
                path,
                notes: notes.len(),
                polyphony: verify::max_polyphony(&notes),
                score: Arc::new(score),
            };
            Ok(JobDone::Loaded(loaded))
        });
    }

    /// 완료된 파일 읽기·변환·저장 작업을 화면 상태에 반영한다.
    fn poll(&mut self) {
        let message = self.receiver.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(v) => Some(v),
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("작업 연결이 종료되었습니다.".into()))
            }
            Err(mpsc::TryRecvError::Empty) => None,
        });
        if let Some(message) = message {
            self.receiver = None;
            match message {
                Ok(JobDone::Loaded(source)) => {
                    self.accept_source(source);
                }
                Ok(JobDone::Worked(result)) => {
                    self.complete_result(result);
                }
                Ok(JobDone::Saved(path)) => {
                    self.status = format!("저장 완료 · {}", path.display());
                    self.export_path = Some(path);
                }
                Err(error) => {
                    self.status = "작업을 완료하지 못했습니다".into();
                    self.error = Some(error);
                }
            }
        }
    }

    /// 현재 파일의 확장자를 제외한 이름을 반환한다.
    fn stem(&self) -> String {
        self.source
            .as_ref()
            .and_then(|s| s.path.file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or("score")
            .to_owned()
    }

    /// 선택한 입력과 설정으로 편곡 또는 분할을 실행한다.
    fn run(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let Some(source) = self.input_source() else {
            return;
        };
        let score = source.score.clone();
        let stem = self.stem();
        match self.mode {
            Mode::Fold => {
                let mut options = self.fold.clone();
                options.gain = match self.gain_mode {
                    0 => Gain::Auto,
                    1 => Gain::None,
                    _ => Gain::Shift(self.gain_shift),
                };
                let track_files = self.track_files;
                self.start_job(
                    ctx,
                    "성부를 분석하고 편곡하고 있습니다…",
                    move || {
                        Ok(JobDone::Worked(workflow::arrange(
                            &score,
                            &options,
                            &stem,
                            track_files,
                        )?))
                    },
                );
            }
            Mode::Split => {
                let options = self.split.clone();
                self.start_job(
                    ctx,
                    "파트별 글자 수를 확인하며 시간축으로 분할하고 있습니다…",
                    move || Ok(JobDone::Worked(workflow::scrolls(&score, &options, &stem)?)),
                );
            }
        }
    }

    /// 표시 결과를 선택한 폴더 아래에 저장한다.
    fn save(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let result = match self.export_result() {
            Ok(Some(result)) => result.into_owned(),
            Ok(None) => return,
            Err(error) => {
                self.error = Some(format!("MMI 저장 준비 실패: {error:#}"));
                return;
            }
        };
        if let Some(parent) = rfd::FileDialog::new()
            .set_title("결과를 저장할 상위 폴더 선택")
            .pick_folder()
        {
            let stem = self.stem();
            self.start_job(
                ctx,
                "결과 파일을 저장하고 있습니다…",
                move || {
                    Ok(JobDone::Saved(workflow::export_new_folder(
                        &result, &parent, &stem,
                    )?))
                },
            );
        }
    }

    /// 공유 아이콘·파일명·모드 전환을 상단에 표시한다.
    fn toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("toolbar")
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(24, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Image::new(&self.app_icon).fit_to_exact_size(egui::vec2(22.0, 22.0)),
                    );
                    ui.add_space(3.0);
                    ui.label(RichText::new("MML 세단기").size(16.0).strong());
                    if let Some(source) = &self.source {
                        ui.add_space(14.0);
                        ui.add_sized(
                            [(ui.available_width() - 120.0).max(0.0), 30.0],
                            egui::Label::new(
                                RichText::new(
                                    source
                                        .path
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy(),
                                )
                                .size(17.0)
                                .color(MUTED),
                            )
                            .truncate()
                            .halign(egui::Align::Min),
                        )
                        .on_hover_text(source.path.display().to_string());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                !self.busy(),
                                egui::Button::new("파일 열기…").min_size(egui::vec2(96.0, 30.0)),
                            )
                            .on_hover_text("Ctrl+O")
                            .clicked()
                        {
                            self.choose_file(ctx);
                        }
                    });
                });
            });
        egui::TopBottomPanel::top("mode-tabs")
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(24, 0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 20.0;
                    ui.add_enabled_ui(!self.busy(), |ui| {
                        for (mode, label) in [(Mode::Fold, "편곡"), (Mode::Split, "악보 분할")]
                        {
                            if nav_tab(ui, label, self.mode == mode).clicked() {
                                self.set_mode(mode);
                            }
                        }
                    });
                });
            });
    }

    /// 지원 악보를 고르는 파일 대화상자를 연다.
    fn choose_file(&mut self, ctx: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("악보 파일", &["mmi", "mid", "midi"])
            .pick_file()
        {
            self.load(ctx, path);
        }
    }

    /// 현재 모드의 설정과 실행 버튼을 배치한다.
    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("options")
            .exact_width(304.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(22))
            .show(ctx, |ui| {
                let available = ui.available_height();
                egui::ScrollArea::vertical()
                    .max_height((available - 68.0).max(100.0))
                    .show(ui, |ui| {
                        ui.add_enabled_ui(!self.busy(), |ui| {
                            ui.label(
                                RichText::new(match self.mode {
                                    Mode::Fold => "편곡 설정",
                                    Mode::Split => "분할 설정",
                                })
                                .size(17.0)
                                .strong(),
                            );
                            ui.add_space(20.0);
                            match self.mode {
                                Mode::Fold => self.fold_options(ui),
                                Mode::Split => self.split_options(ui),
                            }
                        });
                    });
                ui.add_space((ui.available_height() - 54.0).max(16.0));
                ui.separator();
                let ready = !self.busy() && self.input_source().is_some();
                let label = match self.mode {
                    Mode::Fold => "편곡 실행",
                    Mode::Split => "분할 실행",
                };
                if ui
                    .add_enabled(
                        ready,
                        egui::Button::new(RichText::new(label).color(Color32::WHITE))
                            .fill(INK)
                            .min_size(egui::vec2(ui.available_width(), 36.0)),
                    )
                    .clicked()
                {
                    self.run(ctx);
                }
            });
    }

    /// 편곡 설정을 편집하고 변경된 결과를 무효화한다.
    fn fold_options(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        ui.columns(2, |columns| {
            section(&mut columns[0], "트랙 수");
            changed |= columns[0]
                .add_sized(
                    [108.0, 32.0],
                    egui::DragValue::new(&mut self.fold.tracks).range(1..=16),
                )
                .changed();
            section(&mut columns[1], "트랙당 파트");
            changed |= columns[1]
                .add_sized(
                    [108.0, 32.0],
                    egui::DragValue::new(&mut self.fold.parts).range(1..=3),
                )
                .changed();
        });
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "최대 동시 {}음",
                self.fold.tracks * self.fold.parts
            ))
            .size(14.0)
            .color(MUTED),
        );
        ui.add_space(20.0);
        section(ui, "성부 배치");
        egui::ComboBox::from_id_salt("layout")
            .selected_text(match self.fold.layout {
                Layout::Hands => "손별 선율",
                Layout::Voices => "원본 성부 유지",
                Layout::Roles => "주선율 · 속성부 · 베이스",
                Layout::Learned => "핵심 성부 우선",
            })
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(&mut self.fold.layout, Layout::Learned, "핵심 성부 우선")
                    .changed();
                changed |= ui
                    .selectable_value(&mut self.fold.layout, Layout::Hands, "손별 선율")
                    .changed();
                changed |= ui
                    .selectable_value(&mut self.fold.layout, Layout::Voices, "원본 성부 유지")
                    .changed();
                changed |= ui
                    .selectable_value(
                        &mut self.fold.layout,
                        Layout::Roles,
                        "주선율 · 속성부 · 베이스",
                    )
                    .changed();
            });
        ui.add_space(20.0);
        section(ui, "음량");
        egui::ComboBox::from_id_salt("gain")
            .selected_text(["자동 (최고 음량 v15)", "원본 유지", "직접 조정"][self.gain_mode])
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(&mut self.gain_mode, 0, "자동 (최고 음량 v15)")
                    .changed();
                changed |= ui
                    .selectable_value(&mut self.gain_mode, 1, "원본 유지")
                    .changed();
                changed |= ui
                    .selectable_value(&mut self.gain_mode, 2, "직접 조정")
                    .changed();
            });
        if self.gain_mode == 2 {
            changed |= ui
                .add(egui::Slider::new(&mut self.gain_shift, -15..=15).text("가산"))
                .changed();
        }
        ui.add_space(8.0);
        changed |= self.volume_range_options(ui);
        ui.add_space(22.0);
        ui.separator();
        ui.add_space(8.0);
        changed |= ui
            .checkbox(&mut self.track_files, "트랙별 파일도 만들기")
            .changed();
        ui.add_space(14.0);
        if matches!(
            self.fold.layout,
            Layout::Hands | Layout::Roles | Layout::Learned
        ) {
            ui.collapsing("고급 설정", |ui| {
                ui.add_space(8.0);
                section(ui, "높은 음 선호도");
                changed |= ui
                    .add(egui::Slider::new(&mut self.fold.lead_pitch, 0.0..=2.0).step_by(0.05))
                    .changed();
                ui.add_space(10.0);
                section(ui, "선율의 도약 벌점");
                changed |= ui
                    .add(egui::Slider::new(&mut self.fold.lead_continuity, 0.0..=3.0).step_by(0.05))
                    .changed();
            });
        }
        if changed {
            self.invalidate_fold();
        }
    }

    /// DR 압축 범위를 편집하고 실제 설정 변경 여부를 반환한다.
    fn volume_range_options(&mut self, ui: &mut egui::Ui) -> bool {
        let previous = self.fold.volume_range;
        let mut enabled = self.fold.volume_range.is_some();
        if let Some(range) = self.fold.volume_range {
            self.remembered_volume_range = range;
        }
        let mut changed = ui
            .checkbox(&mut enabled, "DR 압축")
            .on_hover_text("이동 후 1~15를 선택 범위로 압축")
            .changed();
        if enabled {
            ui.horizontal(|ui| {
                ui.spacing_mut().button_padding = egui::vec2(8.0, 4.0);
                ui.spacing_mut().interact_size.y = 28.0;
                ui.label("최소");
                let max = self.remembered_volume_range.max;
                changed |= ui
                    .add_sized(
                        [52.0, 28.0],
                        egui::DragValue::new(&mut self.remembered_volume_range.min).range(1..=max),
                    )
                    .changed();
                ui.label("최대");
                let min = self.remembered_volume_range.min;
                changed |= ui
                    .add_sized(
                        [52.0, 28.0],
                        egui::DragValue::new(&mut self.remembered_volume_range.max).range(min..=15),
                    )
                    .changed();
            });
        }
        if changed {
            self.fold.volume_range = enabled.then_some(self.remembered_volume_range);
        }
        self.fold.volume_range != previous
    }

    /// 분할 대상과 글자 수 제한을 편집한다.
    fn split_options(&mut self, ui: &mut egui::Ui) {
        section(ui, "분할 대상");
        let mut folded = self.split_uses_folded;
        egui::ComboBox::from_id_salt("split-source")
            .selected_text(if folded {
                "편곡 결과"
            } else {
                "원본 악보"
            })
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                if self.folded_source.is_some() {
                    ui.selectable_value(&mut folded, true, "편곡 결과");
                }
                ui.selectable_value(&mut folded, false, "원본 악보");
            });
        if folded != self.split_uses_folded {
            self.set_split_source(folded);
        }
        if let Some(source) = self.input_source() {
            ui.label(
                RichText::new(format!(
                    "{}트랙 · {}파트",
                    source.score.tracks.len(),
                    source
                        .score
                        .tracks
                        .iter()
                        .map(|t| t.parts.len())
                        .sum::<usize>()
                ))
                .size(14.0)
                .color(MUTED),
            );
        }
        ui.add_space(20.0);
        let mut changed = false;
        section(ui, "파트당 글자 수 한도");
        changed |= ui
            .add(
                egui::DragValue::new(&mut self.split.limit)
                    .range(16..=2400)
                    .speed(10.0)
                    .suffix(" 자"),
            )
            .changed();
        ui.add_space(20.0);
        section(ui, "쉼표 기준");
        ui.label("인정할 최소 쉼표");
        changed |= ui
            .add(
                egui::DragValue::new(&mut self.split.min_gap)
                    .range(1..=1536)
                    .suffix(" 틱"),
            )
            .changed();
        ui.add_space(8.0);
        ui.label("일찍 끊을 수 있는 긴 쉼표");
        changed |= ui
            .add(
                egui::DragValue::new(&mut self.split.big_gap)
                    .range(1..=6144)
                    .suffix(" 틱"),
            )
            .changed();
        if changed {
            let had_result = self.split_result.is_some();
            self.invalidate_split();
            if had_result {
                self.status = "분할 설정이 변경되었습니다. 분할을 다시 실행하세요.".into();
            }
        }
    }

    /// 결과 요약과 선택한 탭의 내용을 표시한다.
    fn content(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG).inner_margin(22)).show(ctx, |ui| {
            if self.source.is_none() {
                ui.add_space((ui.available_height() * 0.26).max(30.0));
                ui.vertical_centered(|ui| {
                    if ui.add_enabled(!self.busy(), egui::Button::new("파일 열기…").min_size(egui::vec2(116.0, 34.0))).clicked() { self.choose_file(ctx); }
                });
                return;
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new(match self.displayed_result().map(|r| r.kind) { Some(WorkKind::Fold) => "편곡 결과", Some(WorkKind::Split) => "분할 결과", _ if self.mode == Mode::Split => "분할 대상 미리보기", _ => "원본 악보 미리보기" }).size(20.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.can_save() {
                        if ui.add_enabled(!self.busy(), egui::Button::new("MMI 저장…")).clicked() { self.save(ctx); }
                        if self.result.as_ref().is_some_and(|r| r.kind == WorkKind::Fold)
                            && ui.add_enabled(!self.busy(), egui::Button::new("이 결과 분할")).clicked() {
                                self.prepare_split();
                        }
                    }
                });
            });
            ui.add_space(8.0);
            if let Some(source) = self.input_source() {
                let result = self.displayed_result();
                let shown = result.map(|r| &r.score).unwrap_or(source.score.as_ref());
                let count = result.map(|_| self.result_notes).unwrap_or(source.notes);
                let poly = result.map(|_| self.result_polyphony).unwrap_or(source.polyphony);
                ui.label(RichText::new(format!("{} 트랙   ·   {} 파트   ·   {} 음표   ·   최대 동시 {}음   ·   템포 {}개", shown.tracks.len(), shown.tracks.iter().map(|t| t.parts.len()).sum::<usize>(), number(count), poly, shown.tempos().len())).size(14.0).color(MUTED));
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 20.0;
                for (tab, label) in [(Tab::Preview, "노트롤"), (Tab::Mml, "출력 파일")] {
                    if nav_tab(ui, label, self.tab == tab).clicked() { self.tab = tab; }
                }
            });
            ui.separator();
            ui.add_space(10.0);
            egui::ScrollArea::vertical().id_salt("main-scroll").show(ui, |ui| {
                match self.tab {
                    Tab::Preview => self.preview(ui),
                    Tab::Mml => { ui.add_enabled_ui(!self.busy(), |ui| self.artifacts(ui)); },
                }
            });
        });
    }

    /// 악보·재생 조작·트랙 선택과 스크롤을 표시한다.
    fn preview(&mut self, ui: &mut egui::Ui) {
        if self.source.is_none() {
            return;
        }
        let previous_beats = self.beats;
        ui.horizontal(|ui| {
            ui.label(RichText::new("미리듣기").size(14.0).color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::DragValue::new(&mut self.beats)
                            .range(4..=256)
                            .suffix(" 박"),
                    )
                    .on_hover_text("노트롤 위에서 Ctrl+마우스 휠로 확대·축소")
                    .changed()
                {
                    self.zoom_target_beats = self.beats as f64;
                }
                ui.label(RichText::new("표시 범위").size(14.0).color(MUTED));
                if self.result.is_some() {
                    ui.checkbox(
                        &mut self.show_original,
                        if self.mode == Mode::Split {
                            "분할 전"
                        } else {
                            "원본"
                        },
                    );
                }
            });
        });
        self.sync_playback();
        // Arc만 복제해 악보를 유지한 채 화면 선택 상태를 변경한다.
        let Some(source_score) = self.input_source().map(|s| s.score.clone()) else {
            return;
        };
        let score = if self.show_original {
            source_score.as_ref()
        } else {
            self.result
                .as_ref()
                .map(|r| &r.score)
                .unwrap_or(source_score.as_ref())
        };
        let timeline = self.timeline.clone();
        let snapshot = self.player.snapshot();
        let playhead = timeline
            .as_ref()
            .map(|t| t.seconds_to_tick(snapshot.position_seconds));
        let zoom_playhead =
            playhead.filter(|_| self.follow_playhead && snapshot.state == PlaybackState::Playing);
        if self.beats != previous_beats {
            self.start_tick = zoomed_view_start(
                self.start_tick,
                previous_beats,
                self.beats,
                0.0,
                score.total_ticks(),
                zoom_playhead,
            );
        }
        let max_start = (score.total_ticks() - self.beats * 96).max(0);
        self.start_tick = self.start_tick.min(max_start);
        ui.add_space(8.0);
        let split_preview = self
            .result
            .as_ref()
            .and_then(|r| r.split.as_ref())
            .filter(|_| !self.show_original);
        let interaction = piano_roll(
            ui,
            score,
            self.start_tick,
            self.beats * 96,
            playhead,
            split_preview,
            Some(&self.player),
        );
        if let Some((factor, anchor)) = interaction.zoom {
            self.zoom_target_beats = (self.zoom_target_beats / factor).clamp(4.0, 256.0);
            let new_beats = self.zoom_target_beats.round() as i64;
            self.start_tick = zoomed_view_start(
                self.start_tick,
                self.beats,
                new_beats,
                anchor,
                score.total_ticks(),
                zoom_playhead,
            );
            self.beats = new_beats;
            ui.ctx().request_repaint();
        }
        if let Some(tick) = interaction.seek
            && let Some(timeline) = &timeline
        {
            self.player.seek(timeline.tick_to_seconds(tick));
        }
        if interaction.horizontal != 0.0 {
            self.start_tick = (self.start_tick as f64 + interaction.horizontal)
                .round()
                .clamp(0.0, (score.total_ticks() - self.beats * 96).max(0) as f64)
                as i64;
            self.follow_playhead = false;
            ui.ctx().request_repaint();
        }
        if score_scrollbar(
            ui,
            &mut self.start_tick,
            score.total_ticks(),
            self.beats * 96,
        ) {
            self.follow_playhead = false;
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let can_play = timeline
                .as_ref()
                .is_some_and(|t| t.duration_seconds() > 0.0);
            let playing = snapshot.state == PlaybackState::Playing;
            let play_label = if snapshot.state == PlaybackState::Paused {
                "계속 재생 · Space"
            } else {
                "재생 · Space"
            };
            if transport_button(ui, can_play && !playing, TransportIcon::Play, play_label).clicked()
                && let Err(error) = self.player.play()
            {
                self.error = Some(format!("재생할 수 없습니다: {error:#}"));
            }
            if transport_button(ui, playing, TransportIcon::Pause, "일시정지 · Space").clicked()
            {
                self.player.pause();
            }
            if transport_button(ui, can_play, TransportIcon::Stop, "정지 · 처음으로").clicked()
            {
                self.player.stop();
                self.start_tick = 0;
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "{} / {}",
                    playback_time(snapshot.position_seconds),
                    playback_time(snapshot.total_seconds)
                ))
                .monospace()
                .size(14.0)
                .color(MUTED),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.scope(|ui| {
                    ui.spacing_mut().slider_width = 72.0;
                    ui.spacing_mut().interact_size.y = 18.0;
                    if ui
                        .add(
                            egui::Slider::new(&mut self.volume_percent, 0.0..=100.0)
                                .show_value(false),
                        )
                        .on_hover_text(format!("재생 음량 {:.0}%", self.volume_percent))
                        .changed()
                    {
                        self.player.set_volume(self.volume_percent / 100.0);
                    }
                });
                ui.label(RichText::new("음량").size(14.0).color(MUTED));
                ui.checkbox(&mut self.follow_playhead, "따라가기");
            });
        });
        if let Some(split) = split_preview {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}장", split.chunks.len())).size(14.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("파트당 {}자 이하", number(split.limit)))
                            .size(14.0)
                            .color(MUTED),
                    );
                });
            });
            let selected = split_overview(ui, split, timeline.as_deref(), self.selected_split);
            let shown_split = selected.unwrap_or(self.selected_split);
            if let Some(chunk) = split.chunks.get(shown_split) {
                let range = timeline.as_ref().map_or_else(
                    || format!("{}–{} 틱", chunk.start, chunk.end),
                    |t| {
                        format!(
                            "{}–{}",
                            playback_time(t.tick_to_seconds(chunk.start as f64)),
                            playback_time(t.tick_to_seconds(chunk.end as f64))
                        )
                    },
                );
                ui.label(
                    RichText::new(format!(
                        "{:02}장 · {range} · 파트별 글자 수",
                        shown_split + 1
                    ))
                    .size(14.0)
                    .color(MUTED),
                );
                track_controls(ui, &mut self.player, score, Some(&chunk.part_widths));
            }
            if let Some(selected) = selected {
                self.select_split(selected);
            }
            return;
        }
        ui.add_space(8.0);
        let widths = self
            .result
            .as_ref()
            .filter(|_| !self.show_original)
            .and_then(|r| r.artifacts.first())
            .map(|a| {
                a.game_parts
                    .iter()
                    .map(|parts| parts.iter().map(String::len).collect::<Vec<_>>())
                    .collect::<Vec<_>>()
            });
        track_controls(ui, &mut self.player, score, widths.as_deref());
        if let Some(result) = &self.result {
            ui.add_space(12.0);
            ui.label(
                RichText::new(format!("출력 파일 {}개", result.artifacts.len()))
                    .size(14.0)
                    .color(MUTED),
            );
        }
    }

    /// 저장할 출력 파일과 파트별 정보를 표시한다.
    fn artifacts(&mut self, ui: &mut egui::Ui) {
        let result = match self.export_result() {
            Ok(Some(result)) => result,
            Ok(None) => {
                ui.label(RichText::new("출력 파일 없음").color(MUTED));
                return;
            }
            Err(error) => {
                ui.colored_label(Color32::from_rgb(185, 28, 28), format!("{error:#}"));
                return;
            }
        };
        if result.artifacts.is_empty() {
            return;
        }
        let mut selected = self.selected_artifact.min(result.artifacts.len() - 1);
        egui::ComboBox::from_id_salt("artifact")
            .selected_text(&result.artifacts[selected].name)
            .width(360.0)
            .show_ui(ui, |ui| {
                for (i, artifact) in result.artifacts.iter().enumerate() {
                    ui.selectable_value(&mut selected, i, &artifact.name);
                }
            });
        ui.add_space(14.0);
        let artifact = &result.artifacts[selected];
        if !artifact.game_parts.is_empty() {
            ui.label(
                RichText::new(format!(
                    "{}트랙 · {}파트 · 파트별 글자 수",
                    artifact.game_parts.len(),
                    artifact.game_parts.iter().map(Vec::len).sum::<usize>()
                ))
                .small()
                .color(MUTED),
            );
            ui.add_space(8.0);
            for (ti, parts) in artifact.game_parts.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("트랙 {}", ti + 1));
                    ui.label(
                        RichText::new(
                            parts
                                .iter()
                                .map(|p| format!("{}자", number(p.len())))
                                .collect::<Vec<_>>()
                                .join(" / "),
                        )
                        .color(MUTED),
                    );
                });
            }
        }
        ui.add_space(20.0);
        ui.separator();
        ui.add_space(8.0);
        ui.label(RichText::new(format!("파일 {}개", result.artifacts.len())).color(MUTED));
        if let Some(path) = &self.export_path {
            ui.label(format!("저장 위치: {}", path.display()));
        }
        ui.add_space(8.0);
        let save = ui.button("MMI 저장…").clicked();
        drop(result);
        if selected != self.selected_artifact {
            self.select_artifact(selected);
        }
        if save {
            self.save(&ui.ctx().clone());
        }
    }
}

impl eframe::App for MmlApp {
    /// 작업 결과와 사용자 입력을 처리하며 앱 화면을 갱신한다.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        self.update_playback(ctx);
        if !ctx.wants_keyboard_input()
            && ctx.input_mut(|i| consume_playback_shortcut(&mut i.events))
        {
            self.toggle_playback();
        }
        if let Some(path) = self.initial.take() {
            self.load(ctx, path);
        }
        if !self.busy()
            && let Some(path) =
                ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()))
        {
            self.load(ctx, path);
        }
        if !self.busy() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::O))
        {
            self.choose_file(ctx);
        }
        self.toolbar(ctx);
        if self.busy() || !self.status.is_empty() {
            egui::TopBottomPanel::bottom("status")
                .frame(
                    egui::Frame::new()
                        .fill(PANEL)
                        .inner_margin(egui::Margin::symmetric(24, 6)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if self.busy() {
                            ui.spinner();
                            ctx.request_repaint_after(Duration::from_millis(100));
                        }
                        ui.label(RichText::new(&self.status).small().color(MUTED));
                    });
                });
        }
        self.sidebar(ctx);
        self.content(ctx);
        if let Some(error) = self.error.clone() {
            egui::Window::new("작업 오류")
                .collapsible(false)
                .resizable(true)
                .default_width(520.0)
                .show(ctx, |ui| {
                    ui.colored_label(Color32::from_rgb(185, 28, 28), error);
                    ui.add_space(12.0);
                    if ui.button("확인").clicked() {
                        self.error = None;
                    }
                });
        }
    }
}

/// 앱의 글꼴·색상·간격과 위젯 스타일을 설정한다.
fn configure(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Light);
    crate::typography::configure(ctx);
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.panel_fill = BG;
    style.visuals.window_fill = BG;
    style.visuals.extreme_bg_color = BG;
    style.visuals.faint_bg_color = PANEL;
    style.visuals.selection.bg_fill = Color32::from_rgb(228, 228, 231);
    style.visuals.selection.stroke = Stroke::new(1.0_f32, INK);
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    for widget in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widget.bg_fill = BG;
        widget.weak_bg_fill = BG;
        widget.bg_stroke = Stroke::new(1.0_f32, BORDER);
        widget.fg_stroke = Stroke::new(1.0_f32, INK);
        widget.corner_radius = egui::CornerRadius::same(5);
        widget.expansion = 0.0;
    }
    style.visuals.widgets.inactive.bg_fill = BORDER;
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(244, 244, 245);
    style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(244, 244, 245);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(228, 228, 231);
    style.visuals.widgets.active.weak_bg_fill = Color32::from_rgb(228, 228, 231);
    style.visuals.override_text_color = Some(INK);
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.interact_size = egui::vec2(40.0, 32.0);
    style.spacing.combo_height = 250.0;
    style.spacing.slider_rail_height = 4.0;
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(17.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::proportional(17.0));
    style
        .text_styles
        .insert(egui::TextStyle::Heading, FontId::proportional(20.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Monospace, FontId::monospace(14.0));
    ctx.set_style(style);
}

#[derive(Clone, Copy)]
enum TransportIcon {
    Play,
    Pause,
    Stop,
}

/// 재생 상태에 맞는 그래픽 버튼을 그린다.
fn transport_button(
    ui: &mut egui::Ui,
    enabled: bool,
    icon: TransportIcon,
    label: &str,
) -> egui::Response {
    ui.add_enabled(enabled, |ui: &mut egui::Ui| {
        let response = ui.add_sized([38.0, 34.0], egui::Button::new(""));
        if ui.is_rect_visible(response.rect) {
            let color = ui.style().interact(&response).fg_stroke.color;
            let center = response.rect.center();
            let painter = ui.painter();
            match icon {
                TransportIcon::Play => {
                    painter.add(egui::Shape::convex_polygon(
                        vec![
                            center + egui::vec2(-5.0, -7.0),
                            center + egui::vec2(7.0, 0.0),
                            center + egui::vec2(-5.0, 7.0),
                        ],
                        color,
                        Stroke::NONE,
                    ));
                }
                TransportIcon::Pause => {
                    for offset in [-4.5, 4.5] {
                        painter.rect_filled(
                            egui::Rect::from_center_size(
                                center + egui::vec2(offset, 0.0),
                                egui::vec2(4.0, 14.0),
                            ),
                            1,
                            color,
                        );
                    }
                }
                TransportIcon::Stop => {
                    painter.rect_filled(
                        egui::Rect::from_center_size(center, egui::vec2(12.0, 12.0)),
                        1,
                        color,
                    );
                }
            }
        }
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
        response
    })
    .on_hover_cursor(egui::CursorIcon::PointingHand)
    .on_hover_text(label)
}

/// 설정 항목의 짧은 제목을 표시한다.
fn section(ui: &mut egui::Ui, label: &str) {
    ui.label(RichText::new(label).size(14.0).color(INK));
    ui.add_space(4.0);
}

/// 트랙 음소거와 파트별 소리 설정을 표시한다.
fn track_controls(
    ui: &mut egui::Ui,
    player: &mut Player,
    score: &Score,
    part_widths: Option<&[Vec<usize>]>,
) {
    let row_width = ui.available_width();
    egui::Frame::new()
        .fill(PANEL)
        .inner_margin(egui::Margin::symmetric(12, 5))
        .show(ui, |ui| {
            ui.set_width(row_width - 24.0);
            ui.columns(3, |cols| {
                for (col, label) in cols.iter_mut().zip(["트랙", "음표 수", "파트별 글자 수"])
                {
                    col.label(RichText::new(label).size(14.0).color(MUTED));
                }
            });
        });
    for (ti, track) in score.tracks.iter().enumerate() {
        ui.push_id(("track-sound", ti), |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(12, 5))
                .show(ui, |ui| {
                    ui.set_width(row_width - 24.0);
                    let mut track_on = player.track_enabled(ti);
                    ui.columns(3, |cols| {
                        cols[0].horizontal(|ui| {
                            if sound_button(
                                ui,
                                &mut track_on,
                                None,
                                &format!("트랙 {} 소리", ti + 1),
                            )
                            .changed()
                            {
                                player.set_track_enabled(ti, track_on);
                            }
                            track_marker(ui, ti);
                            let name = track
                                .meta
                                .get("name")
                                .cloned()
                                .unwrap_or_else(|| format!("Track{}", ti + 1));
                            ui.add(
                                egui::Label::new(RichText::new(&name).color(if track_on {
                                    INK
                                } else {
                                    MUTED
                                }))
                                .truncate(),
                            )
                            .on_hover_text(name);
                        });
                        cols[1].label(number(track.parts.iter().map(Vec::len).sum()));
                        let widths = part_widths
                            .and_then(|widths| widths.get(ti))
                            .map(|widths| {
                                widths
                                    .iter()
                                    .map(|width| number(*width))
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_else(|| {
                                track
                                    .mml
                                    .trim_start_matches("MML@")
                                    .trim_end_matches(';')
                                    .split(',')
                                    .map(|part| number(part.len()))
                                    .collect()
                            });
                        cols[2].label(RichText::new(widths.join(" / ")).size(14.0));
                    });
                    ui.add_enabled_ui(track_on, |ui| {
                        part_sound_controls(ui, player, ti, track.parts.len());
                    });
                });
        });
        ui.separator();
    }
}

/// 파트별 음소거 버튼과 악기 선택기를 표시한다.
fn part_sound_controls(ui: &mut egui::Ui, player: &mut Player, track: usize, parts: usize) {
    let control_width = 192.0_f32.min(ui.available_width());
    ui.horizontal_wrapped(|ui| {
        for part in 0..parts {
            ui.allocate_ui_with_layout(
                egui::vec2(control_width, 28.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.spacing_mut().button_padding = egui::vec2(8.0, 4.0);
                    ui.spacing_mut().interact_size.y = 28.0;
                    let mut part_on = player.part_enabled(track, part);
                    if sound_button(
                        ui,
                        &mut part_on,
                        Some(&(part + 1).to_string()),
                        &format!("트랙 {} 파트 {} 소리", track + 1, part + 1),
                    )
                    .changed()
                    {
                        player.set_part_enabled(track, part, part_on);
                    }
                    let mut instrument = player.part_instrument(track, part);
                    let response = egui::ComboBox::from_id_salt(("part-instrument", track, part))
                        .selected_text(instrument.name())
                        .width((control_width - 52.0).max(40.0))
                        .truncate()
                        .show_ui(ui, |ui| {
                            for choice in Instrument::ALL {
                                ui.selectable_value(&mut instrument, choice, choice.name());
                            }
                        })
                        .response
                        .on_hover_text(format!(
                            "트랙 {} 파트 {} · {}\n다음 음부터 적용",
                            track + 1,
                            part + 1,
                            instrument.name()
                        ));
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::ComboBox,
                            ui.is_enabled(),
                            format!("트랙 {} 파트 {} 악기", track + 1, part + 1),
                        )
                    });
                    if instrument != player.part_instrument(track, part) {
                        player.set_part_instrument(track, part, instrument);
                    }
                },
            );
        }
    });
}

/// 아이콘 글꼴 없이 스피커 버튼을 벡터로 그린다.
fn sound_button(
    ui: &mut egui::Ui,
    on: &mut bool,
    number: Option<&str>,
    label: &str,
) -> egui::Response {
    let mut response = ui.add_sized(
        [if number.is_some() { 48.0 } else { 32.0 }, 28.0],
        egui::Button::new("").fill(if *on { PANEL } else { BG }),
    );
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(response.rect) {
        let color = if !ui.is_enabled() {
            BORDER
        } else if *on {
            INK
        } else {
            MUTED
        };
        let stroke = Stroke::new(1.35_f32, color);
        let origin = egui::pos2(response.rect.left() + 15.0, response.rect.center().y);
        let point = |x, y| origin + egui::vec2(x, y);
        ui.painter().add(egui::Shape::closed_line(
            vec![
                point(-7.0, -3.0),
                point(-4.0, -3.0),
                point(0.0, -6.0),
                point(0.0, 6.0),
                point(-4.0, 3.0),
                point(-7.0, 3.0),
            ],
            stroke,
        ));
        if *on {
            ui.painter().add(egui::Shape::line(
                vec![
                    point(3.0, -4.0),
                    point(5.0, -2.0),
                    point(5.5, 0.0),
                    point(5.0, 2.0),
                    point(3.0, 4.0),
                ],
                stroke,
            ));
        } else {
            ui.painter()
                .line_segment([point(2.0, -3.0), point(7.0, 3.0)], stroke);
            ui.painter()
                .line_segment([point(2.0, 3.0), point(7.0, -3.0)], stroke);
        }
        if let Some(number) = number {
            ui.painter().text(
                egui::pos2(response.rect.right() - 11.0, origin.y),
                egui::Align2::CENTER_CENTER,
                number,
                FontId::proportional(14.0),
                color,
            );
        }
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, label)
    });
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(format!("{label} · {}", if *on { "켜짐" } else { "꺼짐" }))
}

/// 선택 상태를 표시하는 탭 버튼을 그린다.
fn nav_tab(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::proportional(17.0),
        if selected { INK } else { MUTED },
    );
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(galley.size().x + 8.0, 38.0),
        egui::Sense::click(),
    );
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() {
        ui.painter()
            .rect_filled(rect.shrink2(egui::vec2(0.0, 5.0)), 4, PANEL);
    }
    ui.painter().galley(
        rect.center() - galley.size() * 0.5,
        galley,
        if selected { INK } else { MUTED },
    );
    if selected {
        ui.painter().line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            Stroke::new(2.0_f32, INK),
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            label,
        )
    });
    response
}

/// 정수를 세 자리 단위로 구분해 표시한다.
fn number(value: usize) -> String {
    let text = value.to_string();
    let mut out = String::new();
    for (i, c) in text.chars().enumerate() {
        if i > 0 && (text.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

struct PianoInteraction {
    seek: Option<f64>,
    zoom: Option<(f64, f64)>,
    horizontal: f64,
}

/// 트랙을 구분하는 색상 표시를 그린다.
fn track_marker(ui: &mut egui::Ui, index: usize) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(11.0, 11.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, 2, TRACK_COLORS[index % TRACK_COLORS.len()]);
}

/// 분할된 장의 길이와 현재 선택을 표시한다.
fn split_overview(
    ui: &mut egui::Ui,
    split: &workflow::SplitPreview,
    timeline: Option<&Timeline>,
    selected: usize,
) -> Option<usize> {
    let total = split.chunks.last()?.end.max(1) as f64;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::hover());
    let mut clicked = None;
    for (i, chunk) in split.chunks.iter().enumerate() {
        let left = rect.left() + (chunk.start as f64 / total) as f32 * rect.width();
        let right = rect.left() + (chunk.end as f64 / total) as f32 * rect.width();
        let segment = egui::Rect::from_min_max(
            egui::pos2(left, rect.top()),
            egui::pos2(right, rect.bottom()),
        );
        let response = ui.interact(
            segment,
            ui.id().with(("split-chunk", i)),
            egui::Sense::click(),
        );
        let active = i == selected;
        ui.painter().rect_filled(
            segment,
            0,
            if active {
                Color32::from_rgb(228, 228, 231)
            } else if response.hovered() {
                Color32::from_rgb(244, 244, 245)
            } else {
                PANEL
            },
        );
        ui.painter().line_segment(
            [segment.left_top(), segment.left_bottom()],
            Stroke::new(1.0_f32, MUTED),
        );
        if segment.width() >= 26.0 {
            ui.painter().text(
                segment.center(),
                egui::Align2::CENTER_CENTER,
                format!("{:02}", i + 1),
                FontId::monospace(14.0),
                INK,
            );
        }
        if active {
            ui.painter().line_segment(
                [segment.left_bottom(), segment.right_bottom()],
                Stroke::new(2.0_f32, INK),
            );
        }
        let widest = chunk
            .part_widths
            .iter()
            .flatten()
            .max()
            .copied()
            .unwrap_or(0);
        let range = timeline.map_or_else(
            || format!("{}–{} 틱", chunk.start, chunk.end),
            |t| {
                format!(
                    "{}–{}",
                    playback_time(t.tick_to_seconds(chunk.start as f64)),
                    playback_time(t.tick_to_seconds(chunk.end as f64))
                )
            },
        );
        let label = format!(
            "{:02}장 · {range}\n가장 긴 파트 {}자 / {}자\n클릭하여 이 구간 보기",
            i + 1,
            number(widest),
            number(split.limit)
        );
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, active, &label)
        });
        if response
            .on_hover_text(label)
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            clicked = Some(i);
        }
    }
    ui.painter().rect_stroke(
        rect,
        0,
        Stroke::new(1.0_f32, BORDER),
        egui::StrokeKind::Inside,
    );
    clicked
}

/// 음표와 재생 커서를 그리고 탐색·확대 입력을 처리한다.
fn piano_roll(
    ui: &mut egui::Ui,
    score: &Score,
    start: i64,
    span: i64,
    playhead: Option<f64>,
    split: Option<&workflow::SplitPreview>,
    player: Option<&Player>,
) -> PianoInteraction {
    let (response, painter) = ui.allocate_painter(
        egui::vec2(ui.available_width(), 236.0),
        egui::Sense::click_and_drag(),
    );
    let rect = response.rect;
    painter.rect_filled(rect, 5, PANEL);
    painter.rect_stroke(
        rect,
        5,
        Stroke::new(1.0_f32, BORDER),
        egui::StrokeKind::Inside,
    );
    let plot = egui::Rect::from_min_max(
        rect.min + egui::vec2(38.0, 8.0),
        rect.max - egui::vec2(20.0, 24.0),
    );
    let (pitch_min, pitch_max) = score
        .tracks
        .iter()
        .flat_map(|t| &t.parts)
        .flatten()
        .fold((21, 108), |(lo, hi), n| (lo.min(n.pitch), hi.max(n.pitch)));
    let pitch_h = plot.height() / (pitch_max - pitch_min + 1) as f32;
    for pitch in pitch_min..=pitch_max {
        let y = plot.bottom() - (pitch - pitch_min + 1) as f32 * pitch_h;
        if pitch % 12 == 0 {
            painter.line_segment(
                [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
                Stroke::new(0.5_f32, BORDER),
            );
            painter.text(
                egui::pos2(plot.left() - 5.0, y),
                egui::Align2::RIGHT_CENTER,
                format!("C{}", pitch / 12 - 1),
                FontId::monospace(13.0),
                MUTED,
            );
        }
    }
    let step = if span > 96 * 64 { 1536 } else { 384 };
    for tick in ((start / step) * step..=start + span).step_by(step as usize) {
        if tick < start {
            continue;
        }
        let x = plot.left() + (tick - start) as f32 / span as f32 * plot.width();
        painter.line_segment(
            [egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())],
            Stroke::new(0.5_f32, BORDER),
        );
        painter.text(
            egui::pos2(x, plot.bottom() + 5.0),
            egui::Align2::CENTER_TOP,
            format!("{tick}"),
            FontId::monospace(13.0),
            MUTED,
        );
    }
    let mut hovered = None;
    for (ti, track) in score.tracks.iter().enumerate() {
        for (pi, part) in track.parts.iter().enumerate() {
            let audible = player.is_none_or(|p| p.track_enabled(ti) && p.part_enabled(ti, pi));
            let color = TRACK_COLORS[ti % TRACK_COLORS.len()];
            let color = if audible {
                color
            } else {
                color.gamma_multiply(0.18)
            };
            for note in part {
                if note.off <= start || note.on >= start + span {
                    continue;
                }
                let x1 =
                    plot.left() + (note.on.max(start) - start) as f32 / span as f32 * plot.width();
                let x2 = plot.left()
                    + (note.off.min(start + span) - start) as f32 / span as f32 * plot.width();
                let y = plot.bottom() - (note.pitch - pitch_min + 1) as f32 * pitch_h;
                let r = egui::Rect::from_min_max(
                    egui::pos2(x1, y),
                    egui::pos2(x2.max(x1 + 1.0), y + pitch_h.max(2.0)),
                );
                if plot.intersects(r) {
                    painter.rect_filled(r.intersect(plot), 1, color);
                    if let Some(pos) = response.hover_pos()
                        && r.contains(pos)
                    {
                        hovered = Some(format!(
                            "트랙 {} · 파트 {}\nMIDI {} · v{}\n{}–{}틱",
                            ti + 1,
                            pi + 1,
                            note.pitch,
                            note.vel,
                            note.on,
                            note.off
                        ));
                    }
                }
            }
        }
    }
    if let Some(split) = split {
        for (i, chunk) in split.chunks.iter().enumerate().skip(1) {
            if chunk.start < start || chunk.start > start + span {
                continue;
            }
            let x = plot.left() + (chunk.start - start) as f32 / span as f32 * plot.width();
            let mut y = plot.top();
            while y < plot.bottom() {
                painter.line_segment(
                    [
                        egui::pos2(x, y),
                        egui::pos2(x, (y + 5.0).min(plot.bottom())),
                    ],
                    Stroke::new(1.25_f32, INK),
                );
                y += 9.0;
            }
            let label =
                painter.layout_no_wrap(format!("{}장", i + 1), FontId::proportional(14.0), INK);
            let label_pos = egui::pos2(
                (x + 4.0).min(plot.right() - label.size().x),
                plot.top() + 3.0,
            );
            painter.rect_filled(
                egui::Rect::from_min_size(label_pos, label.size()).expand(2.0),
                2,
                BG,
            );
            painter.galley(label_pos, label, INK);
        }
    }
    if let Some(tick) = playhead.filter(|t| *t >= start as f64 && *t <= (start + span) as f64) {
        let x = plot.left() + ((tick - start as f64) / span as f64) as f32 * plot.width();
        painter.line_segment(
            [egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())],
            Stroke::new(1.25_f32, INK),
        );
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(x - 4.0, plot.top()),
                egui::pos2(x + 4.0, plot.top()),
                egui::pos2(x, plot.top() + 5.0),
            ],
            INK,
            Stroke::NONE,
        ));
    }
    let seek = if response.clicked() || response.dragged() {
        response
            .interact_pointer_pos()
            .filter(|pos| plot.contains(*pos))
            .map(|pos| {
                (start as f64 + ((pos.x - plot.left()) / plot.width()) as f64 * span as f64)
                    .clamp(0.0, score.total_ticks() as f64)
            })
    } else {
        None
    };
    let zoom = response
        .hover_pos()
        .filter(|p| plot.contains(*p))
        .and_then(|pointer| {
            let logarithm = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|event| {
                        if let egui::Event::MouseWheel {
                            unit,
                            delta,
                            modifiers,
                        } = event
                            && (modifiers.ctrl || modifiers.command)
                        {
                            let scale = match unit {
                                egui::MouseWheelUnit::Line => 0.08,
                                egui::MouseWheelUnit::Point => 0.002,
                                egui::MouseWheelUnit::Page => 0.3,
                            };
                            return Some(f64::from(delta.y) * scale);
                        }
                        None
                    })
                    .sum::<f64>()
            });
            (logarithm != 0.0).then(|| {
                (
                    logarithm.clamp(-4.0, 4.0).exp(),
                    f64::from((pointer.x - plot.left()) / plot.width()),
                )
            })
        });
    // egui가 누적한 Shift+휠 가로 이동을 소비해 부모 영역의 세로 스크롤과 중복되지 않게 한다.
    let horizontal = if response.hovered() {
        ui.input_mut(|input| {
            if input.modifiers.ctrl || input.modifiers.command {
                return 0.0;
            }
            -f64::from(std::mem::take(&mut input.smooth_scroll_delta.x)) * span as f64
                / f64::from(plot.width().max(1.0))
        })
    } else {
        0.0
    };
    if let Some(text) = hovered {
        response.on_hover_text(format!("{text}\n클릭하여 재생 위치 이동"));
    }
    PianoInteraction {
        seek,
        zoom,
        horizontal,
    }
}

/// 따라가기 또는 포인터 위치를 기준으로 확대 후 시작점을 계산한다.
fn zoomed_view_start(
    start: i64,
    old_beats: i64,
    new_beats: i64,
    anchor: f64,
    total: i64,
    followed_tick: Option<f64>,
) -> i64 {
    // 따라가기 중에는 커서 위치, 그 외에는 포인터나 범위 입력의 왼쪽 끝을 고정한다.
    let (tick, anchor) = if let Some(tick) = followed_tick {
        (
            tick,
            ((tick - start as f64) / (old_beats * 96) as f64).clamp(0.0, 1.0),
        )
    } else {
        let anchor = anchor.clamp(0.0, 1.0);
        (start as f64 + anchor * (old_beats * 96) as f64, anchor)
    };
    ((tick - anchor * (new_beats * 96) as f64).round() as i64)
        .clamp(0, (total - new_beats * 96).max(0))
}

/// Space의 최초 누름만 소비해 재생 토글 여부를 반환한다.
fn consume_playback_shortcut(events: &mut Vec<egui::Event>) -> bool {
    let mut toggle = false;
    events.retain(|event| {
        if let egui::Event::Key {
            key: egui::Key::Space,
            pressed: true,
            repeat,
            modifiers,
            ..
        } = event
            && *modifiers == egui::Modifiers::NONE
        {
            toggle |= !repeat;
            false
        } else {
            true
        }
    });
    toggle
}

/// 악보 바로 아래에 시간축 스크롤바를 표시한다.
fn score_scrollbar(ui: &mut egui::Ui, start: &mut i64, total: i64, span: i64) -> bool {
    let old = *start;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let scroll = &mut ui.style_mut().spacing.scroll;
        scroll.floating = false;
        scroll.bar_width = 8.0;
        scroll.handle_min_length = 28.0;
        scroll.bar_inner_margin = 0.0;
        scroll.bar_outer_margin = 0.0;
        ui.visuals_mut().extreme_bg_color = Color32::from_rgb(240, 240, 242);
        ui.visuals_mut().widgets.inactive.fg_stroke.color = MUTED;
        let width = ui.available_width().max(1.0);
        let scale = width as f64 / span.max(1) as f64;
        let content_width = (total.max(span) as f64 * scale) as f32;
        let result = egui::ScrollArea::horizontal()
            .id_salt("score-horizontal")
            .auto_shrink([false, true])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .horizontal_scroll_offset((old as f64 * scale) as f32)
            .show(ui, |ui| {
                ui.set_min_size(egui::vec2(content_width, 1.0));
            });
        *start = (result.state.offset.x as f64 / scale).round() as i64;
        *start = (*start).clamp(0, (total - span).max(0));
    });
    (*start - old).abs() > 1
}

/// 재생 시간을 분·초 형식으로 표시한다.
fn playback_time(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        )
    } else {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
#[path = "gui_state_tests.rs"]
mod state_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Space 반복 입력이 재생을 다시 토글하거나 텍스트를 지우지 않는지 확인한다.
    fn held_space_toggles_only_once_and_keeps_text_events() {
        let key = |repeat| egui::Event::Key {
            key: egui::Key::Space,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers: egui::Modifiers::NONE,
        };
        let mut events = vec![key(false), key(true), egui::Event::Text(" ".into())];
        assert!(consume_playback_shortcut(&mut events));
        assert_eq!(events.len(), 1);
        let mut repeated = vec![key(true)];
        assert!(!consume_playback_shortcut(&mut repeated));
        assert!(repeated.is_empty());
    }

    #[test]
    /// 확대 시 포인터의 시점과 악보 경계가 유지되는지 확인한다.
    fn zoom_preserves_pointer_tick_and_clamps_at_score_edges() {
        let next = zoomed_view_start(3072, 32, 16, 0.25, 30000, None);
        assert_eq!(
            3072.0 + 0.25 * 32.0 * 96.0,
            next as f64 + 0.25 * 16.0 * 96.0
        );
        assert_eq!(zoomed_view_start(next, 16, 32, 0.25, 30000, None), 3072);
        assert_eq!(zoomed_view_start(0, 4, 256, 0.5, 30000, None), 0);
        assert_eq!(zoomed_view_start(100, 4, 32, 0.5, 192, None), 0);
    }

    #[test]
    /// 따라가기 중 확대해도 커서의 화면 위치가 유지되는지 확인한다.
    fn following_zoom_keeps_the_playhead_visible_at_its_screen_position() {
        let start = 3072;
        let tick = 5376.0;
        for pointer in [0.0, 0.5, 1.0] {
            let next = zoomed_view_start(start, 32, 4, pointer, 30000, Some(tick));
            assert_eq!((tick - next as f64) / (4.0 * 96.0), 0.75);
            assert_eq!(
                zoomed_view_start(next, 4, 32, pointer, 30000, Some(tick)),
                start
            );
        }
        assert_eq!(zoomed_view_start(0, 32, 256, 1.0, 30000, Some(96.0)), 0);
        let at_end = zoomed_view_start(29616, 4, 32, 0.0, 30000, Some(29950.0));
        assert_eq!(at_end, 30000 - 32 * 96);
        assert!(29950 < at_end + 32 * 96);
    }

    #[test]
    /// 악보 위 Ctrl+휠만 확대 입력으로 처리되는지 확인한다.
    fn only_control_wheel_over_the_piano_requests_zoom() {
        let score = core::parse_mmi("[mml-score]\nmml-track=MML@c1;\nvisible=true\n").unwrap();
        for (control, expected) in [(true, true), (false, false)] {
            let ctx = egui::Context::default();
            // egui의 hit-test에는 직전 패스에서 배치한 위젯이 필요하다.
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        piano_roll(ui, &score, 0, 3072, Some(0.0), None, None);
                    });
                },
            );
            let modifiers = if control {
                egui::Modifiers::CTRL
            } else {
                egui::Modifiers::NONE
            };
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                modifiers,
                events: vec![
                    egui::Event::PointerMoved(egui::pos2(300.0, 100.0)),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta: egui::vec2(0.0, 3.0),
                        modifiers,
                    },
                ],
                ..Default::default()
            };
            let mut zoom = None;
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    zoom = piano_roll(ui, &score, 0, 3072, Some(0.0), None, None).zoom;
                });
            });
            assert_eq!(zoom.is_some(), expected);
            if let Some((factor, anchor)) = zoom {
                assert!(factor > 1.0 && (0.0..=1.0).contains(&anchor));
            }
            assert_eq!(ctx.zoom_factor(), 1.0);
        }
    }

    #[test]
    /// 휠 확대와 범위 편집이 따라가기 설정을 유지하는지 확인한다.
    fn control_wheel_and_range_changes_preserve_the_follow_setting() {
        for follow in [true, false] {
            let ctx = egui::Context::default();
            let mut app = MmlApp::with_context(&ctx, None);
            let score = core::load_mmi(
                &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Op39No11_full_6part.mmi"),
            )
            .unwrap();
            app.accept_source(Loaded {
                path: PathBuf::from("test.mmi"),
                notes: score.all_notes().len(),
                polyphony: 6,
                score: Arc::new(score),
            });
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1180.0, 820.0));
            let draw = |app: &mut MmlApp, modifiers, events| {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        modifiers,
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| app.preview(ui));
                    },
                );
            };
            draw(&mut app, egui::Modifiers::NONE, vec![]);
            app.follow_playhead = follow;
            app.start_tick = 6144;
            for delta in [3.0, -3.0] {
                let before = app.beats;
                draw(
                    &mut app,
                    egui::Modifiers::CTRL,
                    vec![
                        egui::Event::PointerMoved(egui::pos2(300.0, 100.0)),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Line,
                            delta: egui::vec2(0.0, delta),
                            modifiers: egui::Modifiers::CTRL,
                        },
                    ],
                );
                assert_ne!(app.beats, before, "the preview must actually zoom");
                assert_eq!(app.follow_playhead, follow);
            }
            // 범위 편집에 따른 스크롤바 재배치를 수동 탐색으로 처리하면 안 된다.
            for beats in [4, 256, 32] {
                app.beats = beats;
                app.zoom_target_beats = beats as f64;
                for _ in 0..3 {
                    draw(&mut app, egui::Modifiers::NONE, vec![]);
                    assert_eq!(app.follow_playhead, follow);
                    assert_eq!(app.beats, beats);
                }
            }
            assert_eq!(app.player.snapshot().position_seconds, 0.0);
            assert_eq!(app.player.snapshot().state, PlaybackState::Stopped);
        }
    }

    #[test]
    /// Shift+휠이 시간축만 이동시키는지 확인한다.
    fn shift_wheel_moves_the_timeline_without_zooming_or_scrolling_the_page() {
        let ctx = egui::Context::default();
        let mut app = MmlApp::with_context(&ctx, None);
        let score = core::load_mmi(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Op39No11_full_6part.mmi"),
        )
        .unwrap();
        app.accept_source(Loaded {
            path: PathBuf::from("test.mmi"),
            notes: score.all_notes().len(),
            polyphony: 6,
            score: Arc::new(score),
        });
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1180.0, 820.0));
        let mut draw = |ctx: &egui::Context| {
            let mut page_offset = 0.0;
            egui::CentralPanel::default().show(ctx, |ui| {
                let scroll = egui::ScrollArea::vertical()
                    .id_salt("shift-wheel-page")
                    .show(ui, |ui| {
                        app.preview(ui);
                        ui.add_space(1200.0);
                    });
                page_offset = scroll.state.offset.y;
            });
            page_offset
        };
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                draw(ctx);
            },
        );
        let mut page_offset = 0.0;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                modifiers: egui::Modifiers::SHIFT,
                events: vec![
                    egui::Event::PointerMoved(egui::pos2(300.0, 100.0)),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta: egui::vec2(0.0, -3.0),
                        modifiers: egui::Modifiers::SHIFT,
                    },
                ],
                ..Default::default()
            },
            |ctx| {
                page_offset = draw(ctx);
            },
        );
        assert!(
            app.start_tick > 0,
            "Shift+wheel must move forward through the score"
        );
        assert_eq!(app.beats, 32);
        assert_eq!(page_offset, 0.0);
        assert!(!app.follow_playhead);
        assert_eq!(app.player.snapshot().position_seconds, 0.0);
        assert_eq!(app.player.snapshot().state, PlaybackState::Stopped);
    }

    #[test]
    /// 미리보기 진입이 따라가기나 표시 범위를 바꾸지 않는지 확인한다.
    fn opening_preview_does_not_turn_off_follow_or_move_the_view() {
        let ctx = egui::Context::default();
        let mut app = MmlApp::with_context(&ctx, None);
        let score = core::load_mmi(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Op39No11_full_6part.mmi"),
        )
        .unwrap();
        app.source = Some(Loaded {
            path: PathBuf::from("test.mmi"),
            notes: score.all_notes().len(),
            polyphony: 6,
            score: Arc::new(score),
        });
        for frame in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1180.0, 820.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.preview(ui));
            });
            assert!(
                app.follow_playhead,
                "follow disabled on frame {frame}; start={}",
                app.start_tick
            );
            assert_eq!(app.start_tick, 0);
            assert_eq!(app.player.snapshot().state, PlaybackState::Stopped);
        }
    }
}
