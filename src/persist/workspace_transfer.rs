use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::os::fd::{IntoRawFd, OwnedFd};

use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::handoff_runtime::{HandoffRuntimeState, ImportedHandoffRuntime};
use crate::layout::{Node, PaneId, TileLayout};
use crate::pane::PaneState;
use crate::server::workspace_transfer::journal::Route;
use crate::terminal::{TerminalId, TerminalRuntime, TerminalState};
use crate::workspace::{Tab, Workspace};

#[derive(Serialize, Deserialize)]
pub(crate) struct WorkspaceTransferBundle {
    pub transfer_protocol: u32,
    pub transfer_id: String,
    pub source_session: String,
    pub destination_session: String,
    pub source_version: String,
    pub source_protocol: u32,
    pub source_domain: String,
    pub forwarding_depth: u8,
    pub snapshot: super::SessionSnapshot,
    pub identity_cache: WorkspaceIdentityTransferState,
    pub panes: BTreeMap<u32, TransferPane>,
    pub runtimes: Vec<HandoffRuntimeState>,
    pub metadata_tokens: crate::metadata_tokens::MetadataTokens,
    pub metadata_token_sequences: HashMap<String, u64>,
    pub previous_focus: Vec<Option<u32>>,
    pub next_agent_state_change_seq: u64,
    pub next_agent_activity_seq: u64,
    pub aliases: BTreeMap<String, u32>,
    pub plugins: Vec<crate::api::schema::InstalledPluginInfo>,
    pub plugin_panes: BTreeMap<u32, PluginAssociation>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct WorkspaceIdentityTransferState {
    cached_identity_cwd: std::path::PathBuf,
    cached_auto_label: String,
    cached_git_status_key: std::path::PathBuf,
    cached_git_branch: Option<String>,
    cached_git_ahead_behind: Option<(usize, usize)>,
    cached_git_space: Option<crate::workspace::GitSpaceMetadata>,
}

impl WorkspaceIdentityTransferState {
    pub(crate) fn capture(workspace: &Workspace) -> Self {
        Self {
            cached_identity_cwd: workspace.cached_identity_cwd.clone(),
            cached_auto_label: workspace.cached_auto_label.clone(),
            cached_git_status_key: workspace.cached_git_status_key.clone(),
            cached_git_branch: workspace.cached_git_branch.clone(),
            cached_git_ahead_behind: workspace.cached_git_ahead_behind,
            cached_git_space: workspace.cached_git_space.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct TransferPane {
    pub terminal: crate::terminal::state::TerminalTransferState,
    pub seen: bool,
    pub right_click_passthrough: bool,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct PluginAssociation {
    pub plugin_id: String,
    pub entrypoint: String,
}

pub(crate) struct ImportedWorkspace {
    pub workspace: Workspace,
    pub terminals: HashMap<TerminalId, TerminalState>,
    pub runtimes: HashMap<TerminalId, TerminalRuntime>,
    pub plugins: Vec<crate::api::schema::InstalledPluginInfo>,
    pub plugin_panes: HashMap<PaneId, crate::app::state::PluginPaneRecord>,
    pub next_agent_state_change_seq: u64,
    pub next_agent_activity_seq: u64,
    pub checkpoint_cwds: HashMap<u32, std::path::PathBuf>,
}

pub(crate) fn capture(
    app: &App,
    workspace_id: &str,
    transfer_id: String,
    destination_session: String,
    runtimes: Vec<HandoffRuntimeState>,
    forwarding_depth: u8,
) -> io::Result<WorkspaceTransferBundle> {
    let workspace = app
        .state
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "workspace disappeared during transfer",
            )
        })?;
    let mut snapshot = super::capture(
        std::slice::from_ref(workspace),
        &app.state.terminals,
        &app.terminal_runtimes,
        Some(0),
        0,
        app.state.next_agent_activity_seq,
    );
    snapshot.workspaces[0].identity_cwd = workspace.identity_cwd.clone();
    let mut panes = BTreeMap::new();
    let mut plugin_panes = BTreeMap::new();
    let mut plugin_ids = HashSet::new();
    let mut aliases = BTreeMap::new();
    for tab in &workspace.tabs {
        for (&id, pane) in &tab.panes {
            let terminal = app
                .state
                .terminals
                .get(&pane.attached_terminal_id)
                .ok_or_else(|| io::Error::other("workspace contains a missing terminal"))?;
            panes.insert(
                id.raw(),
                TransferPane {
                    terminal: terminal
                        .workspace_transfer_state()
                        .map_err(io::Error::other)?,
                    seen: pane.seen,
                    right_click_passthrough: pane.right_click_passthrough,
                },
            );
            aliases.insert(format!("p_{}", id.raw()), id.raw());
            if let Some(plugin) = app.state.plugin_panes.get(&id) {
                plugin_ids.insert(plugin.plugin_id.clone());
                plugin_panes.insert(
                    id.raw(),
                    PluginAssociation {
                        plugin_id: plugin.plugin_id.clone(),
                        entrypoint: plugin.entrypoint.clone(),
                    },
                );
            }
        }
    }
    for (alias, id) in &app.state.public_pane_id_aliases {
        if panes.contains_key(&id.raw()) {
            aliases.insert(alias.clone(), id.raw());
        }
    }
    for (alias, id) in &app.state.pane_id_aliases {
        if panes.contains_key(&id.raw()) {
            aliases.insert(format!("p_{alias}"), id.raw());
        }
    }
    let plugins = plugin_ids
        .into_iter()
        .map(|id| {
            app.state
                .installed_plugins
                .get(&id)
                .cloned()
                .ok_or_else(|| {
                    io::Error::other(format!("plugin association references missing plugin {id}"))
                })
        })
        .collect::<io::Result<_>>()?;
    let bundle = WorkspaceTransferBundle {
        transfer_protocol: crate::server::workspace_transfer::TRANSFER_PROTOCOL_VERSION,
        transfer_id,
        source_session: crate::server::workspace_transfer::session_name(),
        destination_session,
        source_version: crate::build_info::version(),
        source_protocol: crate::protocol::PROTOCOL_VERSION,
        source_domain: crate::platform::workspace_transfer_domain()?,
        forwarding_depth,
        snapshot,
        identity_cache: WorkspaceIdentityTransferState::capture(workspace),
        panes,
        runtimes,
        metadata_tokens: workspace.metadata_tokens.clone(),
        metadata_token_sequences: workspace.metadata_token_sequences.clone(),
        previous_focus: workspace
            .tabs
            .iter()
            .map(|tab| tab.layout.previous_focus().map(PaneId::raw))
            .collect(),
        next_agent_state_change_seq: app.state.next_agent_state_change_seq,
        next_agent_activity_seq: app.state.next_agent_activity_seq,
        aliases,
        plugins,
        plugin_panes,
    };
    validate(&bundle)?;
    Ok(bundle)
}

fn pane_ids(node: &super::LayoutSnapshot, out: &mut Vec<u32>) -> io::Result<()> {
    match node {
        super::LayoutSnapshot::Pane(id) => out.push(*id),
        super::LayoutSnapshot::Split {
            ratio,
            first,
            second,
            ..
        } => {
            if !ratio.is_finite() || *ratio <= 0.0 || *ratio >= 1.0 {
                return Err(io::Error::other("invalid transfer layout ratio"));
            }
            pane_ids(first, out)?;
            pane_ids(second, out)?;
        }
    }
    Ok(())
}

pub(crate) fn validate(bundle: &WorkspaceTransferBundle) -> io::Result<()> {
    if bundle.transfer_protocol != crate::server::workspace_transfer::TRANSFER_PROTOCOL_VERSION
        || bundle.source_protocol != crate::protocol::PROTOCOL_VERSION
        || bundle.source_version != crate::build_info::version()
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "workspace transfer requires matching server builds",
        ));
    }
    if !crate::server::workspace_transfer::valid_transfer_id(&bundle.transfer_id) {
        return Err(io::Error::other("invalid transfer UUID"));
    }
    for name in [&bundle.source_session, &bundle.destination_session] {
        crate::session::parse_target_name(name)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    }
    if bundle.forwarding_depth >= crate::server::workspace_transfer::MAX_FORWARD_HOPS {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "workspace forwarding hop capacity is exhausted",
        ));
    }
    if bundle.snapshot.workspaces.len() != 1 {
        return Err(io::Error::other(
            "transfer must contain exactly one workspace",
        ));
    }
    let workspace = &bundle.snapshot.workspaces[0];
    if workspace
        .id
        .as_deref()
        .and_then(crate::workspace::public_workspace_number)
        .is_none()
        || workspace.tabs.is_empty()
        || workspace.active_tab >= workspace.tabs.len()
        || workspace.public_tab_numbers.len() != workspace.tabs.len()
        || bundle.previous_focus.len() != workspace.tabs.len()
    {
        return Err(io::Error::other(
            "invalid transfer workspace identity or tab topology",
        ));
    }
    let mut all_ids = HashSet::new();
    for (index, tab) in workspace.tabs.iter().enumerate() {
        let mut ids = Vec::new();
        pane_ids(&tab.layout, &mut ids)?;
        if ids.is_empty()
            || ids.iter().any(|id| !all_ids.insert(*id))
            || ids.len() != tab.panes.len()
            || ids.iter().any(|id| !tab.panes.contains_key(id))
            || tab.focused.is_none_or(|id| !ids.contains(&id))
            || tab.root_pane.is_none_or(|id| !ids.contains(&id))
            || bundle.previous_focus[index].is_some_and(|id| !ids.contains(&id))
        {
            return Err(io::Error::other(
                "transfer pane layout is not an exact bijection",
            ));
        }
    }
    let runtime_ids: HashSet<_> = bundle
        .runtimes
        .iter()
        .map(|runtime| runtime.pane_id)
        .collect();
    if runtime_ids.len() != bundle.runtimes.len()
        || runtime_ids != all_ids
        || bundle.panes.keys().copied().collect::<HashSet<_>>() != all_ids
        || workspace
            .public_pane_numbers
            .keys()
            .copied()
            .collect::<HashSet<_>>()
            != all_ids
        || bundle
            .runtimes
            .iter()
            .any(|runtime| runtime.terminal_snapshot.is_none())
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "transfer requires exactly one full terminal snapshot and live PTY per pane",
        ));
    }
    let pane_numbers: HashSet<_> = workspace.public_pane_numbers.values().copied().collect();
    let tab_numbers: HashSet<_> = workspace.public_tab_numbers.iter().copied().collect();
    if pane_numbers.len() != all_ids.len()
        || pane_numbers.contains(&0)
        || pane_numbers
            .iter()
            .any(|number| *number >= workspace.next_public_pane_number)
        || tab_numbers.len() != workspace.tabs.len()
        || tab_numbers.contains(&0)
        || tab_numbers
            .iter()
            .any(|number| *number >= workspace.next_public_tab_number)
        || bundle.aliases.values().any(|id| !all_ids.contains(id))
        || bundle.plugin_panes.keys().any(|id| !all_ids.contains(id))
    {
        return Err(io::Error::other(
            "transfer contains invalid public ordinals, counters or associations",
        ));
    }
    Ok(())
}

pub(crate) fn route(
    bundle: &WorkspaceTransferBundle,
    destination_workspace: &str,
) -> io::Result<Route> {
    let workspace = &bundle.snapshot.workspaces[0];
    let origin_workspace = workspace
        .id
        .clone()
        .ok_or_else(|| io::Error::other("missing workspace identity"))?;
    let mut identities =
        BTreeMap::from([(origin_workspace.clone(), destination_workspace.to_owned())]);
    for (id, number) in &workspace.public_pane_numbers {
        let destination =
            crate::workspace::public_pane_id_for_number(destination_workspace, *number);
        identities.insert(
            crate::workspace::public_pane_id_for_number(&origin_workspace, *number),
            destination.clone(),
        );
        for (alias, raw) in &bundle.aliases {
            if raw == id {
                identities.insert(alias.clone(), destination.clone());
            }
        }
    }
    for number in &workspace.public_tab_numbers {
        identities.insert(
            crate::workspace::public_tab_id_for_number(&origin_workspace, *number),
            crate::workspace::public_tab_id_for_number(destination_workspace, *number),
        );
    }
    let mut names = BTreeMap::new();
    let mut terminals = HashSet::new();
    let mut terminal_bindings = BTreeMap::new();
    let mut canonical_inverse =
        BTreeMap::from([(destination_workspace.to_owned(), origin_workspace.clone())]);
    for number in workspace.public_pane_numbers.values() {
        canonical_inverse.insert(
            crate::workspace::public_pane_id_for_number(destination_workspace, *number),
            crate::workspace::public_pane_id_for_number(&origin_workspace, *number),
        );
    }
    for number in &workspace.public_tab_numbers {
        canonical_inverse.insert(
            crate::workspace::public_tab_id_for_number(destination_workspace, *number),
            crate::workspace::public_tab_id_for_number(&origin_workspace, *number),
        );
    }
    for (raw, pane) in &bundle.panes {
        let terminal = TerminalState::from_workspace_transfer_state(pane.terminal.clone())
            .map_err(io::Error::other)?;
        if !terminals.insert(terminal.id.clone()) {
            return Err(io::Error::other("duplicate terminal identity in transfer"));
        }
        if identities.contains_key(terminal.id.as_str())
            || canonical_inverse.contains_key(terminal.id.as_str())
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "terminal identity collides with a structural forwarding identity",
            ));
        }
        identities.insert(terminal.id.to_string(), terminal.id.to_string());
        canonical_inverse.insert(terminal.id.to_string(), terminal.id.to_string());
        terminal_bindings.insert(
            terminal.id.to_string(),
            crate::workspace::public_pane_id_for_number(
                destination_workspace,
                workspace.public_pane_numbers[raw],
            ),
        );
        if let Some(name) = terminal.agent_name {
            let number = workspace.public_pane_numbers[raw];
            if names
                .insert(
                    name,
                    crate::workspace::public_pane_id_for_number(destination_workspace, number),
                )
                .is_some()
            {
                return Err(io::Error::other(
                    "duplicate agent name in transferred workspace",
                ));
            }
        }
    }
    Ok(Route {
        transfer_id: bundle.transfer_id.clone(),
        destination_session: bundle.destination_session.clone(),
        origin_workspace,
        destination_workspace: destination_workspace.into(),
        identities,
        canonical_inverse,
        terminals: terminal_bindings,
        names,
        pending_names: Default::default(),
        ready: false,
    })
}

fn restore_node(node: super::LayoutSnapshot, ids: &HashMap<u32, PaneId>) -> Node {
    match node {
        super::LayoutSnapshot::Pane(raw) => Node::Pane(ids[&raw]),
        super::LayoutSnapshot::Split {
            direction,
            ratio,
            first,
            second,
        } => Node::Split {
            direction: match direction {
                super::DirectionSnapshot::Horizontal => ratatui::layout::Direction::Horizontal,
                super::DirectionSnapshot::Vertical => ratatui::layout::Direction::Vertical,
            },
            ratio,
            first: Box::new(restore_node(*first, ids)),
            second: Box::new(restore_node(*second, ids)),
        },
    }
}

pub(crate) struct RestoreContext {
    scrollback_limit_bytes: usize,
    host_terminal_theme: crate::terminal_theme::TerminalTheme,
    host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
    events: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    render_notify: std::sync::Arc<tokio::sync::Notify>,
    render_dirty: std::sync::Arc<crate::render_signal::RenderSignal>,
    terminal_ids: HashSet<TerminalId>,
    unavailable_raw_ids: HashSet<u32>,
}

impl RestoreContext {
    pub(crate) fn from_app(app: &App) -> Self {
        Self {
            scrollback_limit_bytes: app.state.pane_scrollback_limit_bytes,
            host_terminal_theme: app.state.host_terminal_theme,
            host_terminal_appearance: app.state.host_terminal_appearance,
            events: app.event_tx.clone(),
            render_notify: app.render_notify.clone(),
            render_dirty: app.render_dirty.clone(),
            terminal_ids: app.state.terminals.keys().cloned().collect(),
            unavailable_raw_ids: app
                .state
                .pane_id_aliases
                .keys()
                .copied()
                .chain(
                    app.state
                        .workspaces
                        .iter()
                        .flat_map(|workspace| &workspace.tabs)
                        .flat_map(|tab| tab.panes.keys().map(|pane| pane.raw())),
                )
                .collect(),
        }
    }
}

pub(crate) fn restore(
    context: RestoreContext,
    mut bundle: WorkspaceTransferBundle,
    fds: Vec<OwnedFd>,
    destination_workspace: String,
) -> io::Result<ImportedWorkspace> {
    validate(&bundle)?;
    if fds.len() != bundle.runtimes.len() {
        return Err(io::Error::other("transfer FD count mismatch"));
    }
    let ids: HashMap<_, _> = bundle
        .panes
        .keys()
        .map(|raw| {
            let mut id = PaneId::alloc();
            while context.unavailable_raw_ids.contains(&id.raw()) {
                id = PaneId::alloc();
            }
            (*raw, id)
        })
        .collect();
    let checkpoint_cwds = bundle.snapshot.workspaces[0]
        .tabs
        .iter()
        .flat_map(|tab| &tab.panes)
        .map(|(raw, pane)| (ids[raw].raw(), pane.cwd.clone()))
        .collect();
    let mut runtimes = HashMap::new();
    let mut terminals = HashMap::new();
    let mut panes = HashMap::new();
    let mut state_change_sequence = bundle.next_agent_state_change_seq;
    let mut activity_sequence = bundle.next_agent_activity_seq;
    for (raw, pane) in bundle.panes {
        let terminal = TerminalState::from_workspace_transfer_state(pane.terminal)
            .map_err(io::Error::other)?;
        if context.terminal_ids.contains(&terminal.id) || terminals.contains_key(&terminal.id) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "destination terminal identity collision",
            ));
        }
        state_change_sequence = state_change_sequence
            .max(terminal.last_agent_state_change_seq.unwrap_or(0))
            .max(terminal.last_agent_completion_seq.unwrap_or(0));
        activity_sequence = activity_sequence.max(
            terminal
                .agent_activity
                .as_ref()
                .map_or(0, |activity| activity.sequence),
        );
        if state_change_sequence == u64::MAX || activity_sequence == u64::MAX {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "transferred activity sequence space is exhausted",
            ));
        }
        let id = ids[&raw];
        panes.insert(
            raw,
            PaneState {
                attached_terminal_id: terminal.id.clone(),
                seen: pane.seen,
                right_click_passthrough: pane.right_click_passthrough,
            },
        );
        terminals.insert(terminal.id.clone(), terminal);
        if context.unavailable_raw_ids.contains(&id.raw()) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "destination raw pane alias collision",
            ));
        }
    }
    for (runtime, fd) in bundle.runtimes.into_iter().zip(fds) {
        let pane = panes
            .get(&runtime.pane_id)
            .ok_or_else(|| io::Error::other("missing transfer pane"))?;
        let pane_id = ids[&runtime.pane_id];
        let imported = TerminalRuntime::from_handoff_fd(
            ImportedHandoffRuntime {
                master_fd: fd.into_raw_fd(),
                state: runtime.with_pane_id(pane_id),
            },
            context.scrollback_limit_bytes,
            context.host_terminal_theme,
            context.host_terminal_appearance,
            context.events.clone(),
            context.render_notify.clone(),
            context.render_dirty.clone(),
        )?;
        runtimes.insert(pane.attached_terminal_id.clone(), imported);
    }
    let snapshot = bundle.snapshot.workspaces.remove(0);
    let mut tabs = Vec::new();
    for (index, tab) in snapshot.tabs.into_iter().enumerate() {
        let focus = tab
            .focused
            .ok_or_else(|| io::Error::other("missing tab focus"))?;
        let root = tab
            .root_pane
            .ok_or_else(|| io::Error::other("missing tab root"))?;
        let tab_panes = tab
            .panes
            .keys()
            .map(|raw| {
                panes
                    .remove(raw)
                    .map(|pane| (ids[raw], pane))
                    .ok_or_else(|| io::Error::other("missing pane state"))
            })
            .collect::<io::Result<_>>()?;
        let mut layout = TileLayout::from_saved(restore_node(tab.layout, &ids), ids[&focus]);
        layout.restore_previous_focus(bundle.previous_focus[index].map(|raw| ids[&raw]));
        tabs.push(Tab {
            custom_name: tab.custom_name,
            number: snapshot.public_tab_numbers[index],
            root_pane: ids[&root],
            layout,
            panes: tab_panes,
            zoomed: tab.zoomed,
            events: context.events.clone(),
            render_notify: context.render_notify.clone(),
            render_dirty: context.render_dirty.clone(),
            #[cfg(test)]
            runtimes: HashMap::new(),
        });
    }
    let plugin_panes = bundle
        .plugin_panes
        .into_iter()
        .map(|(raw, plugin)| {
            (
                ids[&raw],
                crate::app::state::PluginPaneRecord {
                    plugin_id: plugin.plugin_id,
                    entrypoint: plugin.entrypoint,
                },
            )
        })
        .collect();
    let workspace = Workspace {
        id: destination_workspace,
        custom_name: snapshot.custom_name,
        identity_cwd: snapshot.identity_cwd,
        cached_identity_cwd: bundle.identity_cache.cached_identity_cwd,
        cached_auto_label: bundle.identity_cache.cached_auto_label,
        cached_git_status_key: bundle.identity_cache.cached_git_status_key,
        cached_git_branch: bundle.identity_cache.cached_git_branch,
        cached_git_ahead_behind: bundle.identity_cache.cached_git_ahead_behind,
        cached_git_space: bundle.identity_cache.cached_git_space,
        worktree_space: snapshot.worktree_space,
        metadata_tokens: bundle.metadata_tokens,
        metadata_token_sequences: bundle.metadata_token_sequences,
        public_pane_numbers: snapshot
            .public_pane_numbers
            .into_iter()
            .map(|(raw, number)| (ids[&raw], number))
            .collect(),
        next_public_pane_number: snapshot.next_public_pane_number,
        next_public_tab_number: snapshot.next_public_tab_number,
        tabs,
        active_tab: snapshot.active_tab,
        #[cfg(test)]
        test_runtimes: HashMap::new(),
    };
    Ok(ImportedWorkspace {
        workspace,
        terminals,
        runtimes,
        plugins: bundle.plugins,
        plugin_panes,
        next_agent_state_change_seq: state_change_sequence,
        next_agent_activity_seq: activity_sequence,
        checkpoint_cwds,
    })
}
