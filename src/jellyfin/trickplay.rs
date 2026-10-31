//! Seek-bar previews from a server's trickplay images. Patina makes its
//! own thumbnails of local files only; for a Jellyfin item the server has
//! made them already, when its library has trickplay on, as tiles of
//! thumbnails in a grid. A tile only a signed-in request gets is fetched
//! with the account's client on one of the library's load threads and
//! decoded there; a few are kept decoded, and each thumbnail is cut out of
//! its tile as the pointer reaches it.

use super::api::Client;
use super::models::TrickplayInfo;
use super::playback::Playing;
use slint::{Image, Rgb8Pixel, SharedPixelBuffer};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Decoded tiles kept at once: the one under the pointer and the last
/// before it. A tile of ten by ten thumbnails 320 pixels wide, the server's
/// default, is some 17 MB decoded.
const TILES_KEPT: usize = 2;
/// A tile that failed isn't asked for again for this long, so that moving
/// along the bar doesn't ask the server for it over and over.
const RETRY_AFTER: Duration = Duration::from_secs(30);

/// Of a version's sizes, narrowest first, the narrowest at least `width`
/// pixels wide, or else the widest there is.
pub fn pick(sizes: &[TrickplayInfo], width: u32) -> Option<&TrickplayInfo> {
    sizes
        .iter()
        .find(|info| info.width >= width)
        .or(sizes.last())
}

/// The thumbnail at `x`, `y` of a decoded tile, `info`'s size. None when
/// the tile is smaller than that: the last tile holds only the thumbnails
/// left, and a server's image needn't be what it said.
pub fn crop(
    tile: &image::RgbImage,
    info: &TrickplayInfo,
    x: u32,
    y: u32,
) -> Option<SharedPixelBuffer<Rgb8Pixel>> {
    let (width, height) = (info.width, info.height);
    let fits = |at: u32, size: u32, room: u32| {
        size > 0 && at.checked_add(size).is_some_and(|end| end <= room)
    };
    if !fits(x, width, tile.width()) || !fits(y, height, tile.height()) {
        return None;
    }
    let view = image::imageops::crop_imm(tile, x, y, width, height).to_image();
    Some(SharedPixelBuffer::clone_from_slice(
        view.as_raw(),
        width,
        height,
    ))
}

/// Decodes a tile the server sent, within the image cache's limits.
fn decode(bytes: &[u8]) -> Result<image::RgbImage, String> {
    Ok(super::images::decode_image(bytes)?.into_rgb8())
}

/// One playback's trickplay tiles, on the interface thread.
pub struct Tiles {
    client: Client,
    item_id: String,
    media_source_id: String,
    info: TrickplayInfo,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    /// By tile index, the most recently used last.
    decoded: VecDeque<(u32, Rc<image::RgbImage>)>,
    fetching: HashSet<u32>,
    failed: HashMap<u32, Instant>,
    /// The thumbnail cut out last, by tile and corner: the pointer asks
    /// for the same one many times as it moves over the bar.
    last: Option<((u32, u32, u32), Image)>,
}

impl Tiles {
    /// The tiles of the version `playing` plays, in the size nearest
    /// `width` pixels; None when the server has none.
    pub fn new(client: Client, playing: &Playing, width: u32) -> Option<Rc<Self>> {
        let info = pick(&playing.trickplay, width)?.clone();
        // A layout that locates nothing has no thumbnails to show.
        info.locate(0.0)?;
        Some(Rc::new(Self {
            client,
            item_id: playing.item_id.clone(),
            media_source_id: playing.media_source_id.clone(),
            info,
            state: RefCell::default(),
        }))
    }

    /// The thumbnail for `seconds` when its tile is here. Otherwise None,
    /// and the tile is fetched; `arrived` is called once it is, later, on
    /// this thread, to ask again.
    pub fn thumbnail(
        self: &Rc<Self>,
        seconds: f64,
        arrived: impl FnOnce() + 'static,
    ) -> Option<Image> {
        let (index, x, y) = self.info.locate(seconds)?;
        let mut state = self.state.borrow_mut();
        if let Some((at, image)) = &state.last
            && *at == (index, x, y)
        {
            return Some(image.clone());
        }
        if let Some(position) = state.decoded.iter().position(|(at, _)| *at == index) {
            // The most recently used goes last.
            let tile = state.decoded.remove(position)?;
            state.decoded.push_back(tile.clone());
            let image = Image::from_rgb8(crop(&tile.1, &self.info, x, y)?);
            state.last = Some(((index, x, y), image.clone()));
            return Some(image);
        }
        if state
            .failed
            .get(&index)
            .is_some_and(|failed| failed.elapsed() < RETRY_AFTER)
            || !state.fetching.insert(index)
        {
            return None;
        }
        drop(state);
        self.fetch(index, arrived);
        None
    }

    fn fetch(self: &Rc<Self>, index: u32, arrived: impl FnOnce() + 'static) {
        let client = self.client.clone();
        let url = client.trickplay_tile_url(
            &self.item_id,
            self.info.width,
            index,
            Some(&self.media_source_id),
        );
        let weak = Rc::downgrade(self);
        super::session::load(
            move || {
                client
                    .get_bytes(&url)
                    .map_err(|error| error.to_string())
                    .and_then(|bytes| decode(&bytes))
            },
            move |decoded| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                let mut state = this.state.borrow_mut();
                state.fetching.remove(&index);
                match decoded {
                    Ok(tile) => {
                        state.decoded.push_back((index, Rc::new(tile)));
                        while state.decoded.len() > TILES_KEPT {
                            state.decoded.pop_front();
                        }
                        drop(state);
                        arrived();
                    }
                    Err(error) => {
                        log::debug!("no trickplay tile {index}: {error}");
                        state.failed.insert(index, Instant::now());
                    }
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> TrickplayInfo {
        TrickplayInfo {
            width: 4,
            height: 2,
            tile_width: 3,
            tile_height: 2,
            thumbnail_count: 8,
            interval: 1000,
            bandwidth: 0,
        }
    }

    /// A tile whose every pixel says where it is: red its column, green its
    /// row.
    fn tile(width: u32, height: u32) -> image::RgbImage {
        image::RgbImage::from_fn(width, height, |x, y| image::Rgb([x as u8, y as u8, 7]))
    }

    #[test]
    fn a_thumbnail_is_cut_from_its_place_in_the_tile() {
        let info = layout();
        let full = tile(12, 4);
        // The fifth thumbnail: second row, second column.
        let (index, x, y) = info.locate(4.5).unwrap();
        assert_eq!((index, x, y), (0, 4, 2));
        let thumbnail = crop(&full, &info, x, y).unwrap();
        assert_eq!((thumbnail.width(), thumbnail.height()), (4, 2));
        let pixels = thumbnail.as_slice();
        assert_eq!(pixels[0], Rgb8Pixel { r: 4, g: 2, b: 7 });
        assert_eq!(pixels[7], Rgb8Pixel { r: 7, g: 3, b: 7 });
        // The second tile holds the last two, in one row it is cut to.
        let (index, x, y) = info.locate(7.0).unwrap();
        assert_eq!((index, x, y), (1, 4, 0));
        let short = tile(8, 2);
        let thumbnail = crop(&short, &info, x, y).unwrap();
        assert_eq!(thumbnail.as_slice()[0], Rgb8Pixel { r: 4, g: 0, b: 7 });
    }

    #[test]
    fn a_thumbnail_outside_the_tile_is_none() {
        let info = layout();
        // A tile cut shorter than its layout says, or narrower.
        assert!(crop(&tile(12, 2), &info, 0, 2).is_none());
        assert!(crop(&tile(7, 4), &info, 4, 0).is_none());
        assert!(crop(&tile(12, 4), &info, u32::MAX, 0).is_none());
        let empty = TrickplayInfo {
            width: 0,
            ..layout()
        };
        assert!(crop(&tile(12, 4), &empty, 0, 0).is_none());
    }

    #[test]
    fn the_size_nearest_the_preview_is_picked() {
        let size = |width| TrickplayInfo { width, ..layout() };
        let sizes = [size(160), size(320), size(640)];
        assert_eq!(pick(&sizes, 240).unwrap().width, 320);
        assert_eq!(pick(&sizes, 320).unwrap().width, 320);
        assert_eq!(pick(&sizes, 100).unwrap().width, 160);
        assert_eq!(pick(&sizes, 1200).unwrap().width, 640);
        assert!(pick(&[], 320).is_none());
    }
}
