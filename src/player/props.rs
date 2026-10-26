macro_rules! props {
    ($($variant:ident => $name:literal,)*) => {
        /// The mpv properties every player observes. The discriminant is the
        /// observation's userdata.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(u64)]
        pub enum Prop {
            $($variant,)*
        }

        impl Prop {
            pub const ALL: &'static [Prop] = &[$(Prop::$variant,)*];

            pub fn name(self) -> &'static str {
                match self {
                    $(Prop::$variant => $name,)*
                }
            }

            pub fn from_userdata(userdata: u64) -> Option<Prop> {
                Self::ALL.get(userdata as usize).copied()
            }
        }
    };
}

props! {
    Idle => "idle-active",
    Pause => "pause",
    CoreIdle => "core-idle",
    PausedForCache => "paused-for-cache",
    CacheBuffering => "cache-buffering-state",
    Seeking => "seeking",
    Seekable => "seekable",
    EofReached => "eof-reached",
    TimePos => "time-pos",
    Duration => "duration",
    DemuxerCacheTime => "demuxer-cache-time",
    Volume => "volume",
    VolumeMax => "volume-max",
    Mute => "mute",
    Speed => "speed",
    MediaTitle => "media-title",
    Path => "path",
    Filename => "filename",
    FileFormat => "file-format",
    FileSize => "file-size",
    Metadata => "metadata",
    Playlist => "playlist",
    PlaylistPos => "playlist-pos",
    TrackList => "track-list",
    ChapterList => "chapter-list",
    Chapter => "chapter",
    Vid => "vid",
    Aid => "aid",
    Sid => "sid",
    SecondarySid => "secondary-sid",
    AudioDelay => "audio-delay",
    SubDelay => "sub-delay",
    SecondarySubDelay => "secondary-sub-delay",
    SubScale => "sub-scale",
    SubPos => "sub-pos",
    SubVisibility => "sub-visibility",
    SecondarySubVisibility => "secondary-sub-visibility",
    VideoParams => "video-params",
    VideoAspectOverride => "video-aspect-override",
    VideoRotate => "video-rotate",
    VideoCrop => "video-crop",
    VideoZoom => "video-zoom",
    VideoFilters => "vf",
    Deinterlace => "deinterlace",
    Hwdec => "hwdec",
    HwdecCurrent => "hwdec-current",
    Brightness => "brightness",
    Contrast => "contrast",
    Saturation => "saturation",
    Gamma => "gamma",
    Hue => "hue",
    LoopFile => "loop-file",
    LoopPlaylist => "loop-playlist",
    AbLoopA => "ab-loop-a",
    AbLoopB => "ab-loop-b",
    Shuffle => "shuffle",
    AudioFilters => "af",
    AudioDeviceList => "audio-device-list",
    AudioDevice => "audio-device",
    ContainerFps => "container-fps",
    EstimatedVfFps => "estimated-vf-fps",
    AudioParams => "audio-params",
    AudioCodecName => "audio-codec-name",
    VideoCodec => "video-codec",
    Fullscreen => "fullscreen",
    Ontop => "ontop",
    WindowScale => "window-scale",
}

#[cfg(test)]
mod tests {
    use super::Prop;

    #[test]
    fn userdata_maps_back_to_the_property() {
        for (index, prop) in Prop::ALL.iter().enumerate() {
            assert_eq!(*prop as usize, index);
            assert_eq!(Prop::from_userdata(index as u64), Some(*prop));
        }
        assert_eq!(Prop::from_userdata(Prop::ALL.len() as u64), None);
    }
}
