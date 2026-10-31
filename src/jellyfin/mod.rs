//! Patina's Jellyfin client: browsing a server's library in the player
//! window and playing from it. docs/jellyfin.md is the design.

pub mod api;
pub mod auth;
pub mod discovery;
pub mod images;
pub mod library;
pub mod models;
pub mod playback;
pub mod segments;
pub mod session;
pub mod store;
pub mod trickplay;

#[cfg(test)]
mod tests;
