//! The System Media Transport Controls on Windows: media keys, the volume
//! flyout's media panel, and the lock screen.

use super::{Command, Handler, LoopMode, NowPlaying, Status};
use std::cell::Cell;
use std::time::Instant;
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Media::{
    AutoRepeatModeChangeRequestedEventArgs, MediaPlaybackAutoRepeatMode, MediaPlaybackStatus,
    MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
    PlaybackRateChangeRequestedEventArgs, ShuffleEnabledChangeRequestedEventArgs,
    SystemMediaTransportControls, SystemMediaTransportControlsButton,
    SystemMediaTransportControlsButtonPressedEventArgs,
    SystemMediaTransportControlsTimelineProperties,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;
use windows::core::{HSTRING, Ref, factory};

pub struct Smtc {
    controls: SystemMediaTransportControls,
    duration: Cell<f64>,
    /// The latest position, and the status it was last given with: Windows
    /// reports the position it was last told, plus the time since while
    /// playing, so a pause or a resume has to come with the position.
    position: Cell<f64>,
    status: Cell<Option<Status>>,
    last_timeline: Cell<Option<Instant>>,
}

/// SMTC times are in 100 ns units.
fn span(seconds: f64) -> TimeSpan {
    TimeSpan {
        Duration: (seconds.max(0.0) * 10_000_000.0) as i64,
    }
}

impl Smtc {
    pub fn start(handler: Handler, window: raw_window_handle::RawWindowHandle) -> Option<Self> {
        let raw_window_handle::RawWindowHandle::Win32(handle) = window else {
            return None;
        };
        let hwnd = HWND(handle.hwnd.get() as *mut core::ffi::c_void);
        let result = (|| -> windows::core::Result<SystemMediaTransportControls> {
            let interop =
                factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>()?;
            // SAFETY: `hwnd` is a live top-level window owned by this process.
            let controls: SystemMediaTransportControls = unsafe { interop.GetForWindow(hwnd)? };
            controls.SetIsEnabled(true)?;
            controls.SetIsPlayEnabled(true)?;
            controls.SetIsPauseEnabled(true)?;
            controls.SetIsStopEnabled(true)?;
            controls.SetIsFastForwardEnabled(true)?;
            controls.SetIsRewindEnabled(true)?;

            let buttons = handler.clone();
            controls.ButtonPressed(&TypedEventHandler::new(
                move |_, args: Ref<SystemMediaTransportControlsButtonPressedEventArgs>| {
                    if let Some(args) = args.as_ref() {
                        let command = match args.Button()? {
                            SystemMediaTransportControlsButton::Play => Some(Command::Play),
                            SystemMediaTransportControlsButton::Pause => Some(Command::Pause),
                            SystemMediaTransportControlsButton::Stop => Some(Command::Stop),
                            SystemMediaTransportControlsButton::Next => Some(Command::Next),
                            SystemMediaTransportControlsButton::Previous => Some(Command::Previous),
                            SystemMediaTransportControlsButton::FastForward => {
                                Some(Command::Seek(10.0))
                            }
                            SystemMediaTransportControlsButton::Rewind => {
                                Some(Command::Seek(-10.0))
                            }
                            _ => None,
                        };
                        if let Some(command) = command {
                            buttons(command);
                        }
                    }
                    Ok(())
                },
            ))?;

            let position = handler.clone();
            controls.PlaybackPositionChangeRequested(&TypedEventHandler::new(
                move |_, args: Ref<PlaybackPositionChangeRequestedEventArgs>| {
                    if let Some(args) = args.as_ref() {
                        let requested = args.RequestedPlaybackPosition()?;
                        position(Command::SetPosition(
                            requested.Duration as f64 / 10_000_000.0,
                        ));
                    }
                    Ok(())
                },
            ))?;

            let rate = handler.clone();
            controls.PlaybackRateChangeRequested(&TypedEventHandler::new(
                move |_, args: Ref<PlaybackRateChangeRequestedEventArgs>| {
                    if let Some(args) = args.as_ref() {
                        rate(Command::SetRate(args.RequestedPlaybackRate()?));
                    }
                    Ok(())
                },
            ))?;

            let shuffle = handler.clone();
            controls.ShuffleEnabledChangeRequested(&TypedEventHandler::new(
                move |_, args: Ref<ShuffleEnabledChangeRequestedEventArgs>| {
                    if let Some(args) = args.as_ref() {
                        shuffle(Command::SetShuffle(args.RequestedShuffleEnabled()?));
                    }
                    Ok(())
                },
            ))?;

            let repeat = handler;
            controls.AutoRepeatModeChangeRequested(&TypedEventHandler::new(
                move |_, args: Ref<AutoRepeatModeChangeRequestedEventArgs>| {
                    if let Some(args) = args.as_ref() {
                        let mode = match args.RequestedAutoRepeatMode()? {
                            MediaPlaybackAutoRepeatMode::Track => LoopMode::Track,
                            MediaPlaybackAutoRepeatMode::List => LoopMode::Playlist,
                            _ => LoopMode::None,
                        };
                        repeat(Command::SetLoop(mode));
                    }
                    Ok(())
                },
            ))?;
            Ok(controls)
        })();
        match result {
            Ok(controls) => Some(Self {
                controls,
                duration: Cell::new(0.0),
                position: Cell::new(0.0),
                status: Cell::new(None),
                last_timeline: Cell::new(None),
            }),
            Err(error) => {
                log::warn!("media keys are unavailable: {error}");
                None
            }
        }
    }

    pub fn update(&self, now: &NowPlaying) {
        let result = (|| -> windows::core::Result<()> {
            let controls = &self.controls;
            controls.SetPlaybackStatus(match now.status {
                Status::Playing => MediaPlaybackStatus::Playing,
                Status::Paused => MediaPlaybackStatus::Paused,
                Status::Stopped => MediaPlaybackStatus::Stopped,
            })?;
            controls.SetIsNextEnabled(now.can_next)?;
            controls.SetIsPreviousEnabled(now.can_previous)?;
            controls.SetShuffleEnabled(now.shuffle)?;
            controls.SetAutoRepeatMode(match now.loop_mode {
                LoopMode::None => MediaPlaybackAutoRepeatMode::None,
                LoopMode::Track => MediaPlaybackAutoRepeatMode::Track,
                LoopMode::Playlist => MediaPlaybackAutoRepeatMode::List,
            })?;
            if now.rate > 0.0 {
                controls.SetPlaybackRate(now.rate)?;
            }

            let updater = controls.DisplayUpdater()?;
            updater.ClearAll()?;
            if now.audio_only {
                updater.SetType(MediaPlaybackType::Music)?;
                let music = updater.MusicProperties()?;
                music.SetTitle(&HSTRING::from(&now.title))?;
                music.SetArtist(&HSTRING::from(&now.artist))?;
                music.SetAlbumTitle(&HSTRING::from(&now.album))?;
            } else {
                updater.SetType(MediaPlaybackType::Video)?;
                updater
                    .VideoProperties()?
                    .SetTitle(&HSTRING::from(&now.title))?;
            }
            updater.Update()?;
            self.duration.set(now.duration);
            Ok(())
        })();
        if self.status.replace(Some(now.status)) != Some(now.status) {
            self.timeline(self.position.get());
        }
        if let Err(error) = result {
            log::debug!("could not update the media controls: {error}");
        }
    }

    fn timeline(&self, seconds: f64) {
        let result = (|| -> windows::core::Result<()> {
            let timeline = SystemMediaTransportControlsTimelineProperties::new()?;
            let duration = self.duration.get();
            timeline.SetStartTime(span(0.0))?;
            timeline.SetEndTime(span(duration))?;
            timeline.SetMinSeekTime(span(0.0))?;
            timeline.SetMaxSeekTime(span(duration))?;
            timeline.SetPosition(span(seconds))?;
            self.controls.UpdateTimelineProperties(&timeline)
        })();
        if let Err(error) = result {
            log::debug!("could not update the media timeline: {error}");
        }
        self.last_timeline.set(Some(Instant::now()));
    }

    /// Windows extrapolates the position itself; refreshing it every few
    /// seconds keeps it from drifting.
    pub fn set_position(&self, seconds: f64) {
        self.position.set(seconds);
        if self
            .last_timeline
            .get()
            .is_none_or(|last| last.elapsed().as_secs() >= 5)
        {
            self.timeline(seconds);
        }
    }

    pub fn seeked(&self, seconds: f64) {
        self.position.set(seconds);
        self.timeline(seconds);
    }
}
