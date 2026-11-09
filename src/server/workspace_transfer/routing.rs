use std::collections::BTreeSet;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use interprocess::local_socket::traits::Stream as _;
use serde_json::Value;

use super::journal::{Route, SourcePhase, TransferRegistry};
use crate::api::schema::{Method, Request};
use crate::ipc::{LocalStream, LocalStreamReadCount};

const MAX_HOPS: usize = super::MAX_FORWARD_HOPS as usize;
const MAX_RESPONSE_LINE: usize = 64 * 1024 * 1024;

pub(crate) struct ForwardRequest {
    pub route: Route,
    pub request: Value,
    reserved_name: Option<(String, String)>,
}

fn identity_fields() -> &'static [&'static str] {
    &[
        "workspace_id",
        "source_workspace_id",
        "before_workspace_id",
        "tab_id",
        "pane_id",
        "caller_pane_id",
        "source_pane_id",
        "target_pane_id",
        "terminal_id",
    ]
}

fn visit_target_fields(value: &mut Value, visitor: &mut impl FnMut(&mut Value)) {
    if let Some(object) = value.as_object_mut() {
        for key in identity_fields() {
            if let Some(value) = object.get_mut(*key).filter(|value| value.is_string()) {
                visitor(value);
            }
        }
        if let Some(ids) = object
            .get_mut("workspace_ids")
            .and_then(Value::as_array_mut)
        {
            for value in ids {
                visitor(value);
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetKind {
    Structural,
    Agent,
}

fn visit_request_targets(value: &mut Value, visitor: &mut impl FnMut(&mut Value, TargetKind)) {
    let method = value["method"].as_str().unwrap_or_default().to_owned();
    let Some(params) = value.get_mut("params") else {
        return;
    };
    visit_target_fields(params, &mut |value| visitor(value, TargetKind::Structural));
    if method.starts_with("agent.") {
        if let Some(target) = params.get_mut("target").filter(|value| value.is_string()) {
            visitor(target, TargetKind::Agent);
        }
    }
    if method == "events.wait" {
        if let Some(event) = params.get_mut("match_event") {
            visit_target_fields(event, &mut |value| visitor(value, TargetKind::Structural));
        }
    }
    if method == "events.subscribe" {
        if let Some(subscriptions) = params
            .get_mut("subscriptions")
            .and_then(Value::as_array_mut)
        {
            for subscription in subscriptions {
                visit_target_fields(subscription, &mut |value| {
                    visitor(value, TargetKind::Structural)
                });
            }
        }
    }
    if method == "pane.move" {
        if let Some(destination) = params.get_mut("destination") {
            visit_target_fields(destination, &mut |value| {
                visitor(value, TargetKind::Structural)
            });
        }
    }
    if method == "command.invoke" {
        if let Some(selection) = params.get_mut("selection") {
            visit_target_fields(selection, &mut |value| {
                visitor(value, TargetKind::Structural)
            });
        }
    }
    if method.starts_with("plugin.") {
        if let Some(context) = params.get_mut("context") {
            visit_target_fields(context, &mut |value| visitor(value, TargetKind::Structural));
        }
    }
}

pub(crate) fn targets(request: &Request) -> Vec<(String, TargetKind)> {
    let Ok(mut value) = serde_json::to_value(request) else {
        return Vec::new();
    };
    let mut targets = Vec::new();
    visit_request_targets(&mut value, &mut |value, kind| {
        if let Some(target) = value.as_str() {
            targets.push((target.to_owned(), kind));
        }
    });
    targets
}

fn translate(route: &Route, target: &str, kind: TargetKind) -> Option<String> {
    if kind == TargetKind::Agent
        && !target.split_once(":p").is_some_and(|(workspace, pane)| {
            crate::workspace::public_workspace_number(workspace).is_some()
                && crate::workspace::decode_public_number(pane).is_some()
        })
    {
        return route.names.get(target).cloned();
    }
    if let Some(target) = route.identities.get(target) {
        return Some(target.clone());
    }
    let (workspace, suffix) = target.split_once(':')?;
    let destination = route.identities.get(workspace)?;
    (suffix.starts_with('p') || suffix.starts_with('t')).then(|| format!("{destination}:{suffix}"))
}

pub(crate) fn is_deferred_report(request: &Request) -> bool {
    matches!(
        request.method,
        Method::PaneReportAgent(_)
            | Method::PaneReportAgentSession(_)
            | Method::PaneReportMetadata(_)
            | Method::PaneReleaseAgent(_)
            | Method::WorkspaceReportMetadata(_)
    )
}

pub(crate) fn resolve_when_ready(
    registry: &TransferRegistry,
    request: &Request,
    original: &str,
    running: &AtomicBool,
) -> io::Result<Option<ForwardRequest>> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match resolve(registry, request, original) {
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    && is_deferred_report(request)
                    && running.load(Ordering::Relaxed)
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(25));
            }
            result => return result,
        }
    }
}

fn local_alias_busy(journal: &super::journal::Journal, id: &str) -> Option<bool> {
    let workspace = journal.live_pane_aliases.get(id)?;
    Some(journal.sources.values().any(|source| {
        source.params.workspace_id == *workspace
            && matches!(
                source.phase,
                SourcePhase::Preparing | SourcePhase::CommitDecided
            )
    }))
}

pub(crate) fn resolve(
    registry: &TransferRegistry,
    request: &Request,
    original: &str,
) -> io::Result<Option<ForwardRequest>> {
    if matches!(
        request.method,
        Method::WorkspaceTransfer(_) | Method::WorkspaceTransferStatus(_)
    ) {
        return Ok(None);
    }
    if registry.read(|journal| {
        journal.routes.is_empty()
            && !journal.sources.values().any(|source| {
                matches!(
                    source.phase,
                    SourcePhase::Preparing | SourcePhase::CommitDecided
                )
            })
    })? {
        return Ok(None);
    }
    let candidates = targets(request);
    let relevant = registry.read(|journal| {
        candidates.iter().any(|(id, kind)| {
            if let Some(busy) = local_alias_busy(journal, id) {
                return busy;
            }
            journal
                .routes
                .values()
                .any(|route| translate(route, id, *kind).is_some())
                || journal.sources.values().any(|source| {
                    matches!(
                        source.phase,
                        SourcePhase::Preparing | SourcePhase::CommitDecided
                    ) && (source.source_identities.contains(id)
                        || source.source_names.contains_key(id)
                        || id == &source.params.workspace_id
                        || id
                            .strip_prefix(&source.params.workspace_id)
                            .is_some_and(|suffix| suffix.starts_with(':')))
                })
        })
    })?;
    if !relevant {
        return Ok(None);
    }
    let journal = registry.snapshot()?;
    let mut value = serde_json::to_value(request).map_err(io::Error::other)?;
    let mut route_ids = BTreeSet::new();
    let mut has_local = false;
    let mut busy = false;
    let mut ambiguous = false;
    visit_request_targets(&mut value, &mut |target, kind| {
        let Some(id) = target.as_str() else { return };
        if let Some(alias_busy) = local_alias_busy(&journal, id) {
            has_local = true;
            busy |= alias_busy;
            return;
        }
        let named_agent = kind == TargetKind::Agent
            && !id.split_once(":p").is_some_and(|(workspace, pane)| {
                crate::workspace::public_workspace_number(workspace).is_some()
                    && crate::workspace::decode_public_number(pane).is_some()
            });
        if journal.sources.values().any(|source| {
            matches!(
                source.phase,
                SourcePhase::Preparing | SourcePhase::CommitDecided
            ) && ((named_agent && source.source_names.contains_key(id))
                || (!named_agent
                    && (source.source_identities.contains(id)
                        || id == source.params.workspace_id
                        || id
                            .strip_prefix(&source.params.workspace_id)
                            .is_some_and(|suffix| suffix.starts_with(':')))))
        }) {
            busy = true;
            has_local = true;
            return;
        }
        let local_agent = named_agent
            && journal.live_names.get(id).is_some_and(|terminal| {
                let reserved = journal
                    .name_routes
                    .get(id)
                    .and_then(|stack| stack.last())
                    .and_then(|route| journal.routes.get(route))
                    .and_then(|route| route.terminal_for_name(id));
                reserved.is_none_or(|reserved| reserved == terminal)
            });
        if (kind == TargetKind::Structural && journal.live_terminals.contains(id)) || local_agent {
            has_local = true;
            return;
        }
        let indexed_route = if named_agent {
            journal.name_routes.get(id).and_then(|stack| stack.last())
        } else if journal.terminal_routes.contains_key(id) {
            journal
                .terminal_routes
                .get(id)
                .and_then(|stack| stack.last())
        } else {
            None
        };
        let mut matches: Vec<_> = journal
            .routes
            .iter()
            .filter(|(route_id, _)| indexed_route.is_none_or(|selected| selected == *route_id))
            .filter_map(|(route_id, route)| {
                translate(route, id, kind).map(|translated| (route_id, route, translated))
            })
            .collect();
        if !named_agent
            && matches
                .iter()
                .any(|(_, route, _)| route.identities.contains_key(id))
        {
            matches.retain(|(_, route, _)| route.identities.contains_key(id));
        }
        if matches.windows(2).any(|pair| {
            pair[0].1.destination_session != pair[1].1.destination_session || pair[0].2 != pair[1].2
        }) {
            ambiguous = true;
            return;
        }
        if let Some((route_id, route, translated)) = matches.into_iter().next() {
            route_ids.insert(route_id.clone());
            busy |= !route.ready;
            *target = Value::String(translated);
        } else {
            has_local = true;
            busy |= (kind == TargetKind::Structural || id.contains(":p"))
                && journal.sources.values().any(|record| {
                    matches!(
                        record.phase,
                        SourcePhase::Preparing | SourcePhase::CommitDecided
                    ) && (id == record.params.workspace_id
                        || id
                            .strip_prefix(&record.params.workspace_id)
                            .is_some_and(|suffix| suffix.starts_with(':')))
                });
        }
    });
    if ambiguous {
        return Err(io::Error::other("ambiguous reserved transfer identity"));
    }
    if busy {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "workspace transfer is in progress",
        ));
    }
    let Some(first) = route_ids.first() else {
        return Ok(None);
    };
    let mut route = journal
        .routes
        .get(first)
        .cloned()
        .ok_or_else(|| io::Error::other("missing transfer route"))?;
    if has_local
        || route_ids.iter().any(|id| {
            journal
                .routes
                .get(id)
                .is_none_or(|other| other.destination_session != route.destination_session)
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "request mixes local and transferred session targets",
        ));
    }
    if matches!(request.method, Method::WorkspaceTransfer(_)) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "submit another transfer on the owning session endpoint",
        ));
    }
    if let Method::EventsSubscribe(params) = &request.method {
        if params.subscriptions.iter().any(|subscription| {
            serde_json::to_value(subscription).is_ok_and(|mut value| {
                let mut found = false;
                visit_target_fields(&mut value, &mut |_| found = true);
                !found
            })
        }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "subscription mixes targeted and session-wide events",
            ));
        }
    }
    let original: Value = serde_json::from_str(original).map_err(io::Error::other)?;
    let mut hops: Vec<String> = match original.get("_herdr_forward_hops") {
        Some(value) => serde_json::from_value(value.clone()).map_err(io::Error::other)?,
        None => Vec::new(),
    };
    let current = format!("{}:{}", super::session_name(), route.transfer_id);
    if hops.len() >= MAX_HOPS || hops.contains(&current) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "workspace forwarding loop or hop limit",
        ));
    }
    hops.push(current);
    value["_herdr_forward_hops"] = serde_json::to_value(hops).map_err(io::Error::other)?;
    let mut reserved_name = None;
    if matches!(
        request.method,
        Method::AgentStart(_) | Method::AgentRename(_)
    ) {
        if let Some(name) = value["params"]["name"]
            .as_str()
            .filter(|name| !name.is_empty())
        {
            let target = value["params"]["target"]
                .as_str()
                .or_else(|| value["params"]["pane_id"].as_str())
                .ok_or_else(|| io::Error::other("forwarded agent operation has no target"))?;
            let already_bound = route.names.get(name).is_some_and(|bound| bound == target);
            let target_terminal = route
                .terminals
                .iter()
                .find_map(|(terminal, pane)| (pane == target).then_some(terminal.as_str()));
            registry.update(|journal| {
                if !already_bound && journal.live_names.contains_key(name)
                    || journal.routes.values().any(|other| {
                        other.names.get(name).is_some_and(|bound| {
                            (other.destination_session != route.destination_session
                                || bound != target)
                                && (target_terminal.is_none()
                                    || other.terminal_for_name(name) != target_terminal)
                        })
                    })
                {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "agent name collides with an origin namespace binding",
                    ));
                }
                let current = journal
                    .routes
                    .get_mut(&route.transfer_id)
                    .ok_or_else(|| io::Error::other("forwarding route disappeared"))?;
                current.names.insert(name.into(), target.into());
                if !already_bound {
                    current.pending_names.insert(name.into());
                }
                journal
                    .name_routes
                    .entry(name.into())
                    .or_insert_with(|| vec![route.transfer_id.clone()]);
                Ok(())
            })?;
            if !already_bound {
                reserved_name = Some((name.into(), target.into()));
            }
            route.names.insert(name.into(), target.into());
        }
    }
    Ok(Some(ForwardRequest {
        route,
        request: value,
        reserved_name,
    }))
}

fn response_identity_fields() -> &'static [&'static str] {
    &[
        "workspace_id",
        "tab_id",
        "pane_id",
        "terminal_id",
        "focused_pane_id",
        "root_pane_id",
        "active_tab_id",
        "source_pane_id",
        "target_pane_id",
        "previous_pane_id",
        "from_workspace_id",
        "to_workspace_id",
        "from_tab_id",
        "to_tab_id",
        "old_pane_id",
        "new_pane_id",
        "old_workspace_id",
        "new_workspace_id",
        "previous_workspace_id",
        "previous_tab_id",
        "closed_workspace_id",
        "closed_tab_id",
        "neighbor_pane_id",
        "focused_workspace_id",
        "focused_tab_id",
        "source_workspace_id",
        "open_workspace_id",
    ]
}

const RESPONSE_CONTAINERS: &[&str] = &[
    "result",
    "snapshot",
    "workspace",
    "workspaces",
    "tab",
    "tabs",
    "pane",
    "panes",
    "root_pane",
    "agent",
    "agents",
    "layout",
    "root",
    "first",
    "second",
    "event",
    "data",
    "from",
    "to",
    "source",
    "destination",
    "swap",
    "move_result",
    "zoom",
    "focus",
    "neighbor",
    "neighbors",
    "edges",
    "resize",
    "read",
    "process_info",
    "plugin_pane",
    "context",
    "selection",
    "explain",
    "left",
    "right",
    "up",
    "down",
    "created_workspace",
    "created_tab",
    "source_layout",
    "target_layout",
    "worktree",
    "worktrees",
];

fn visit_response(value: &mut Value, visitor: &mut impl FnMut(&mut Value)) {
    match value {
        Value::Array(values) => {
            for value in values {
                visit_response(value, visitor);
            }
        }
        Value::Object(object) => {
            for key in response_identity_fields() {
                if let Some(value) = object.get_mut(*key).filter(|value| value.is_string()) {
                    visitor(value);
                }
            }
            for key in ["pane_ids", "workspace_ids", "tab_ids"] {
                if let Some(values) = object.get_mut(key).and_then(Value::as_array_mut) {
                    for value in values {
                        visitor(value);
                    }
                }
            }
            for key in RESPONSE_CONTAINERS {
                if let Some(value) = object.get_mut(*key) {
                    visit_response(value, visitor);
                }
            }
        }
        _ => {}
    }
}

fn release_rejected_name(
    registry: &TransferRegistry,
    forward: &ForwardRequest,
    value: &Value,
) -> io::Result<()> {
    if value.get("error").is_none() {
        return Ok(());
    }
    if let Some((name, _)) = &forward.reserved_name {
        registry.update(|journal| {
            let removed = if let Some(route) = journal.routes.get_mut(&forward.route.transfer_id) {
                if route.pending_names.remove(name) {
                    route.names.remove(name);
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if removed {
                if let Some(stack) = journal.name_routes.get_mut(name) {
                    stack.retain(|id| id != &forward.route.transfer_id);
                }
                journal.name_routes.retain(|_, stack| !stack.is_empty());
            }
            Ok(())
        })?;
    }
    Ok(())
}

fn visit_named_agents(value: &mut Value, visitor: &mut impl FnMut(&mut Value, &str)) {
    match value {
        Value::Array(values) => {
            for value in values {
                visit_named_agents(value, visitor);
            }
        }
        Value::Object(object) => {
            if object.get("terminal_id").is_some() {
                if let Some(pane) = object
                    .get("pane_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                {
                    if let Some(name) = object.get_mut("name").filter(|value| value.is_string()) {
                        visitor(name, &pane);
                    }
                }
            }
            for key in ["result", "agent", "agents", "event", "data"] {
                if let Some(value) = object.get_mut(key) {
                    visit_named_agents(value, visitor);
                }
            }
        }
        _ => {}
    }
}

fn collect_terminal_records(
    value: &Value,
    records: &mut std::collections::BTreeMap<String, String>,
) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_terminal_records(value, records);
            }
        }
        Value::Object(object) => {
            if let (Some(terminal), Some(pane)) = (
                object.get("terminal_id").and_then(Value::as_str),
                object.get("pane_id").and_then(Value::as_str),
            ) {
                records.insert(terminal.into(), pane.into());
            }
            for key in RESPONSE_CONTAINERS {
                if let Some(value) = object.get(*key) {
                    collect_terminal_records(value, records);
                }
            }
        }
        _ => {}
    }
}

fn track_terminal_relocations(
    registry: &TransferRegistry,
    route: &Route,
    value: &Value,
) -> io::Result<()> {
    let mut records = std::collections::BTreeMap::new();
    collect_terminal_records(value, &mut records);
    if records.is_empty()
        || registry.read(|journal| {
            records.iter().all(|(terminal, pane)| {
                journal
                    .routes
                    .get(&route.transfer_id)
                    .is_some_and(|current| current.terminals.get(terminal) == Some(pane))
                    && journal
                        .routes
                        .values()
                        .filter(|current| current.destination_session == route.destination_session)
                        .all(|current| {
                            current
                                .terminals
                                .get(terminal)
                                .is_none_or(|current_pane| current_pane == pane)
                        })
            })
        })?
    {
        return Ok(());
    }
    registry.update(|journal| {
        for (terminal, pane) in records {
            if journal.live_terminals.contains(&terminal)
                && !journal
                    .routes
                    .values()
                    .any(|current| current.terminals.contains_key(&terminal))
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "remote terminal collides with a live origin terminal",
                ));
            }
            for current in journal
                .routes
                .values_mut()
                .filter(|current| current.destination_session == route.destination_session)
            {
                if let Some(previous) = current
                    .terminals
                    .get(&terminal)
                    .cloned()
                    .filter(|previous| previous != &pane)
                {
                    for target in current
                        .identities
                        .values_mut()
                        .chain(current.names.values_mut())
                    {
                        if *target == previous {
                            *target = pane.clone();
                        }
                    }
                    current.terminals.insert(terminal.clone(), pane.clone());
                }
            }
            if let Some(current) = journal.routes.get_mut(&route.transfer_id) {
                current.terminals.entry(terminal.clone()).or_insert(pane);
            }
            journal
                .terminal_routes
                .entry(terminal)
                .or_insert_with(|| vec![route.transfer_id.clone()]);
        }
        Ok(())
    })
}

pub(crate) fn reverse_response(
    registry: &TransferRegistry,
    route: &Route,
    value: &mut Value,
) -> io::Result<()> {
    track_terminal_relocations(registry, route, value)?;
    let mut terminal_records = std::collections::BTreeMap::new();
    collect_terminal_records(value, &mut terminal_records);
    let mut unknown = BTreeSet::new();
    visit_response(value, &mut |value| {
        if let Some(id) = value.as_str() {
            unknown.insert(id.to_owned());
        }
    });
    if unknown.is_empty() {
        return Ok(());
    }
    let journal = registry.snapshot()?;
    let routes: Vec<_> = journal
        .routes
        .values()
        .filter(|other| other.destination_session == route.destination_session)
        .collect();
    let mut reverse = std::collections::BTreeMap::new();
    let mut inverse_candidates = Vec::new();
    for other in &routes {
        for (destination, origin) in &other.canonical_inverse {
            reverse
                .entry(destination.clone())
                .or_insert_with(|| origin.clone());
        }
        if !other.canonical_inverse.is_empty() {
            continue;
        }
        for (origin, destination) in &other.identities {
            let score = if origin == &other.origin_workspace
                || origin
                    .strip_prefix(&other.origin_workspace)
                    .is_some_and(|suffix| suffix.starts_with(":p") || suffix.starts_with(":t"))
                || origin.starts_with("term_")
            {
                0
            } else if crate::workspace::public_workspace_number(
                origin
                    .split_once(':')
                    .map_or(origin.as_str(), |(workspace, _)| workspace),
            )
            .is_some()
            {
                1
            } else {
                2
            };
            inverse_candidates.push((score, origin, destination));
        }
    }
    inverse_candidates.sort_unstable();
    for (_, origin, destination) in inverse_candidates {
        reverse
            .entry(destination.clone())
            .or_insert_with(|| origin.clone());
    }
    let mut changed = false;
    let mut updated = journal
        .routes
        .get(&route.transfer_id)
        .cloned()
        .unwrap_or_else(|| route.clone());
    let mut resolved_names = BTreeSet::new();
    visit_named_agents(value, &mut |name, pane| {
        let Some(remote_name) = name.as_str().map(str::to_owned) else {
            return;
        };
        let bound_here = routes.iter().any(|other| {
            other
                .names
                .get(&remote_name)
                .is_some_and(|target| target == pane)
        });
        let terminal = routes
            .iter()
            .flat_map(|other| &other.terminals)
            .find_map(|(terminal, target)| (target == pane).then_some(terminal.as_str()));
        let collision = (!bound_here
            && journal
                .live_names
                .get(&remote_name)
                .is_some_and(|local| Some(local.as_str()) != terminal))
            || journal.routes.values().any(|other| {
                other.names.get(&remote_name).is_some_and(|target| {
                    (other.destination_session != route.destination_session || target != pane)
                        && (terminal.is_none() || other.terminal_for_name(&remote_name) != terminal)
                })
            });
        let alias = if collision {
            routes
                .iter()
                .flat_map(|other| &other.names)
                .find(|(_, target)| target.as_str() == pane)
                .map(|(name, _)| name.clone())
                .unwrap_or_else(|| {
                    static NEXT: std::sync::atomic::AtomicU64 =
                        std::sync::atomic::AtomicU64::new(1);
                    loop {
                        let alias = format!("forward-{}", NEXT.fetch_add(1, Ordering::Relaxed));
                        if !journal.live_names.contains_key(&alias)
                            && !updated.names.contains_key(&alias)
                            && !journal
                                .routes
                                .values()
                                .any(|other| other.names.contains_key(&alias))
                        {
                            break alias;
                        }
                    }
                })
        } else {
            remote_name
        };
        if !updated.names.contains_key(&alias) {
            updated.names.insert(alias.clone(), pane.into());
            changed = true;
        }
        if updated.pending_names.contains(&alias) {
            changed = true;
        }
        resolved_names.insert(alias.clone());
        *name = Value::String(alias);
    });
    for id in unknown {
        if reverse.contains_key(&id) {
            continue;
        }
        if let Some((workspace, suffix)) = id.split_once(':') {
            if suffix.starts_with('p') || suffix.starts_with('t') {
                let origin_workspace = if let Some(origin) = reverse.get(workspace) {
                    origin.clone()
                } else {
                    let alias = crate::workspace::generate_workspace_id();
                    reverse.insert(workspace.to_owned(), alias.clone());
                    updated
                        .identities
                        .insert(alias.clone(), workspace.to_owned());
                    updated
                        .canonical_inverse
                        .insert(workspace.to_owned(), alias.clone());
                    alias
                };
                let alias = format!("{origin_workspace}:{suffix}");
                reverse.insert(id.clone(), alias.clone());
                updated.canonical_inverse.insert(id.clone(), alias.clone());
                updated.identities.insert(alias, id);
                changed = true;
            }
        } else if terminal_records.contains_key(&id) {
            if journal.live_terminals.contains(&id) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "remote terminal collides with a live origin terminal",
                ));
            }
            if routes.iter().any(|other| {
                other
                    .identities
                    .get(&id)
                    .is_some_and(|target| target != &id)
            }) {
                return Err(io::Error::other("terminal forwarding identity collision"));
            }
            updated.identities.insert(id.clone(), id.clone());
            updated.canonical_inverse.insert(id.clone(), id.clone());
            reverse.insert(id.clone(), id);
            changed = true;
        } else if crate::workspace::public_workspace_number(&id).is_some() {
            let alias = crate::workspace::generate_workspace_id();
            reverse.insert(id.clone(), alias.clone());
            updated.canonical_inverse.insert(id.clone(), alias.clone());
            updated.identities.insert(alias, id);
            changed = true;
        }
    }
    if changed {
        registry.update(|journal| {
            for id in updated
                .identities
                .keys()
                .filter(|id| crate::workspace::public_workspace_number(id).is_some())
            {
                journal.reserved_workspaces.insert(id.clone());
            }
            let current = journal
                .routes
                .entry(updated.transfer_id.clone())
                .or_insert_with(|| updated.clone());
            for (origin, destination) in updated.identities {
                if current
                    .identities
                    .get(&origin)
                    .is_some_and(|existing| existing != &destination)
                {
                    return Err(io::Error::other("forwarded identity binding collision"));
                }
                current.identities.insert(origin, destination);
            }
            for (destination, origin) in updated.canonical_inverse {
                current
                    .canonical_inverse
                    .entry(destination)
                    .or_insert(origin);
            }
            for name in resolved_names {
                current.pending_names.remove(&name);
            }
            for (name, destination) in updated.names {
                if current
                    .names
                    .get(&name)
                    .is_some_and(|existing| existing != &destination)
                {
                    return Err(io::Error::other("forwarded agent name binding collision"));
                }
                current.names.insert(name.clone(), destination);
                journal
                    .name_routes
                    .entry(name)
                    .or_insert_with(|| vec![route.transfer_id.clone()]);
            }
            Ok(())
        })?;
    }
    visit_response(value, &mut |value| {
        if let Some(origin) = value.as_str().and_then(|id| reverse.get(id)) {
            *value = Value::String(origin.clone());
        }
    });
    Ok(())
}

fn exchange_request(destination: &str, request: &Value, timeout: Duration) -> io::Result<Value> {
    use std::io::{BufRead, BufReader, Read};
    let target = crate::session::parse_target_name(destination)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let mut remote =
        crate::ipc::connect_local_stream(&crate::session::api_socket_path_for(target.as_deref()))?;
    remote.set_send_timeout(Some(timeout))?;
    remote.set_recv_timeout(Some(timeout))?;
    serde_json::to_writer(&mut remote, request).map_err(io::Error::other)?;
    remote.write_all(b"\n")?;
    remote.flush()?;
    let mut line = String::new();
    BufReader::new(remote)
        .take(MAX_RESPONSE_LINE as u64)
        .read_line(&mut line)?;
    if !line.ends_with('\n') {
        return Err(io::Error::other(
            "forwarded response was incomplete or oversized",
        ));
    }
    serde_json::from_str(&line).map_err(io::Error::other)
}

fn canonicalize_agent_target(
    registry: &TransferRegistry,
    forward: &mut ForwardRequest,
) -> io::Result<Option<Value>> {
    if !forward.request["method"]
        .as_str()
        .is_some_and(|method| method.starts_with("agent."))
    {
        return Ok(None);
    }
    let Some(target) = forward.request["params"]["target"]
        .as_str()
        .map(str::to_owned)
    else {
        return Ok(None);
    };
    let mut probe = serde_json::json!({"id":forward.request["id"],"method":"pane.get","params":{"pane_id":target}});
    if let Some(hops) = forward.request.get("_herdr_forward_hops") {
        probe["_herdr_forward_hops"] = hops.clone();
    }
    let response = exchange_request(
        &forward.route.destination_session,
        &probe,
        Duration::from_secs(5),
    )?;
    if response.get("error").is_some() {
        return Ok(Some(response));
    }
    let pane = &response["result"]["pane"];
    let canonical = pane["pane_id"]
        .as_str()
        .ok_or_else(|| io::Error::other("destination omitted canonical agent pane identity"))?;
    if forward.route.terminals.iter().any(|(terminal, pane_id)| {
        pane_id == &target && pane["terminal_id"].as_str() != Some(terminal.as_str())
    }) {
        return Err(io::Error::other("forwarded agent pane occupant changed"));
    }
    track_terminal_relocations(registry, &forward.route, &response)?;
    forward.request["params"]["target"] = Value::String(canonical.into());
    Ok(None)
}

pub(crate) fn request_once(
    registry: &TransferRegistry,
    mut forward: ForwardRequest,
) -> io::Result<String> {
    let mut value = match canonicalize_agent_target(registry, &mut forward)? {
        Some(error) => error,
        None => exchange_request(
            &forward.route.destination_session,
            &forward.request,
            Duration::from_secs(30),
        )?,
    };
    release_rejected_name(registry, &forward, &value)?;
    reverse_response(registry, &forward.route, &mut value)?;
    serde_json::to_string(&value).map_err(io::Error::other)
}

pub(crate) fn forward(
    stream: &mut LocalStream,
    registry: &TransferRegistry,
    mut forward: ForwardRequest,
    running: &AtomicBool,
) -> io::Result<()> {
    if let Some(mut error) = canonicalize_agent_target(registry, &mut forward)? {
        release_rejected_name(registry, &forward, &error)?;
        reverse_response(registry, &forward.route, &mut error)?;
        serde_json::to_writer(&mut *stream, &error).map_err(io::Error::other)?;
        stream.write_all(b"\n")?;
        return stream.flush();
    }
    crate::ipc::set_local_stream_polling(stream, true)?;
    if crate::ipc::local_stream_peer_closed(stream)? {
        return Ok(());
    }
    let target = crate::session::parse_target_name(&forward.route.destination_session)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let path = crate::session::api_socket_path_for(target.as_deref());
    let mut remote = crate::ipc::connect_local_stream(&path)?;
    remote.set_send_timeout(Some(Duration::from_secs(5)))?;
    serde_json::to_writer(&mut remote, &forward.request).map_err(io::Error::other)?;
    remote.write_all(b"\n")?;
    remote.flush()?;
    crate::ipc::set_local_stream_polling(&mut remote, true)?;
    crate::ipc::set_local_stream_polling(stream, true)?;
    let mut pending = Vec::new();
    let mut buffer = [0u8; 16384];
    while running.load(Ordering::Relaxed) {
        if crate::ipc::local_stream_peer_closed(stream)? {
            return Ok(());
        }
        match crate::ipc::poll_local_stream_read_count(&mut remote, &mut buffer)? {
            LocalStreamReadCount::Pending => std::thread::sleep(Duration::from_millis(25)),
            LocalStreamReadCount::Closed => return Ok(()),
            LocalStreamReadCount::Data(count) => {
                for &byte in &buffer[..count] {
                    pending.push(byte);
                    if pending.len() > MAX_RESPONSE_LINE {
                        return Err(io::Error::other("forwarded response exceeded size limit"));
                    }
                    if byte == b'\n' {
                        let mut value: Value =
                            serde_json::from_slice(&pending).map_err(io::Error::other)?;
                        release_rejected_name(registry, &forward, &value)?;
                        reverse_response(registry, &forward.route, &mut value)?;
                        crate::ipc::set_local_stream_polling(stream, false)?;
                        serde_json::to_writer(&mut *stream, &value).map_err(io::Error::other)?;
                        stream.write_all(b"\n")?;
                        stream.flush()?;
                        crate::ipc::set_local_stream_polling(stream, true)?;
                        pending.clear();
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn fixture() -> TransferRegistry {
        let registry = TransferRegistry::memory();
        registry
            .update(|journal| {
                journal.routes.insert(
                    "move".into(),
                    Route {
                        transfer_id: "move".into(),
                        destination_session: "other".into(),
                        origin_workspace: "w1".into(),
                        destination_workspace: "w7".into(),
                        identities: BTreeMap::from([
                            ("w1".into(), "w7".into()),
                            ("w1:p2".into(), "w7:p2".into()),
                            ("p_2".into(), "w7:p2".into()),
                        ]),
                        canonical_inverse: BTreeMap::from([
                            ("w7".into(), "w1".into()),
                            ("w7:p2".into(), "w1:p2".into()),
                        ]),
                        terminals: BTreeMap::new(),
                        names: BTreeMap::from([("coder".into(), "w7:p2".into())]),
                        pending_names: Default::default(),
                        ready: true,
                    },
                );
                Ok(())
            })
            .unwrap();
        registry
    }

    fn pending_source(
        workspace_id: &str,
        phase: SourcePhase,
    ) -> super::super::journal::SourceRecord {
        super::super::journal::SourceRecord {
            params: crate::api::schema::WorkspaceTransferParams {
                workspace_id: workspace_id.into(),
                destination_session: "other".into(),
                transfer_id: "pending".into(),
            },
            info: crate::api::schema::WorkspaceTransferInfo {
                transfer_id: "pending".into(),
                state: crate::api::schema::WorkspaceTransferState::Pending,
                destination_session: "other".into(),
                workspace_id: None,
                focused_tab_id: None,
                focused_pane_id: None,
                error: None,
            },
            phase,
            route: None,
            source_identities: Default::default(),
            source_names: Default::default(),
            runtime_released: false,
        }
    }

    #[test]
    fn retained_tab_aliases_take_precedence_over_exported_workspace_namespaces() {
        let registry = fixture();
        let mut state = crate::app::AppState::test_new();
        state.workspaces = vec![crate::workspace::Workspace::test_new("retained")];
        state.workspaces[0].id = "w2".into();
        let pane = state.workspaces[0].tabs[0].root_pane;
        state.public_pane_id_aliases.insert("w1:p9".into(), pane);
        state.pane_id_aliases.insert(2, pane);
        registry.sync_live_bindings(&state).unwrap();
        for value in [
            serde_json::json!({"id":"pane", "method":"pane.get", "params":{"pane_id":"w1:p9"}}),
            serde_json::json!({"id":"raw", "method":"pane.get", "params":{"pane_id":"p_2"}}),
            serde_json::json!({"id":"agent", "method":"agent.get", "params":{"target":"w1:p9"}}),
        ] {
            let request: Request = serde_json::from_value(value.clone()).unwrap();
            assert!(resolve(&registry, &request, &value.to_string())
                .unwrap()
                .is_none());
        }
        for phase in [SourcePhase::Preparing, SourcePhase::CommitDecided] {
            registry
                .update(|journal| {
                    journal
                        .sources
                        .insert("pending".into(), pending_source("w1", phase));
                    Ok(())
                })
                .unwrap();
            let value = serde_json::json!({"id":"local", "method":"pane.get", "params":{"pane_id":"w1:p9"}});
            let request: Request = serde_json::from_value(value.clone()).unwrap();
            assert!(resolve(&registry, &request, &value.to_string())
                .unwrap()
                .is_none());
            let value = serde_json::json!({"id":"moving", "method":"pane.get", "params":{"pane_id":"w1:p2"}});
            let request: Request = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(
                resolve(&registry, &request, &value.to_string())
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::WouldBlock
            );
        }
        registry
            .update(|journal| {
                journal
                    .sources
                    .insert("owner".into(), pending_source("w2", SourcePhase::Preparing));
                Ok(())
            })
            .unwrap();
        let value = serde_json::json!({"id":"owner-moving", "method":"pane.get", "params":{"pane_id":"w1:p9"}});
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            resolve(&registry, &request, &value.to_string())
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn explicit_moved_pane_routes_precede_an_older_workspace_prefix_route() {
        let registry = fixture();
        registry
            .update(|journal| {
                let mut route = journal.routes["move"].clone();
                route.transfer_id = "second".into();
                route.origin_workspace = "w2".into();
                route.destination_workspace = "w9".into();
                route.destination_session = "third".into();
                route.identities =
                    BTreeMap::from([("w2".into(), "w9".into()), ("w1:p9".into(), "w9:p3".into())]);
                route.names.clear();
                route.canonical_inverse.clear();
                journal.routes.insert("second".into(), route);
                Ok(())
            })
            .unwrap();
        let value =
            serde_json::json!({"id":"moved", "method":"pane.get", "params":{"pane_id":"w1:p9"}});
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        let forwarded = resolve(&registry, &request, &value.to_string())
            .unwrap()
            .unwrap();
        assert_eq!(forwarded.route.destination_session, "third");
        assert_eq!(forwarded.request["params"]["pane_id"], "w9:p3");
    }

    #[test]
    fn agent_names_cannot_capture_structural_ids_or_raw_aliases() {
        let registry = fixture();
        registry
            .update(|journal| {
                let route = journal.routes.get_mut("move").unwrap();
                route.names.insert("w2".into(), "w7:p3".into());
                route.names.insert("p_2".into(), "w7:p3".into());
                Ok(())
            })
            .unwrap();
        let local: Request = serde_json::from_value(serde_json::json!({"id":"local","method":"workspace.get","params":{"workspace_id":"w2"}})).unwrap();
        assert!(
            resolve(&registry, &local, &serde_json::to_string(&local).unwrap())
                .unwrap()
                .is_none()
        );
        let agent: Request = serde_json::from_value(
            serde_json::json!({"id":"agent","method":"agent.get","params":{"target":"p_2"}}),
        )
        .unwrap();
        let forwarded = resolve(&registry, &agent, &serde_json::to_string(&agent).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(forwarded.request["params"]["target"], "w7:p3");
    }

    #[test]
    fn learned_identity_and_workspace_prefix_routes_deduplicate_when_equivalent() {
        let registry = fixture();
        registry
            .update(|journal| {
                journal
                    .routes
                    .get_mut("move")
                    .unwrap()
                    .identities
                    .insert("w2:p9".into(), "w9:p9".into());
                let mut other = journal.routes["move"].clone();
                other.transfer_id = "second".into();
                other.origin_workspace = "w2".into();
                other.destination_workspace = "w9".into();
                other.identities = BTreeMap::from([("w2".into(), "w9".into())]);
                other.canonical_inverse = BTreeMap::from([("w9".into(), "w2".into())]);
                other.names.clear();
                journal.routes.insert("second".into(), other);
                Ok(())
            })
            .unwrap();
        let request: Request = serde_json::from_value(
            serde_json::json!({"id":"pane","method":"pane.get","params":{"pane_id":"w2:p9"}}),
        )
        .unwrap();
        let forward = resolve(
            &registry,
            &request,
            &serde_json::to_string(&request).unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(forward.request["params"]["pane_id"], "w9:p9");
        let mut response =
            serde_json::json!({"result":{"pane":{"workspace_id":"w9","pane_id":"w9:p9"}}});
        reverse_response(&registry, &forward.route, &mut response).unwrap();
        assert_eq!(response["result"]["pane"]["pane_id"], "w2:p9");
    }

    #[test]
    fn forwarded_name_reservations_reject_live_origin_collision_and_clean_definitive_rejection() {
        let registry = fixture();
        registry
            .update(|journal| {
                journal
                    .live_names
                    .insert("local".into(), "term_local".into());
                Ok(())
            })
            .unwrap();
        let colliding: Request = serde_json::from_value(serde_json::json!({"id":"rename","method":"agent.rename","params":{"target":"coder","name":"local"}})).unwrap();
        assert!(resolve(
            &registry,
            &colliding,
            &serde_json::to_string(&colliding).unwrap()
        )
        .is_err());
        let request: Request = serde_json::from_value(serde_json::json!({"id":"rename","method":"agent.rename","params":{"target":"coder","name":"new-name"}})).unwrap();
        let forward = resolve(
            &registry,
            &request,
            &serde_json::to_string(&request).unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            registry.snapshot().unwrap().routes["move"].names["new-name"],
            "w7:p2"
        );
        release_rejected_name(
            &registry,
            &forward,
            &serde_json::json!({"id":"rename","error":{"code":"rejected","message":"collision"}}),
        )
        .unwrap();
        assert!(!registry.snapshot().unwrap().routes["move"]
            .names
            .contains_key("new-name"));
        assert_eq!(
            registry.snapshot().unwrap().routes["move"].names["coder"],
            "w7:p2"
        );
    }

    #[test]
    fn hook_reports_wait_for_ownership_while_interactive_mutations_remain_fenced() {
        let registry = fixture();
        registry
            .update(|journal| {
                journal.routes.get_mut("move").unwrap().ready = false;
                Ok(())
            })
            .unwrap();
        let report_value = serde_json::json!({
            "id":"report", "method":"pane.report_agent",
            "params":{"pane_id":"w1:p2", "source":"custom:move", "agent":"worker", "state":"working", "seq":8}
        });
        let report: Request = serde_json::from_value(report_value.clone()).unwrap();
        let mutation_value = serde_json::json!({"id":"input", "method":"pane.send_text", "params":{"pane_id":"w1:p2", "text":"do not replay"}});
        let mutation: Request = serde_json::from_value(mutation_value.clone()).unwrap();
        let running = AtomicBool::new(true);
        assert_eq!(
            resolve_when_ready(&registry, &mutation, &mutation_value.to_string(), &running)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        let delayed = registry.clone();
        let ready = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            delayed
                .update(|journal| {
                    journal.routes.get_mut("move").unwrap().ready = true;
                    Ok(())
                })
                .unwrap();
        });
        let forwarded = resolve_when_ready(&registry, &report, &report_value.to_string(), &running)
            .unwrap()
            .unwrap();
        ready.join().unwrap();
        assert_eq!(forwarded.request["params"]["pane_id"], "w7:p2");
        assert_eq!(forwarded.request["params"]["seq"], 8);
        assert_eq!(forwarded.request["params"]["state"], "working");
    }

    #[test]
    fn forwarding_hops_are_bounded() {
        let registry = fixture();
        let mut request =
            serde_json::json!({"id":"loop","method":"pane.get","params":{"pane_id":"w1:p2"}});
        request["_herdr_forward_hops"] =
            serde_json::json!(["a", "b", "c", "d", "e", "f", "g", "h"]);
        let typed: Request = serde_json::from_value(request.clone()).unwrap();
        assert!(resolve(&registry, &typed, &request.to_string()).is_err());
    }

    #[test]
    fn layout_template_ids_do_not_select_forwarding_targets() {
        let registry = fixture();
        let root = serde_json::json!({
            "type":"split", "direction":"right", "ratio":0.5,
            "first":{"type":"pane", "pane_id":"w1:p2"},
            "second":{"type":"pane", "pane_id":"opaque template ID"}
        });
        let mut value =
            serde_json::json!({"id":"layout", "method":"layout.apply", "params":{"root":root}});
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        assert!(targets(&request).is_empty());
        assert!(resolve(&registry, &request, &value.to_string())
            .unwrap()
            .is_none());
        for (field, source, destination) in
            [("workspace_id", "w1", "w7"), ("tab_id", "w1:t3", "w7:t3")]
        {
            value["params"][field] = Value::String(source.into());
            let request: Request = serde_json::from_value(value.clone()).unwrap();
            let forward = resolve(&registry, &request, &value.to_string())
                .unwrap()
                .unwrap();
            assert_eq!(forward.request["params"][field], destination);
            assert_eq!(forward.request["params"]["root"], root);
            value["params"].as_object_mut().unwrap().remove(field);
        }
    }

    #[tokio::test]
    async fn layout_template_ids_cannot_bypass_active_workspace_fence() {
        let (_, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state = crate::app::AppState::test_with_adversarial_identity_state();
        app.state
            .workspaces
            .push(crate::workspace::Workspace::test_new("unfenced"));
        app.state.ensure_test_terminals();
        app.state.assert_invariants_for_test();
        let fenced = app.state.workspaces[0].id.clone();
        let unfenced = app.state.workspaces[1].id.clone();
        let template = app
            .public_pane_id(1, app.state.workspaces[1].tabs[0].root_pane)
            .unwrap();
        app.state.workspace_transfer_fences.insert(fenced.clone());
        let mut value = serde_json::json!({"id":"layout", "method":"layout.apply", "params":{"root":{"type":"pane", "pane_id":template}}});
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        assert!(app.transfer_request_is_fenced_at(&request, Some(0)));
        assert!(!app.transfer_request_is_fenced_at(&request, Some(1)));
        for (workspace, expected) in [(fenced, true), (unfenced, false)] {
            value["params"]["workspace_id"] = Value::String(workspace);
            let request: Request = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(
                app.transfer_request_is_fenced_at(&request, Some(0)),
                expected
            );
        }
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn rewrites_structural_targets_not_hook_payloads() {
        let registry = fixture();
        let value = serde_json::json!({"id":"hook", "method":"pane.report_agent", "params":{
            "pane_id":"w1:p2", "source":"hook", "agent":"claude", "state":"idle", "message":"w1:p2", "resume_argv":["w1:p2"], "agent_session_path":"w1:p2"
        }});
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        let forwarded = resolve(&registry, &request, &value.to_string())
            .unwrap()
            .unwrap();
        assert_eq!(forwarded.request["params"]["pane_id"], "w7:p2");
        assert_eq!(forwarded.request["params"]["message"], "w1:p2");
        assert_eq!(forwarded.request["params"]["resume_argv"][0], "w1:p2");
        assert_eq!(forwarded.request["params"]["agent_session_path"], "w1:p2");
    }

    #[test]
    fn tab_transfer_routes_both_endpoints_and_rejects_mixed_sessions() {
        let registry = fixture();
        for (tab_id, workspace_id) in [("w1:t3", "w2"), ("w2:t3", "w1")] {
            let value = serde_json::json!({
                "id":"move-tab", "method":"tab.move_to_workspace",
                "params":{"tab_id":tab_id,"workspace_id":workspace_id}
            });
            let request: Request = serde_json::from_value(value.clone()).unwrap();
            let endpoints = targets(&request);
            assert_eq!(endpoints.len(), 2);
            assert!(endpoints.iter().any(|(id, _)| id == tab_id));
            assert!(endpoints.iter().any(|(id, _)| id == workspace_id));
            assert_eq!(
                resolve(&registry, &request, &value.to_string())
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
        registry
            .update(|journal| {
                let mut second = journal.routes["move"].clone();
                second.transfer_id = "second".into();
                second.origin_workspace = "w2".into();
                second.destination_workspace = "w8".into();
                second.destination_session = "third".into();
                second.identities = BTreeMap::from([("w2".into(), "w8".into())]);
                second.canonical_inverse = BTreeMap::from([("w8".into(), "w2".into())]);
                second.names.clear();
                journal.routes.insert("second".into(), second);
                Ok(())
            })
            .unwrap();
        let value = serde_json::json!({
            "id":"move-tab", "method":"tab.move_to_workspace",
            "params":{"tab_id":"w1:t3","workspace_id":"w2"}
        });
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            resolve(&registry, &request, &value.to_string())
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        registry
            .update(|journal| {
                journal
                    .routes
                    .get_mut("second")
                    .unwrap()
                    .destination_session = "other".into();
                Ok(())
            })
            .unwrap();
        let forwarded = resolve(&registry, &request, &value.to_string())
            .unwrap()
            .unwrap();
        assert_eq!(forwarded.request["params"]["tab_id"], "w7:t3");
        assert_eq!(forwarded.request["params"]["workspace_id"], "w8");
    }

    #[test]
    fn mixed_session_requests_reject_before_forwarding() {
        let registry = fixture();
        let request: Request = serde_json::from_value(serde_json::json!({"id":"swap","method":"pane.swap","params":{"source_pane_id":"w1:p2","target_pane_id":"w2:p1"}})).unwrap();
        assert!(resolve(
            &registry,
            &request,
            &serde_json::to_string(&request).unwrap()
        )
        .is_err());
    }

    #[test]
    fn name_binding_and_nested_wait_target_are_forwarded() {
        let registry = fixture();
        for value in [
            serde_json::json!({"id":"a","method":"agent.wait","params":{"target":"coder","until":["idle"]}}),
            serde_json::json!({"id":"b","method":"events.wait","params":{"match_event":{"event":"pane_exited","pane_id":"w1:p2"}}}),
        ] {
            let request: Request = serde_json::from_value(value.clone()).unwrap();
            let forwarded = resolve(&registry, &request, &value.to_string())
                .unwrap()
                .unwrap();
            assert!(forwarded.request.to_string().contains("w7:p2"));
        }
    }

    #[test]
    fn reverses_response_and_event_identities_without_payload_replacement() {
        let registry = fixture();
        let route = registry.snapshot().unwrap().routes["move"].clone();
        let mut value = serde_json::json!({"result":{"type":"pane_info","pane":{"workspace_id":"w7","pane_id":"w7:p2","tokens":{"pane_id":"w7:p2"},"text":"w7:p2","cwd":"w7:p2"}}});
        reverse_response(&registry, &route, &mut value).unwrap();
        assert_eq!(value["result"]["pane"]["pane_id"], "w1:p2");
        assert_eq!(value["result"]["pane"]["tokens"]["pane_id"], "w7:p2");
        assert_eq!(value["result"]["pane"]["cwd"], "w7:p2");
    }
}
