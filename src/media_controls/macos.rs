//! MediaPlayer's remote command center and now-playing center on macOS:
//! media keys, Control Center and the lock screen. Both must be used from
//! the main thread, which is where Patina's interface runs.

use super::{Command, Handler, NowPlaying, Status};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use objc2_media_player::{
    MPChangePlaybackPositionCommandEvent, MPMediaItemPropertyAlbumTitle, MPMediaItemPropertyArtist,
    MPMediaItemPropertyPlaybackDuration, MPMediaItemPropertyTitle, MPNowPlayingInfoCenter,
    MPNowPlayingInfoPropertyElapsedPlaybackTime, MPNowPlayingInfoPropertyPlaybackRate,
    MPNowPlayingPlaybackState, MPRemoteCommand, MPRemoteCommandCenter, MPRemoteCommandEvent,
    MPRemoteCommandHandlerStatus,
};
use std::cell::RefCell;
use std::ptr::NonNull;

/// Every Objective-C object is an `AnyObject`, which the info dictionary holds.
fn object<T: objc2::Message>(value: Retained<T>) -> Retained<AnyObject> {
    // SAFETY: an upcast to the root object type.
    unsafe { Retained::cast_unchecked(value) }
}

pub struct NowPlayingCenter {
    center: Retained<MPNowPlayingInfoCenter>,
    now: RefCell<NowPlaying>,
    position: std::cell::Cell<f64>,
    /// The handler tokens; commands stay registered while they live.
    _targets: Vec<Retained<AnyObject>>,
}

fn add(
    command: &MPRemoteCommand,
    handler: &Handler,
    make: impl Fn(NonNull<MPRemoteCommandEvent>) -> Option<Command> + 'static,
) -> Retained<AnyObject> {
    let handler = handler.clone();
    let block = RcBlock::new(move |event: NonNull<MPRemoteCommandEvent>| {
        if let Some(command) = make(event) {
            handler(command);
        }
        MPRemoteCommandHandlerStatus::Success
    });
    // SAFETY: called on the main thread; the block lives as long as the
    // returned target token, which the center keeps.
    unsafe {
        command.setEnabled(true);
        command.addTargetWithHandler(&block)
    }
}

impl NowPlayingCenter {
    pub fn start(handler: Handler) -> Option<Self> {
        // SAFETY: main-thread calls into MediaPlayer with valid objects.
        unsafe {
            let commands = MPRemoteCommandCenter::sharedCommandCenter();
            let targets = vec![
                add(&commands.playCommand(), &handler, |_| Some(Command::Play)),
                add(&commands.pauseCommand(), &handler, |_| Some(Command::Pause)),
                add(&commands.togglePlayPauseCommand(), &handler, |_| {
                    Some(Command::PlayPause)
                }),
                add(&commands.stopCommand(), &handler, |_| Some(Command::Stop)),
                add(&commands.nextTrackCommand(), &handler, |_| {
                    Some(Command::Next)
                }),
                add(&commands.previousTrackCommand(), &handler, |_| {
                    Some(Command::Previous)
                }),
                add(&commands.skipForwardCommand(), &handler, |_| {
                    Some(Command::Seek(15.0))
                }),
                add(&commands.skipBackwardCommand(), &handler, |_| {
                    Some(Command::Seek(-15.0))
                }),
                add(
                    &commands.changePlaybackPositionCommand(),
                    &handler,
                    |event| {
                        let event = event.cast::<MPChangePlaybackPositionCommandEvent>();
                        Some(Command::SetPosition(event.as_ref().positionTime()))
                    },
                ),
            ];
            Some(Self {
                center: MPNowPlayingInfoCenter::defaultCenter(),
                now: RefCell::new(NowPlaying::default()),
                position: std::cell::Cell::new(0.0),
                _targets: targets,
            })
        }
    }

    fn publish(&self) {
        let now = self.now.borrow();
        // SAFETY: main-thread calls with valid Foundation objects; the
        // dictionary holds NSString and NSNumber values as MediaPlayer expects.
        unsafe {
            if now.status == Status::Stopped && now.title.is_empty() {
                self.center.setNowPlayingInfo(None);
                self.center
                    .setPlaybackState(MPNowPlayingPlaybackState::Stopped);
                return;
            }
            let mut keys: Vec<&NSString> = Vec::new();
            let mut values: Vec<Retained<AnyObject>> = Vec::new();
            let mut text = |key: &'static NSString, value: &str| {
                if !value.is_empty() {
                    keys.push(key);
                    values.push(object(NSString::from_str(value)));
                }
            };
            text(MPMediaItemPropertyTitle, &now.title);
            text(MPMediaItemPropertyArtist, &now.artist);
            text(MPMediaItemPropertyAlbumTitle, &now.album);
            let mut number = |key: &'static NSString, value: f64| {
                keys.push(key);
                values.push(object(NSNumber::numberWithDouble(value)));
            };
            number(MPMediaItemPropertyPlaybackDuration, now.duration);
            number(
                MPNowPlayingInfoPropertyElapsedPlaybackTime,
                self.position.get(),
            );
            number(
                MPNowPlayingInfoPropertyPlaybackRate,
                if now.status == Status::Playing {
                    now.rate
                } else {
                    0.0
                },
            );
            let values: Vec<&AnyObject> = values.iter().map(|value| &**value).collect();
            let info = NSDictionary::from_slices(&keys, &values);
            self.center.setNowPlayingInfo(Some(&info));
            self.center.setPlaybackState(match now.status {
                Status::Playing => MPNowPlayingPlaybackState::Playing,
                Status::Paused => MPNowPlayingPlaybackState::Paused,
                Status::Stopped => MPNowPlayingPlaybackState::Stopped,
            });
        }
    }

    pub fn update(&self, now: &NowPlaying) {
        *self.now.borrow_mut() = now.clone();
        self.publish();
    }

    /// macOS extrapolates the position from the elapsed time and rate.
    pub fn set_position(&self, seconds: f64) {
        self.position.set(seconds);
    }

    pub fn seeked(&self, seconds: f64) {
        self.position.set(seconds);
        self.publish();
    }
}
