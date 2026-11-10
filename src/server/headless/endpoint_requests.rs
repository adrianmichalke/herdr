use super::*;

impl HeadlessServer {
    pub(super) fn handle_client_shell_endpoint_request(
        &mut self,
        client_id: u64,
        boot_id: String,
        mut request: Box<api::schema::Request>,
    ) -> bool {
        let Some(client) = self.clients.get(&client_id) else {
            return false;
        };
        if !matches!(client.mode, ClientConnectionMode::ClientShell) {
            self.remove_client_and_resize_if_needed(client_id);
            return true;
        }
        let request_id = request.id.clone();
        if !crate::server::client_commands::supports_client_shell_method(&request.method) {
            let message = crate::server::client_commands::error_message(
                boot_id,
                request_id,
                "unsupported_endpoint_command",
                "this method is not available through the client shell command lane",
            );
            self.send_to_client(client_id, message);
            return false;
        }
        if boot_id != self.client_shell_boot_id {
            let message = crate::server::client_commands::error_message(
                boot_id,
                request_id,
                "stale_boot",
                "endpoint command targeted an earlier server boot",
            );
            self.send_to_client(client_id, message);
            return false;
        }
        let surface_active = client.shell_surface_active;
        if let api::schema::Method::ClientShellSurfaceSet(params) = &request.method {
            let Some((changed, projection_revision)) =
                self.set_client_shell_surface_active(client_id, params.active)
            else {
                return false;
            };
            self.send_to_client(
                client_id,
                crate::server::client_commands::success_message_with_result(
                    boot_id,
                    request_id,
                    api::schema::ResponseResult::ClientShellSurfaceSet {
                        active: params.active,
                        projection_revision,
                    },
                ),
            );
            return changed;
        }
        if client.shell_endpoint_command_in_flight {
            let message = crate::server::client_commands::error_message(
                boot_id,
                request_id,
                "endpoint_busy",
                "this endpoint is still processing another command",
            );
            self.send_to_client(client_id, message);
            return false;
        }
        let transfer_status = matches!(
            request.method,
            api::schema::Method::WorkspaceTransferStatus(_)
        );
        if !surface_active && !transfer_status {
            let message = crate::server::client_commands::error_message(
                boot_id,
                request_id,
                "surface_inactive",
                "this method requires an active client shell surface",
            );
            self.send_to_client(client_id, message);
            return false;
        }

        let pending_changed =
            if matches!(request.method, api::schema::Method::PaneFocusIfSession(_)) {
                if self.shutting_down || self.host_shutdown_requested() {
                    let message = crate::server::client_commands::error_message(
                        boot_id,
                        request_id,
                        "server_unavailable",
                        "server is shutting down",
                    );
                    self.send_to_client(client_id, message);
                    return false;
                }
                self.drain_all_internal_events_with_forwarding()
                    | self.app.expire_due_metadata(Instant::now())
            } else {
                false
            };
        if self.client_request_is_transfer_fenced(client_id, &request) {
            let message = crate::server::client_commands::error_message(
                boot_id,
                request_id,
                "workspace_busy",
                "workspace is being transferred; retry after the move completes",
            );
            self.send_to_client(client_id, message);
            return pending_changed;
        }

        if let api::schema::Method::PaneFocusIfSession(params) = &request.method {
            if self.shutting_down || self.host_shutdown_requested() {
                let message = crate::server::client_commands::error_message(
                    boot_id,
                    request_id,
                    "server_unavailable",
                    "server is shutting down",
                );
                self.send_to_client(client_id, message);
                return pending_changed;
            }
            if !self.app.pane_focus_session_is_current(params) {
                let message = crate::server::client_commands::error_message(
                    boot_id,
                    request_id,
                    "stale_agent_session",
                    "pane or native agent session changed",
                );
                self.send_to_client(client_id, message);
                return pending_changed;
            }
        }

        let api_request_id = format!(
            "endpoint:{}:{client_id}:{request_id}",
            self.client_shell_boot_id
        );
        request.id = api_request_id.clone();
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        if let Err(err) = crate::server::client_commands::spawn_response_waiter(
            client_id,
            boot_id.clone(),
            request_id.clone(),
            response_rx,
            self.server_event_tx.clone(),
        ) {
            let message = crate::server::client_commands::error_message(
                boot_id,
                request_id,
                "server_unavailable",
                format!("failed to start endpoint response bridge: {err}"),
            );
            self.send_to_client(client_id, message);
            return pending_changed;
        }
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.shell_endpoint_command_in_flight = true;
            // A later source restore has a new projection revision. Keep this request's lease
            // so a delayed worktree response cannot focus a pane after endpoint switching.
            client.shell_endpoint_command_surface_revision = Some(client.shell_projection_revision);
            let deferred_worktree = matches!(
                &request.method,
                api::schema::Method::WorktreeCreate(_)
                    | api::schema::Method::WorktreeRemove(_)
                    | api::schema::Method::WorktreeList(_)
                    | api::schema::Method::WorktreeOpen(_)
            );
            let deferred_navigation = match &request.method {
                api::schema::Method::WorktreeCreate(params) => params.focus,
                api::schema::Method::WorktreeOpen(params) => params.focus,
                _ => false,
            };
            client.shell_deferred_navigation_request_id =
                deferred_worktree.then(|| api_request_id.clone());
            client.shell_deferred_navigation_response = deferred_navigation.then(Vec::new);
        }
        let foreground_changed = !transfer_status && self.promote_client_to_foreground(client_id);
        pending_changed
            | foreground_changed
            | self.handle_client_shell_api_request_after_internal_events_drained(
                client_id,
                api::ApiRequestMessage {
                    request: *request,
                    respond_to,
                    response_write_complete: None,
                },
            )
    }
}
