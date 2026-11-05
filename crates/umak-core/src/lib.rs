//! Library, database, scanner and tag model.
//!
//! This crate must never depend on Tauri or UI code.

pub mod api;
pub mod db;
pub mod jobs;
pub mod library;
pub mod player;
pub mod playlists;
pub mod scan;
pub mod stats;
pub mod transfer;

use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct AppInfo {
    pub name: &'static str,
    pub version: &'static str,
}

pub fn app_info() -> AppInfo {
    AppInfo {
        name: "Umak",
        version: env!("CARGO_PKG_VERSION"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_info_reports_the_workspace_version() {
        assert_eq!(app_info().version, env!("CARGO_PKG_VERSION"));
    }
}
