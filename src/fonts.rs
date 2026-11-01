//! The interface's typeface beyond the windows that name it.

use slint::fontique_011::fontique::GenericFamily;

/// Puts `family`, the typeface every window names as its
/// default-font-family (Theme.font-family), first among the sans-serif
/// fonts too, ahead of the system's. On Linux Slint draws the menus itself
/// (the menu the title bar's button opens, and right-clicking the video),
/// each as a window of its own that inherits no default-font-family and
/// asks for sans-serif instead; without this they would be in the system's
/// font and everything else in IBM Plex. On macOS and Windows the menus are
/// the system's own, in its menu font. The system's sans-serif fonts stay
/// behind `family`, for text it has no glyphs for.
///
/// To be called once a window exists: creating one registers the font
/// files compiled into Patina, and before that the family may be unknown.
pub fn lead_sans_serif(family: &str) {
    let mut collection = slint::fontique_011::shared_collection();
    let Some(id) = collection.family_id(family) else {
        log::warn!("no font family {family:?}; menus use the system's sans-serif font");
        return;
    };
    let current: Vec<_> = collection
        .generic_families(GenericFamily::SansSerif)
        .collect();
    if current.first() == Some(&id) {
        return;
    }
    let rest = current.into_iter().filter(|other| *other != id);
    collection.set_generic_families(GenericFamily::SansSerif, std::iter::once(id).chain(rest));
}
