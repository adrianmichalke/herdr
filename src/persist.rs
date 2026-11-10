//! Session persistence — save/restore workspaces, layouts, and working directories.
//!
//! Stored at `~/.config/herdr/session.json`.
//! Optional pane screen history is stored separately at `session-history.json`.
//! Installed plugins are persisted separately at `plugins.json`.

mod io;
mod pane_aliases;
pub mod plugin_registry;
mod restore;
mod snapshot;
#[cfg(unix)]
pub(crate) mod workspace_transfer;
mod writer;

pub(crate) use self::io::load_from_path;
pub use self::io::{clear_history, load, load_history};
pub use self::pane_aliases::PaneAliasesSnapshot;
pub(crate) use self::pane_aliases::{reserve_pane_alias_namespaces, RestoredPaneAliases};
#[cfg(unix)]
pub(crate) use self::restore::restore_handoff_with_aliases;
pub(crate) use self::restore::{restore_with_aliases, restored_worktree_space_membership};
pub use self::snapshot::{
    capture, capture_history, DirectionSnapshot, LayoutSnapshot, SessionHistorySnapshot,
    SessionSnapshot, TabSnapshot, WorkspaceSnapshot,
};
pub(crate) use self::writer::SessionWriter;
