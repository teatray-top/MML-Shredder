//! 실제 오디오 장치에서 짧은 음을 재생하며 기본 조작을 검사합니다.

use anyhow::{Result, ensure};
use mmlfold::{
    Note, Score, Track,
    playback::{PlaybackState, Player},
    synth::Timeline,
};
use std::{sync::Arc, thread, time::Duration};

/// 실제 장치에서 재생·일시정지·탐색·정지를 검사합니다.
fn main() -> Result<()> {
    let timeline = Arc::new(Timeline::from_score(&Score {
        tracks: vec![Track {
            parts: vec![vec![Note {
                on: 0,
                off: 192,
                pitch: 69,
                vel: 7,
                src: (0, 0),
            }]],
            ..Track::default()
        }],
        ..Score::default()
    })?);
    let mut player = Player::default();
    player.load(timeline.clone());
    player.set_volume(0.15);
    player.play()?;
    thread::sleep(Duration::from_millis(180));
    check_error(&mut player)?;
    let playing = player.snapshot();
    ensure!(playing.state == PlaybackState::Playing);
    ensure!(
        playing.position_seconds > 0.0,
        "audio callback did not advance"
    );
    println!("Playing at {:.3} s", playing.position_seconds);

    player.pause();
    thread::sleep(Duration::from_millis(60));
    let paused = player.snapshot();
    thread::sleep(Duration::from_millis(60));
    check_error(&mut player)?;
    ensure!(paused.state == PlaybackState::Paused);
    ensure!(player.snapshot().position_seconds == paused.position_seconds);
    println!("Pause held at {:.3} s", paused.position_seconds);

    player.seek(0.4);
    player.play()?;
    thread::sleep(Duration::from_millis(140));
    check_error(&mut player)?;
    ensure!(player.snapshot().position_seconds > 0.4);
    player.stop();
    thread::sleep(Duration::from_millis(60));
    ensure!(player.snapshot().state == PlaybackState::Stopped);
    ensure!(player.snapshot().position_seconds == 0.0);

    player.load(timeline);
    ensure!(player.snapshot().state == PlaybackState::Stopped);
    ensure!(player.snapshot().position_seconds == 0.0);
    check_error(&mut player)?;
    println!("Seek, stop, and score replacement passed");
    Ok(())
}

/// 재생 장치의 비동기 오류를 결과로 전달합니다.
fn check_error(player: &mut Player) -> Result<()> {
    if let Some(error) = player.take_error() {
        anyhow::bail!(error);
    }
    Ok(())
}
