//! 오디오 장치와 재생·정지·탐색 명령을 연결한다.

use crate::instruments::Instrument;
use crate::synth::{Audibility, Renderer, Timeline};
use anyhow::{Context, Result, anyhow, bail};
use cpal::{
    FromSample, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
}

#[derive(Clone, Copy, Debug)]
pub struct PlaybackSnapshot {
    pub state: PlaybackState,
    pub position_seconds: f64,
    pub total_seconds: f64,
}

impl Default for PlaybackSnapshot {
    /// 정지 상태의 빈 재생 스냅샷을 만든다.
    fn default() -> Self {
        Self {
            state: PlaybackState::Stopped,
            position_seconds: 0.0,
            total_seconds: 0.0,
        }
    }
}

#[derive(Clone, Copy)]
struct Published {
    revision: u64,
    snapshot: PlaybackSnapshot,
}

struct Shared {
    published: Mutex<Published>,
    failed: AtomicBool,
}

impl Shared {
    /// UI와 오디오 스레드가 공유할 재생 상태를 만든다.
    fn new(snapshot: PlaybackSnapshot, revision: u64) -> Self {
        Self {
            published: Mutex::new(Published { revision, snapshot }),
            failed: AtomicBool::new(false),
        }
    }

    /// 마지막으로 게시된 재생 상태를 읽는다.
    fn read(&self) -> Published {
        *self.published.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 오디오 스레드를 기다리게 하지 않고 최신 위치를 게시한다.
    fn publish(&self, value: Published) {
        // UI 잠금 때문에 오디오를 기다리게 하지 않으며 누락된 위치는 다음 버퍼에서 갱신한다.
        if let Ok(mut published) = self.published.try_lock() {
            *published = value;
        }
    }
}

enum Action {
    Play,
    Pause,
    Stop,
    Seek {
        seconds: f64,
        prepared: Option<Box<Renderer>>,
    },
    Volume(f32),
    TrackEnabled {
        track: usize,
        enabled: bool,
    },
    PartEnabled {
        track: usize,
        part: usize,
        enabled: bool,
    },
    PartInstrument {
        track: usize,
        part: usize,
        instrument: Instrument,
    },
}

struct Command {
    revision: u64,
    action: Action,
}

/// 콜백이 소유하는 재생 상태이며 명령은 버퍼 단위로 동기화한다.
struct Transport {
    renderer: Box<Renderer>,
    retired: Option<Sender<Box<Renderer>>>,
    state: PlaybackState,
    total_seconds: f64,
    volume: f32,
    current_volume: f32,
    volume_step: f32,
    revision: u64,
}

impl Transport {
    /// 저장된 위치·음량·선택 상태로 콜백용 재생기를 준비한다.
    #[cfg(test)]
    fn new(
        timeline: Arc<Timeline>,
        sample_rate: u32,
        snapshot: PlaybackSnapshot,
        volume: f32,
        revision: u64,
        audibility: Audibility,
    ) -> Self {
        Self::try_new(
            timeline,
            sample_rate,
            snapshot,
            volume,
            revision,
            audibility,
        )
        .expect("valid test timeline and installed sound banks")
    }

    /// 음원 오류를 호출자에게 반환하며 콜백용 재생기를 준비한다.
    fn try_new(
        timeline: Arc<Timeline>,
        sample_rate: u32,
        snapshot: PlaybackSnapshot,
        volume: f32,
        revision: u64,
        audibility: Audibility,
    ) -> Result<Self> {
        let mut renderer = Renderer::try_with_audibility(timeline, sample_rate, audibility)?;
        renderer.seek(snapshot.position_seconds);
        Ok(Self {
            renderer: Box::new(renderer),
            retired: None,
            state: snapshot.state,
            total_seconds: snapshot.total_seconds,
            volume,
            current_volume: volume,
            volume_step: 1.0 / (sample_rate.max(1) as f32 * 0.01),
            revision,
        })
    }

    /// 버퍼 경계에서 재생 명령을 순서대로 적용한다.
    fn command(&mut self, command: Command) {
        self.revision = command.revision;
        match command.action {
            Action::Play => {
                if self.renderer.finished()
                    || self.renderer.position_seconds() >= self.total_seconds
                {
                    self.renderer.seek(0.0);
                }
                self.state = PlaybackState::Playing;
            }
            Action::Pause => {
                if self.state == PlaybackState::Playing {
                    self.state = PlaybackState::Paused;
                    self.renderer.sync_audibility();
                }
            }
            Action::Stop => {
                self.state = PlaybackState::Stopped;
                self.renderer.seek(0.0);
            }
            Action::Seek { seconds, prepared } => {
                if let Some(renderer) = prepared {
                    let previous = std::mem::replace(&mut self.renderer, renderer);
                    if let Some(retired) = &self.retired {
                        // 큰 음성 버퍼의 해제는 렌더링을 재개한 뒤 UI 스레드에서 처리한다.
                        let _ = retired.send(previous);
                    }
                } else {
                    self.renderer
                        .seek(clamp_position(seconds, self.total_seconds));
                }
            }
            Action::Volume(volume) => self.volume = clamp_volume(volume),
            Action::TrackEnabled { track, enabled } => {
                self.renderer.set_track_enabled(track, enabled);
                if self.state != PlaybackState::Playing {
                    self.renderer.sync_audibility();
                }
            }
            Action::PartEnabled {
                track,
                part,
                enabled,
            } => {
                self.renderer.set_part_enabled(track, part, enabled);
                if self.state != PlaybackState::Playing {
                    self.renderer.sync_audibility();
                }
            }
            Action::PartInstrument {
                track,
                part,
                instrument,
            } => {
                self.renderer.set_part_instrument(track, part, instrument);
            }
        }
    }

    /// 재생 중 한 프레임을 출력하고 음량을 부드럽게 변경한다.
    fn next_frame(&mut self) -> [f32; 2] {
        if self.state != PlaybackState::Playing {
            return [0.0, 0.0];
        }
        let frame = self.renderer.next_frame();
        if self.renderer.finished() {
            self.state = PlaybackState::Stopped;
        }
        self.current_volume +=
            (self.volume - self.current_volume).clamp(-self.volume_step, self.volume_step);
        frame.map(|sample| {
            if sample.is_finite() {
                (sample * self.current_volume).clamp(-1.0, 1.0)
            } else {
                0.0
            }
        })
    }

    /// 현재 revision과 재생 위치를 게시 가능한 상태로 묶는다.
    fn published(&self) -> Published {
        Published {
            revision: self.revision,
            snapshot: PlaybackSnapshot {
                state: self.state,
                position_seconds: clamp_position(
                    self.renderer.position_seconds(),
                    self.total_seconds,
                ),
                total_seconds: self.total_seconds,
            },
        }
    }
}

/// 출력 스트림을 소유하며 생성과 악보 로드만으로는 장치를 열지 않는다.
pub struct Player {
    timeline: Option<Arc<Timeline>>,
    stream: Option<cpal::Stream>,
    commands: Option<Sender<Command>>,
    errors: Option<Receiver<String>>,
    shared: Arc<Shared>,
    requested: Published,
    volume: f32,
    pending_error: Option<String>,
    audibility: Audibility,
    sample_rate: Option<u32>,
    retired: Option<Receiver<Box<Renderer>>>,
}

impl Default for Player {
    /// 장치를 열지 않은 기본 플레이어를 만든다.
    fn default() -> Self {
        let snapshot = PlaybackSnapshot::default();
        Self {
            timeline: None,
            stream: None,
            commands: None,
            errors: None,
            shared: Arc::new(Shared::new(snapshot, 0)),
            requested: Published {
                revision: 0,
                snapshot,
            },
            volume: 0.65,
            pending_error: None,
            audibility: Audibility::default(),
            sample_rate: None,
            retired: None,
        }
    }
}

impl Player {
    /// 장치를 열지 않고 악보를 교체하며 이전 스트림의 명령·오류·상태를 격리한다.
    pub fn load(&mut self, timeline: Arc<Timeline>) {
        self.close_stream();
        let snapshot = PlaybackSnapshot {
            total_seconds: timeline.duration_seconds,
            ..PlaybackSnapshot::default()
        };
        self.audibility = Audibility::all(&timeline);
        self.timeline = Some(timeline);
        self.requested = Published {
            revision: 0,
            snapshot,
        };
        self.shared = Arc::new(Shared::new(snapshot, 0));
        self.pending_error = None;
    }

    /// 필요할 때 장치를 열고 현재 위치에서 재생한다.
    pub fn play(&mut self) -> Result<()> {
        self.collect_error();
        let Some(timeline) = self.timeline.clone() else {
            bail!("재생할 악보를 먼저 열어 주세요.");
        };
        if timeline.duration_seconds <= 0.0 {
            bail!("이 악보에는 재생할 음표가 없습니다.");
        }
        let mut snapshot = self.snapshot();
        if snapshot.state == PlaybackState::Playing {
            return Ok(());
        }
        if self.stream.is_none() {
            self.open_stream(timeline)?;
        }
        #[cfg(target_arch = "wasm32")]
        if let Some(stream) = &self.stream {
            let cpal::platform::StreamInner::WebAudio(stream) = stream.as_inner();
            let _ = stream
                .audio_context()
                .resume()
                .map_err(|error| anyhow!("오디오 재생을 재개할 수 없습니다: {error:?}"))?;
        }
        if snapshot.position_seconds >= snapshot.total_seconds {
            snapshot.position_seconds = 0.0;
        }
        snapshot.state = PlaybackState::Playing;
        self.issue(Action::Play, snapshot);
        if self.stream.is_none() {
            return Err(anyhow!(self.pending_error.clone().unwrap_or_else(|| {
                "오디오 재생을 시작할 수 없습니다.".to_owned()
            })));
        }
        Ok(())
    }

    /// 재생 위치를 유지한 채 일시정지한다.
    pub fn pause(&mut self) {
        let mut snapshot = self.snapshot();
        if snapshot.state == PlaybackState::Playing {
            snapshot.state = PlaybackState::Paused;
            self.issue(Action::Pause, snapshot);
        }
    }

    /// 재생을 멈추고 시작 위치로 돌아간다.
    pub fn stop(&mut self) {
        let mut snapshot = self.snapshot();
        snapshot.state = PlaybackState::Stopped;
        snapshot.position_seconds = 0.0;
        self.issue(Action::Stop, snapshot);
    }

    /// 현재 재생 상태를 유지하며 지정 위치의 렌더러를 준비한다.
    pub fn seek(&mut self, seconds: f64) {
        let mut snapshot = self.snapshot();
        snapshot.position_seconds = clamp_position(seconds, snapshot.total_seconds);
        // 지속음 재생이 필요한 루프·필터·엔벌로프 복원은 오디오 스레드 밖에서 준비한다.
        let prepared = self
            .sample_rate
            .zip(self.timeline.as_ref())
            .map(|(rate, timeline)| {
                let mut renderer =
                    Renderer::try_with_audibility(timeline.clone(), rate, self.audibility.clone())?;
                renderer.seek(snapshot.position_seconds);
                Ok::<_, anyhow::Error>(Box::new(renderer))
            })
            .transpose();
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.pending_error = Some(format!("재생 위치를 변경할 수 없습니다: {error:#}"));
                return;
            }
        };
        self.issue(
            Action::Seek {
                seconds: snapshot.position_seconds,
                prepared,
            },
            snapshot,
        );
    }

    /// 전체 미리듣기 음량을 유효 범위 안에서 변경한다.
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = clamp_volume(volume);
        self.issue(Action::Volume(self.volume), self.snapshot());
    }

    /// 트랙의 소리 선택 상태를 반환한다.
    pub fn track_enabled(&self, track: usize) -> bool {
        self.audibility.track_enabled(track)
    }

    /// 트랙이 꺼져 있어도 파트 자체의 선택 상태를 반환한다.
    pub fn part_enabled(&self, track: usize, part: usize) -> bool {
        self.audibility.part_enabled(track, part)
    }

    /// 트랙 상태를 저장하고 재생기에 전달한다.
    pub fn set_track_enabled(&mut self, track: usize, enabled: bool) {
        if self.audibility.set_track_enabled(track, enabled) {
            self.issue(Action::TrackEnabled { track, enabled }, self.snapshot());
        }
    }

    /// 파트 상태를 저장하고 재생기에 전달한다.
    pub fn set_part_enabled(&mut self, track: usize, part: usize, enabled: bool) {
        if self.audibility.set_part_enabled(track, part, enabled) {
            self.issue(
                Action::PartEnabled {
                    track,
                    part,
                    enabled,
                },
                self.snapshot(),
            );
        }
    }

    /// 파트의 선택 악기를 반환한다.
    pub fn part_instrument(&self, track: usize, part: usize) -> Instrument {
        self.audibility.part_instrument(track, part)
    }

    /// 선택 악기를 저장하고 재생 상태에 맞춰 적용한다.
    pub fn set_part_instrument(&mut self, track: usize, part: usize, instrument: Instrument) {
        if self.audibility.set_part_instrument(track, part, instrument) {
            let snapshot = self.snapshot();
            self.issue(
                Action::PartInstrument {
                    track,
                    part,
                    instrument,
                },
                snapshot,
            );
            if snapshot.state == PlaybackState::Stopped && self.sample_rate.is_some() {
                // 정지·탐색 때 준비된 첫 음도 무음 상태에서는 새 악기로 다시 만든다.
                self.seek(snapshot.position_seconds);
            }
        }
    }

    /// 최신 UI 명령보다 오래된 콜백 상태를 제외해 재생 위치를 반환한다.
    pub fn snapshot(&self) -> PlaybackSnapshot {
        if let Some(retired) = &self.retired {
            for old in retired.try_iter() {
                drop(old);
            }
        }
        let published = self.shared.read();
        // 이전 콜백 상태가 방금 내린 탐색·정지 명령을 되돌리지 않도록 revision을 비교한다.
        let mut snapshot = if published.revision >= self.requested.revision {
            published.snapshot
        } else {
            self.requested.snapshot
        };
        if self.shared.failed.load(Ordering::Acquire) {
            snapshot.state = PlaybackState::Stopped;
        }
        snapshot
    }

    /// 장치 오류를 모아 한 번만 반환한다.
    pub fn take_error(&mut self) -> Option<String> {
        self.collect_error();
        self.pending_error.take()
    }

    /// 새 revision의 명령을 보내고 UI 요청 상태를 즉시 갱신한다.
    fn issue(&mut self, action: Action, snapshot: PlaybackSnapshot) {
        self.requested.revision = self.requested.revision.wrapping_add(1);
        self.requested.snapshot = snapshot;
        if let Some(commands) = &self.commands {
            if commands
                .send(Command {
                    revision: self.requested.revision,
                    action,
                })
                .is_err()
            {
                self.pending_error = Some("오디오 장치와의 연결이 끊어졌습니다.".to_owned());
                self.close_stream();
                self.requested.snapshot.state = PlaybackState::Stopped;
                self.shared = Arc::new(Shared::new(
                    self.requested.snapshot,
                    self.requested.revision,
                ));
            }
        } else {
            self.shared = Arc::new(Shared::new(snapshot, self.requested.revision));
        }
    }

    /// 비동기 장치 오류를 수집하고 스트림을 정리한다.
    fn collect_error(&mut self) {
        let error = self
            .errors
            .as_ref()
            .and_then(|errors| errors.try_recv().ok());
        if let Some(error) = error {
            let mut snapshot = self.snapshot();
            snapshot.state = PlaybackState::Stopped;
            self.close_stream();
            self.requested.snapshot = snapshot;
            self.shared = Arc::new(Shared::new(snapshot, self.requested.revision));
            self.pending_error = Some(error);
        }
    }

    /// 스트림을 먼저 닫고 명령·오류 채널과 이전 렌더러를 정리한다.
    fn close_stream(&mut self) {
        self.stream = None;
        self.commands = None;
        self.errors = None;
        self.sample_rate = None;
        self.retired = None;
    }

    /// 기본 출력 장치를 선택하고 지원 형식에 맞는 스트림을 연다.
    fn open_stream(&mut self, timeline: Arc<Timeline>) -> Result<()> {
        #[cfg(target_arch = "wasm32")]
        if !crate::web_audio::ready() {
            bail!("가상 악기를 먼저 불러와 주세요.");
        }
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("사용 가능한 오디오 출력 장치를 찾을 수 없습니다.")?;
        let supported = device
            .default_output_config()
            .context("오디오 장치의 출력 설정을 읽을 수 없습니다.")?;
        let sample_format = supported.sample_format();
        #[cfg(not(target_arch = "wasm32"))]
        let config: cpal::StreamConfig = supported.into();
        #[cfg(target_arch = "wasm32")]
        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(44_100),
            buffer_size: cpal::BufferSize::Fixed(2048),
        };
        if config.channels == 0 || config.sample_rate.0 == 0 {
            bail!("오디오 장치가 유효하지 않은 출력 설정을 반환했습니다.");
        }
        if !(16_000..=192_000).contains(&config.sample_rate.0) {
            bail!(
                "지원하지 않는 오디오 샘플레이트입니다: {} Hz",
                config.sample_rate.0
            );
        }
        let (commands_tx, commands_rx) = mpsc::channel();
        let (errors_tx, errors_rx) = mpsc::channel();
        let (retired_tx, retired_rx) = mpsc::channel();
        let snapshot = self.snapshot();
        let shared = Arc::new(Shared::new(snapshot, self.requested.revision));
        let mut transport = Transport::try_new(
            timeline,
            config.sample_rate.0,
            snapshot,
            self.volume,
            self.requested.revision,
            self.audibility.clone(),
        )?;
        transport.retired = Some(retired_tx);
        let stream = match sample_format {
            cpal::SampleFormat::I8 => build_stream::<i8>(
                &device,
                &config,
                transport,
                commands_rx,
                errors_tx,
                shared.clone(),
            ),
            cpal::SampleFormat::I16 => build_stream::<i16>(
                &device,
                &config,
                transport,
                commands_rx,
                errors_tx,
                shared.clone(),
            ),
            cpal::SampleFormat::I32 => build_stream::<i32>(
                &device,
                &config,
                transport,
                commands_rx,
                errors_tx,
                shared.clone(),
            ),
            cpal::SampleFormat::I64 => build_stream::<i64>(
                &device,
                &config,
                transport,
                commands_rx,
                errors_tx,
                shared.clone(),
            ),
            cpal::SampleFormat::U8 => build_stream::<u8>(
                &device,
                &config,
                transport,
                commands_rx,
                errors_tx,
                shared.clone(),
            ),
            cpal::SampleFormat::U16 => build_stream::<u16>(
                &device,
                &config,
                transport,
                commands_rx,
                errors_tx,
                shared.clone(),
            ),
            cpal::SampleFormat::U32 => build_stream::<u32>(
                &device,
                &config,
                transport,
                commands_rx,
                errors_tx,
                shared.clone(),
            ),
            cpal::SampleFormat::U64 => build_stream::<u64>(
                &device,
                &config,
                transport,
                commands_rx,
                errors_tx,
                shared.clone(),
            ),
            cpal::SampleFormat::F32 => build_stream::<f32>(
                &device,
                &config,
                transport,
                commands_rx,
                errors_tx,
                shared.clone(),
            ),
            cpal::SampleFormat::F64 => build_stream::<f64>(
                &device,
                &config,
                transport,
                commands_rx,
                errors_tx,
                shared.clone(),
            ),
            other => bail!("지원하지 않는 오디오 샘플 형식입니다: {other}"),
        }
        .context("오디오 출력 스트림을 만들 수 없습니다.")?;
        stream
            .play()
            .context("오디오 출력 장치를 시작할 수 없습니다.")?;
        self.stream = Some(stream);
        self.sample_rate = Some(config.sample_rate.0);
        self.retired = Some(retired_rx);
        self.commands = Some(commands_tx);
        self.errors = Some(errors_rx);
        self.shared = shared;
        Ok(())
    }
}

/// 출력 버퍼마다 명령을 처리하고 장치 형식으로 샘플을 채운다.
fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut transport: Transport,
    commands: Receiver<Command>,
    errors: Sender<String>,
    shared: Arc<Shared>,
) -> std::result::Result<cpal::Stream, cpal::BuildStreamError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let error_shared = shared.clone();
    device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            while let Ok(command) = commands.try_recv() {
                transport.command(command);
            }
            for frame in data.chunks_mut(channels) {
                let stereo = if shared.failed.load(Ordering::Relaxed) {
                    [0.0, 0.0]
                } else {
                    transport.next_frame()
                };
                write_frame(frame, stereo);
            }
            shared.publish(transport.published());
        },
        move |error| {
            error_shared.failed.store(true, Ordering::Release);
            let _ = errors.send(format!("오디오 출력 중 오류가 발생했습니다: {error}"));
        },
        None,
    )
}

/// 모노·스테레오 출력을 장치의 채널 수와 샘플 형식에 맞춘다.
fn write_frame<T: SizedSample + FromSample<f32>>(frame: &mut [T], stereo: [f32; 2]) {
    if frame.len() == 1 {
        frame[0] = T::from_sample((stereo[0] + stereo[1]) * 0.5);
    } else {
        for (channel, sample) in frame.iter_mut().enumerate() {
            *sample = T::from_sample(stereo.get(channel).copied().unwrap_or(0.0));
        }
    }
}

/// NaN을 0으로 바꾸고 재생 위치를 악보의 시간 범위로 제한한다.
fn clamp_position(seconds: f64, total_seconds: f64) -> f64 {
    if seconds.is_nan() {
        0.0
    } else {
        seconds.clamp(0.0, total_seconds.max(0.0))
    }
}

/// NaN을 0으로 바꾸고 음량을 0부터 1 사이로 제한한다.
fn clamp_volume(volume: f32) -> f32 {
    if volume.is_nan() {
        0.0
    } else {
        volume.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Note, Score, Track};

    /// 지정 길이의 테스트용 피아노 타임라인을 만든다.
    fn timeline(ticks: i64) -> Arc<Timeline> {
        Arc::new(
            Timeline::from_score(&Score {
                tracks: vec![Track {
                    parts: vec![vec![Note {
                        on: 0,
                        off: ticks,
                        pitch: 69,
                        vel: 12,
                        src: (0, 0),
                    }]],
                    ..Track::default()
                }],
                ..Score::default()
            })
            .unwrap(),
        )
    }

    /// 장치 없이 검사할 테스트 재생기를 만든다.
    fn transport() -> Transport {
        let timeline = timeline(192);
        let snapshot = PlaybackSnapshot {
            total_seconds: timeline.duration_seconds,
            ..PlaybackSnapshot::default()
        };
        let audibility = Audibility::all(&timeline);
        Transport::new(timeline, 48_000, snapshot, 0.65, 0, audibility)
    }

    /// 테스트 명령에 증가한 revision을 붙여 재생기에 전달한다.
    fn command(transport: &mut Transport, action: Action) {
        transport.command(Command {
            revision: transport.revision + 1,
            action,
        });
    }

    #[test]
    /// 일시정지가 샘플 시계를 보존하고 정지가 처음으로 돌아가는지 확인한다.
    fn pause_holds_sample_clock_and_stop_rewinds() {
        let mut transport = transport();
        command(&mut transport, Action::Play);
        for _ in 0..12_000 {
            transport.next_frame();
        }
        let before = transport.published().snapshot.position_seconds;
        assert!((before - 0.25).abs() < 1e-7);
        command(&mut transport, Action::Pause);
        for _ in 0..200 {
            assert_eq!(transport.next_frame(), [0.0, 0.0]);
        }
        assert_eq!(transport.published().snapshot.position_seconds, before);
        command(&mut transport, Action::Play);
        transport.next_frame();
        assert!(transport.published().snapshot.position_seconds > before);
        command(&mut transport, Action::Stop);
        assert_eq!(transport.state, PlaybackState::Stopped);
        assert_eq!(transport.published().snapshot.position_seconds, 0.0);
        assert_eq!(transport.next_frame(), [0.0, 0.0]);
    }

    #[test]
    /// 탐색의 상태 보존과 끝난 곡의 처음 재생을 확인한다.
    fn seek_preserves_state_and_end_replays_from_start() {
        let mut transport = transport();
        for state in [
            PlaybackState::Stopped,
            PlaybackState::Playing,
            PlaybackState::Paused,
        ] {
            transport.state = state;
            command(
                &mut transport,
                Action::Seek {
                    seconds: 0.5,
                    prepared: None,
                },
            );
            assert_eq!(transport.state, state);
            assert!((transport.published().snapshot.position_seconds - 0.5).abs() < 1e-7);
        }
        command(
            &mut transport,
            Action::Seek {
                seconds: f64::INFINITY,
                prepared: None,
            },
        );
        assert_eq!(
            transport.published().snapshot.position_seconds,
            transport.total_seconds
        );
        command(&mut transport, Action::Play);
        assert_eq!(transport.published().snapshot.position_seconds, 0.0);
        for _ in 0..96_000 {
            transport.next_frame();
        }
        assert_eq!(transport.state, PlaybackState::Stopped);
        assert_eq!(
            transport.published().snapshot.position_seconds,
            transport.total_seconds
        );
        command(&mut transport, Action::Play);
        assert_eq!(transport.state, PlaybackState::Playing);
        assert_eq!(transport.published().snapshot.position_seconds, 0.0);
    }

    #[test]
    /// 무음 상태의 조작이 장치를 열지 않는지 확인한다.
    fn silent_player_controls_do_not_open_device() {
        let mut player = Player::default();
        player.load(timeline(192));
        player.seek(0.4);
        assert_eq!(player.snapshot().position_seconds, 0.4);
        player.pause();
        assert_eq!(player.snapshot().state, PlaybackState::Stopped);
        player.set_volume(2.0);
        assert_eq!(player.volume, 1.0);
        player.set_volume(f32::NAN);
        assert_eq!(player.volume, 0.0);
        player.seek(f64::NAN);
        assert_eq!(player.snapshot().position_seconds, 0.0);
        assert!(player.stream.is_none());
        player.stop();
        assert_eq!(player.snapshot().state, PlaybackState::Stopped);
    }

    #[test]
    /// 오래된 콜백이 대기 중인 탐색 위치를 덮지 않는지 확인한다.
    fn pending_seek_is_not_overwritten_by_old_callback() {
        let mut player = Player::default();
        player.load(timeline(192));
        let (tx, rx) = mpsc::channel();
        player.commands = Some(tx);
        player.seek(0.7);
        player.shared.publish(Published {
            revision: 0,
            snapshot: PlaybackSnapshot::default(),
        });
        assert_eq!(player.snapshot().position_seconds, 0.7);
        let command = rx.try_recv().unwrap();
        player.shared.publish(Published {
            revision: command.revision,
            snapshot: PlaybackSnapshot {
                position_seconds: 0.71,
                ..player.requested.snapshot
            },
        });
        assert_eq!(player.snapshot().position_seconds, 0.71);
    }

    #[test]
    /// 새 악보가 이전 상태와 오류를 모두 교체하는지 확인한다.
    fn loading_replaces_all_old_score_state_and_errors() {
        let mut player = Player::default();
        player.load(timeline(192));
        let old_shared = player.shared.clone();
        let (tx, rx) = mpsc::channel();
        player.commands = Some(tx);
        player.seek(0.9);
        player.pending_error = Some("old failure".to_owned());
        player.load(timeline(96));
        old_shared.publish(Published {
            revision: u64::MAX,
            snapshot: PlaybackSnapshot {
                state: PlaybackState::Playing,
                position_seconds: 0.9,
                total_seconds: 1.0,
            },
        });
        old_shared.failed.store(true, Ordering::Release);
        assert_eq!(player.snapshot().state, PlaybackState::Stopped);
        assert_eq!(player.snapshot().position_seconds, 0.0);
        assert!((player.snapshot().total_seconds - 0.5).abs() < 1e-7);
        assert!(player.take_error().is_none());
        assert!(rx.try_recv().is_ok()); // The old queue cannot reach the new transport.
        assert!(rx.try_recv().is_err());
        assert!(player.commands.is_none());
    }

    #[test]
    /// 장치 오류가 재생을 멈추고 한 번만 보고되는지 확인한다.
    fn async_device_error_stops_transport_and_is_reported_once() {
        let mut player = Player::default();
        player.load(timeline(192));
        let (tx, rx) = mpsc::channel();
        player.errors = Some(rx);
        player.shared.failed.store(true, Ordering::Release);
        tx.send("device disconnected".to_owned()).unwrap();
        assert_eq!(player.snapshot().state, PlaybackState::Stopped);
        assert_eq!(player.take_error().as_deref(), Some("device disconnected"));
        assert!(player.take_error().is_none());
        assert!(player.errors.is_none());
        assert!(!player.shared.failed.load(Ordering::Acquire));
    }

    #[test]
    /// 샘플 형식과 채널 배치에 맞는 무음이 출력되는지 확인한다.
    fn output_formats_and_channel_mapping_have_correct_silence() {
        let mut mono = [0.0f32; 1];
        write_frame(&mut mono, [0.8, -0.4]);
        assert!((mono[0] - 0.2).abs() < 1e-6);
        let mut surround = [1.0f32; 6];
        write_frame(&mut surround, [0.8, -0.4]);
        assert_eq!(surround, [0.8, -0.4, 0.0, 0.0, 0.0, 0.0]);
        let mut signed = [1i16; 2];
        write_frame(&mut signed, [0.0, 0.0]);
        assert_eq!(signed, [0, 0]);
        let mut unsigned = [0u16; 2];
        write_frame(&mut unsigned, [0.0, 0.0]);
        assert_eq!(unsigned, [32768, 32768]);
    }

    #[test]
    /// 음소거가 시계를 멈추지 않고 무음으로 전환하는지 확인한다.
    fn muted_output_becomes_silent_without_stopping_clock() {
        let mut transport = transport();
        command(&mut transport, Action::Play);
        command(&mut transport, Action::Volume(0.0));
        for _ in 0..1_000 {
            transport.next_frame();
        }
        assert_eq!(transport.next_frame(), [0.0, 0.0]);
        assert!(transport.published().snapshot.position_seconds > 0.02);
        assert_eq!(transport.state, PlaybackState::Playing);
    }

    #[test]
    /// 선택 상태가 재개·장치 재연결까지 유지되고 새 악보에서 초기화되는지 확인한다.
    fn selections_survive_silent_controls_and_reopen_but_reset_on_load() {
        let mut player = Player::default();
        player.load(timeline(384));
        assert!(player.track_enabled(0));
        assert!(player.part_enabled(0, 0));
        assert!(!player.track_enabled(99));
        assert!(!player.part_enabled(0, 99));
        player.set_part_enabled(0, 0, false);
        player.set_track_enabled(0, false);
        player.set_track_enabled(0, true);
        assert!(!player.part_enabled(0, 0));
        player.set_part_enabled(99, 99, false);
        player.set_track_enabled(99, false);
        player.seek(0.4);
        player.pause();
        player.stop();
        player.seek(0.7);
        player.pending_error = Some("test disconnection".into());
        player.close_stream();
        assert!(!player.part_enabled(0, 0));
        assert_eq!(player.snapshot().position_seconds, 0.7);
        assert!(player.stream.is_none());
        // 실제 장치 없이 재연결 때 사용하는 생성자와 저장된 선택 상태를 검사한다.
        let mut reopened = Transport::new(
            player.timeline.clone().unwrap(),
            48_000,
            player.snapshot(),
            player.volume,
            player.requested.revision,
            player.audibility.clone(),
        );
        command(&mut reopened, Action::Play);
        for _ in 0..1000 {
            assert_eq!(reopened.next_frame(), [0.0; 2]);
        }
        assert!(reopened.published().snapshot.position_seconds > 0.7);
        player.load(timeline(96));
        assert!(player.track_enabled(0));
        assert!(player.part_enabled(0, 0));
        assert_eq!(player.snapshot().position_seconds, 0.0);
    }

    #[test]
    /// 재생 중 선택 변경과 일시정지 후 음소거가 위치나 소리 누출을 만들지 않는지 확인한다.
    fn live_commands_preserve_transport_and_paused_mute_has_no_resume_leak() {
        let mut transport = transport();
        command(&mut transport, Action::Play);
        for _ in 0..9600 {
            transport.next_frame();
        }
        command(&mut transport, Action::Pause);
        let position = transport.published().snapshot.position_seconds;
        command(
            &mut transport,
            Action::PartEnabled {
                track: 0,
                part: 0,
                enabled: false,
            },
        );
        assert_eq!(transport.state, PlaybackState::Paused);
        for _ in 0..1000 {
            assert_eq!(transport.next_frame(), [0.0; 2]);
        }
        assert_eq!(transport.published().snapshot.position_seconds, position);
        command(&mut transport, Action::Play);
        for _ in 0..1000 {
            assert_eq!(transport.next_frame(), [0.0; 2]);
        }
        assert!(transport.published().snapshot.position_seconds > position);
        let position = transport.published().snapshot.position_seconds;
        command(
            &mut transport,
            Action::PartEnabled {
                track: 0,
                part: 0,
                enabled: true,
            },
        );
        assert_eq!(transport.state, PlaybackState::Playing);
        assert_eq!(transport.published().snapshot.position_seconds, position);
        let energy: f32 = (0..4800).map(|_| transport.next_frame()[0].powi(2)).sum();
        assert!(energy > 0.001);
        // 페이드가 진행되기 전에 한 콜백에서 음소거와 일시정지가 함께 도착할 수 있다.
        command(
            &mut transport,
            Action::TrackEnabled {
                track: 0,
                enabled: false,
            },
        );
        command(&mut transport, Action::Pause);
        command(&mut transport, Action::Play);
        for _ in 0..480 {
            assert_eq!(transport.next_frame(), [0.0; 2]);
        }
        command(
            &mut transport,
            Action::Seek {
                seconds: 0.5,
                prepared: None,
            },
        );
        assert!(!transport.renderer.track_enabled(0));
        assert!(transport.renderer.part_enabled(0, 0));
        for _ in 0..4800 {
            assert_eq!(transport.next_frame(), [0.0; 2]);
        }
    }

    #[test]
    /// 악기 선택이 재생 조작을 거쳐 유지되고 새 악보에서 초기화되는지 확인한다.
    fn instrument_selection_survives_transport_controls_and_resets_on_load() {
        let mut player = Player::default();
        player.load(timeline(384));
        player.set_part_instrument(0, 0, Instrument::Flute);
        player.set_part_instrument(99, 99, Instrument::Trumpet);
        player.set_part_enabled(0, 0, false);
        player.seek(0.5);
        player.pause();
        player.stop();
        assert_eq!(player.part_instrument(0, 0), Instrument::Flute);
        assert_eq!(player.part_instrument(99, 99), Instrument::Piano);
        assert!(!player.part_enabled(0, 0));
        assert!(player.stream.is_none());
        player.load(timeline(96));
        assert_eq!(player.part_instrument(0, 0), Instrument::Piano);
        assert!(player.part_enabled(0, 0));
    }

    #[test]
    /// 준비된 탐색 렌더러가 일시정지와 파트 선택을 보존하는지 확인한다.
    fn prepared_instrument_seek_preserves_pause_and_selections() {
        let mut transport = transport();
        command(
            &mut transport,
            Action::PartInstrument {
                track: 0,
                part: 0,
                instrument: Instrument::Flute,
            },
        );
        command(&mut transport, Action::Play);
        for _ in 0..4800 {
            transport.next_frame();
        }
        command(&mut transport, Action::Pause);
        let source = timeline(192);
        let mut selections = Audibility::all(&source);
        selections.set_part_instrument(0, 0, Instrument::Flute);
        let mut prepared = Renderer::with_audibility(source, 48_000, selections);
        prepared.seek(0.5);
        command(
            &mut transport,
            Action::Seek {
                seconds: 0.5,
                prepared: Some(Box::new(prepared)),
            },
        );
        assert_eq!(transport.state, PlaybackState::Paused);
        assert_eq!(transport.published().snapshot.position_seconds, 0.5);
        assert_eq!(transport.renderer.part_instrument(0, 0), Instrument::Flute);
        assert_eq!(transport.next_frame(), [0.0; 2]);
        command(&mut transport, Action::Play);
        let energy: f32 = (0..4800).map(|_| transport.next_frame()[0].powi(2)).sum();
        assert!(
            energy > 0.001,
            "held flute should resume after the prepared seek"
        );
    }

    #[test]
    /// 정지 중 악기 변경이 첫 음을 다시 준비하고 이전 엔진을 UI에서 회수하는지 확인한다.
    fn stopped_program_change_rebuilds_the_primed_first_note_and_retires_on_ui() {
        let mut player = Player::default();
        player.load(timeline(192));
        let mut transport = transport();
        command(&mut transport, Action::Stop);
        let (commands, received) = mpsc::channel();
        let (retired, reclaimed) = mpsc::channel();
        transport.retired = Some(retired);
        player.commands = Some(commands);
        player.sample_rate = Some(48_000);
        player.retired = Some(reclaimed);
        player.set_part_instrument(0, 0, Instrument::Flute);
        for cmd in received.try_iter() {
            transport.command(cmd);
        }
        assert_eq!(transport.renderer.part_instrument(0, 0), Instrument::Flute);
        assert_eq!(transport.state, PlaybackState::Stopped);
        let previous = player.retired.as_ref().unwrap().try_recv().unwrap();
        drop(previous);
        let source = timeline(192);
        let mut mix = Audibility::all(&source);
        mix.set_part_instrument(0, 0, Instrument::Flute);
        let mut reference = Transport::new(source, 48_000, player.snapshot(), 0.65, 0, mix);
        command(&mut reference, Action::Play);
        command(&mut transport, Action::Play);
        for _ in 0..24_000 {
            assert_eq!(transport.next_frame(), reference.next_frame());
        }
    }
}
