#![cfg(target_os = "macos")]
#![allow(non_upper_case_globals)]

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use block::ConcreteBlock;
use cocoa::{
    base::{id, nil, NO, YES},
    foundation::{NSInteger, NSSize, NSString, NSUInteger},
};
use core_graphics::geometry::CGSize;
use dispatch::{Queue, QueuePriority};
use objc::{class, msg_send, sel, sel_impl};

use crate::{MediaControlEvent, MediaMetadata, MediaPlayback, PlatformConfig};

#[derive(Debug)]
pub struct Error;

pub struct MediaControls;

impl MediaControls {
    pub fn new(_config: PlatformConfig) -> Self {
        Self
    }

    pub fn attach<F>(&mut self, event_handler: F) -> Result<(), Error>
    where
        F: Fn(MediaControlEvent) + Send + 'static,
    {
        unsafe { attach_command_handlers(Arc::new(event_handler)) };
        Ok(())
    }

    pub fn detach(&mut self) -> Result<(), Error> {
        unsafe { detach_command_handlers() };
        Ok(())
    }

    pub fn set_playback(&mut self, playback: MediaPlayback) -> Result<(), Error> {
        unsafe { set_playback_status(playback) };
        Ok(())
    }

    pub fn set_metadata(&mut self, metadata: MediaMetadata) -> Result<(), Error> {
        unsafe { set_playback_metadata(metadata) };
        Ok(())
    }
}

// MPNowPlayingPlaybackState
const MPNowPlayingPlaybackStatePlaying: NSUInteger = 1;
const MPNowPlayingPlaybackStatePaused: NSUInteger = 2;
const MPNowPlayingPlaybackStateStopped: NSUInteger = 3;

// MPRemoteCommandHandlerStatus
const MPRemoteCommandHandlerStatusSuccess: NSInteger = 0;

extern "C" {
    static MPMediaItemPropertyTitle: id; // NSString
    static MPMediaItemPropertyArtist: id; // NSString
    static MPMediaItemPropertyAlbumTitle: id; // NSString
    static MPMediaItemPropertyArtwork: id; // NSString
    static MPMediaItemPropertyPlaybackDuration: id; // NSString
    static MPNowPlayingInfoPropertyElapsedPlaybackTime: id; // NSString
}

unsafe fn set_playback_status(playback: MediaPlayback) {
    let media_center: id = msg_send!(class!(MPNowPlayingInfoCenter), defaultCenter);
    let state = match playback {
        MediaPlayback::Stopped => MPNowPlayingPlaybackStateStopped,
        MediaPlayback::Paused { .. } => MPNowPlayingPlaybackStatePaused,
        MediaPlayback::Playing { .. } => MPNowPlayingPlaybackStatePlaying,
    };
    let _: () = msg_send!(media_center, setPlaybackState: state);
    if let MediaPlayback::Paused {
        progress: Some(progress),
    }
    | MediaPlayback::Playing {
        progress: Some(progress),
    } = playback
    {
        set_playback_progress(progress.0);
    }
}

static GLOBAL_METADATA_COUNTER: AtomicUsize = AtomicUsize::new(1);

unsafe fn set_playback_metadata(metadata: MediaMetadata) {
    let prev_counter = GLOBAL_METADATA_COUNTER.fetch_add(1, Ordering::SeqCst);
    let media_center: id = msg_send!(class!(MPNowPlayingInfoCenter), defaultCenter);
    let now_playing: id = msg_send!(class!(NSMutableDictionary), dictionary);
    if let Some(title) = metadata.title {
        let _: () = msg_send!(now_playing, setObject: ns_string(title)
                                              forKey: MPMediaItemPropertyTitle);
    }
    if let Some(artist) = metadata.artist {
        let _: () = msg_send!(now_playing, setObject: ns_string(artist)
                                              forKey: MPMediaItemPropertyArtist);
    }
    if let Some(album) = metadata.album {
        let _: () = msg_send!(now_playing, setObject: ns_string(album)
                                              forKey: MPMediaItemPropertyAlbumTitle);
    }
    if let Some(duration) = metadata.duration {
        let _: () = msg_send!(now_playing, setObject: ns_number(duration.as_secs_f64())
                                              forKey: MPMediaItemPropertyPlaybackDuration);
    }
    if let Some(cover_url) = metadata.cover_url {
        let cover_url = cover_url.to_owned();
        Queue::global(QueuePriority::Default).exec_async(move || {
            load_and_set_playback_artwork(ns_url(&cover_url), prev_counter + 1);
        });
    }
    let _: () = msg_send!(media_center, setNowPlayingInfo: now_playing);
}

unsafe fn load_and_set_playback_artwork(url: id, for_counter: usize) {
    let (image, size) = ns_image_from_url(url);
    let artwork = mp_artwork(image, size);
    if GLOBAL_METADATA_COUNTER.load(Ordering::SeqCst) == for_counter {
        set_playback_artwork(artwork);
    }
}

unsafe fn set_playback_artwork(artwork: id) {
    let media_center: id = msg_send!(class!(MPNowPlayingInfoCenter), defaultCenter);
    let now_playing: id = msg_send!(class!(NSMutableDictionary), dictionary);
    let prev_now_playing: id = msg_send!(media_center, nowPlayingInfo);
    let _: () = msg_send!(now_playing, addEntriesFromDictionary: prev_now_playing);
    let _: () = msg_send!(now_playing, setObject: artwork
                                          forKey: MPMediaItemPropertyArtwork);
    let _: () = msg_send!(media_center, setNowPlayingInfo: now_playing);
}

unsafe fn set_playback_progress(progress: Duration) {
    let media_center: id = msg_send!(class!(MPNowPlayingInfoCenter), defaultCenter);
    let now_playing: id = msg_send!(class!(NSMutableDictionary), dictionary);
    let prev_now_playing: id = msg_send!(media_center, nowPlayingInfo);
    let _: () = msg_send!(now_playing, addEntriesFromDictionary: prev_now_playing);
    let _: () = msg_send!(now_playing, setObject: ns_number(progress.as_secs_f64())
                                          forKey: MPNowPlayingInfoPropertyElapsedPlaybackTime);
    let _: () = msg_send!(media_center, setNowPlayingInfo: now_playing);
}

unsafe fn attach_command_handlers(handler: Arc<dyn Fn(MediaControlEvent)>) {
    let command_center: id = msg_send!(class!(MPRemoteCommandCenter), sharedCommandCenter);

    // togglePlayPauseCommand
    let play_pause_handler = ConcreteBlock::new({
        let handler = handler.clone();
        move |_event: id| -> NSInteger {
            (handler)(MediaControlEvent::Toggle);
            MPRemoteCommandHandlerStatusSuccess
        }
    })
    .copy();
    let cmd: id = msg_send!(command_center, togglePlayPauseCommand);
    let _: () = msg_send!(cmd, setEnabled: YES);
    let _: () = msg_send!(cmd, addTargetWithHandler: play_pause_handler);

    // playCommand
    let play_handler = ConcreteBlock::new({
        let handler = handler.clone();
        move |_event: id| -> NSInteger {
            (handler)(MediaControlEvent::Play);
            MPRemoteCommandHandlerStatusSuccess
        }
    })
    .copy();
    let cmd: id = msg_send!(command_center, playCommand);
    let _: () = msg_send!(cmd, setEnabled: YES);
    let _: () = msg_send!(cmd, addTargetWithHandler: play_handler);

    // pauseCommand
    let pause_handler = ConcreteBlock::new({
        let handler = handler.clone();
        move |_event: id| -> NSInteger {
            (handler)(MediaControlEvent::Pause);
            MPRemoteCommandHandlerStatusSuccess
        }
    })
    .copy();
    let cmd: id = msg_send!(command_center, pauseCommand);
    let _: () = msg_send!(cmd, setEnabled: YES);
    let _: () = msg_send!(cmd, addTargetWithHandler: pause_handler);

    // previousTrackCommand
    let previous_track_handler = ConcreteBlock::new({
        let handler = handler.clone();
        move |_event: id| -> NSInteger {
            (handler)(MediaControlEvent::Previous);
            MPRemoteCommandHandlerStatusSuccess
        }
    })
    .copy();
    let cmd: id = msg_send!(command_center, previousTrackCommand);
    let _: () = msg_send!(cmd, setEnabled: YES);
    let _: () = msg_send!(cmd, addTargetWithHandler: previous_track_handler);

    // nextTrackCommand
    let next_track_handler = ConcreteBlock::new({
        let handler = handler.clone();
        move |_event: id| -> NSInteger {
            (handler)(MediaControlEvent::Next);
            MPRemoteCommandHandlerStatusSuccess
        }
    })
    .copy();
    let cmd: id = msg_send!(command_center, nextTrackCommand);
    let _: () = msg_send!(cmd, setEnabled: YES);
    let _: () = msg_send!(cmd, addTargetWithHandler: next_track_handler);
}

unsafe fn detach_command_handlers() {
    let command_center: id = msg_send!(class!(MPRemoteCommandCenter), sharedCommandCenter);

    let cmd: id = msg_send!(command_center, togglePlayPauseCommand);
    let _: () = msg_send!(cmd, setEnabled: NO);
    let _: () = msg_send!(cmd, removeTarget: nil);

    let cmd: id = msg_send!(command_center, playCommand);
    let _: () = msg_send!(cmd, setEnabled: NO);
    let _: () = msg_send!(cmd, removeTarget: nil);

    let cmd: id = msg_send!(command_center, pauseCommand);
    let _: () = msg_send!(cmd, setEnabled: NO);
    let _: () = msg_send!(cmd, removeTarget: nil);

    let cmd: id = msg_send!(command_center, previousTrackCommand);
    let _: () = msg_send!(cmd, setEnabled: NO);
    let _: () = msg_send!(cmd, removeTarget: nil);

    let cmd: id = msg_send!(command_center, nextTrackCommand);
    let _: () = msg_send!(cmd, setEnabled: NO);
    let _: () = msg_send!(cmd, removeTarget: nil);
}

unsafe fn ns_string(value: &str) -> id {
    NSString::alloc(nil).init_str(value)
}

unsafe fn ns_number(value: f64) -> id {
    let number: id = msg_send!(class!(NSNumber), numberWithDouble: value);
    number
}

unsafe fn ns_url(value: &str) -> id {
    let url: id = msg_send!(class!(NSURL), URLWithString: ns_string(value));
    url
}

unsafe fn ns_image_from_url(url: id) -> (id, CGSize) {
    let image: id = msg_send!(class!(NSImage), alloc);
    let image: id = msg_send!(image, initWithContentsOfURL: url);
    let size: NSSize = msg_send!(image, size);
    (image, CGSize::new(size.width, size.height))
}

unsafe fn mp_artwork(image: id, bounds: CGSize) -> id {
    let handler = ConcreteBlock::new(move |_size: CGSize| -> id { image }).copy();
    let artwork: id = msg_send!(class!(MPMediaItemArtwork), alloc);
    let artwork: id = msg_send!(artwork, initWithBoundsSize: bounds
                                         requestHandler: handler);
    artwork
}
