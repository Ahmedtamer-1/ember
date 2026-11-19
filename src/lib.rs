//! Ember browser core library.

pub mod adblock;
pub mod app;
pub mod context_menu;
pub mod downloads;
pub mod find;
pub mod pages;
pub mod permissions;
pub mod popup;
pub mod privacy;
pub mod settings;
pub mod storage;
pub mod tabs;
pub mod theme;
pub mod ui;
pub mod zoom;

pub use app::run;
