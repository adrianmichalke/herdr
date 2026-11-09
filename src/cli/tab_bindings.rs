use clap::Command;

use crate::api::schema::tabs::{
    tab_id, TabCreateProjection, TabListProjection, TabMoveToWorkspaceProjection,
    TabRenameProjection,
};
use crate::api::schema::{Method, Request, TabTarget};

use super::super::projection::{self, cli_projection, Field, Leaf, ParseError};

cli_projection! {
    pub(crate) TabTargetProjection for TabTarget {
        tab_id => Field::new(String::new()).input(tab_id()),
    }
}

const LEAVES: [Leaf; 7] = [
    Leaf {
        name: "list",
        about: "List tabs",
        id: "cli:tab:list",
        arguments: TabListProjection::arguments,
        method: |matches| TabListProjection::from_matches(matches).map(Method::TabList),
    },
    Leaf {
        name: "create",
        about: "Create a tab",
        id: "cli:tab:create",
        arguments: TabCreateProjection::arguments,
        method: |matches| TabCreateProjection::from_matches(matches).map(Method::TabCreate),
    },
    Leaf {
        name: "get",
        about: "Show a tab",
        id: "cli:tab:get",
        arguments: TabTargetProjection::arguments,
        method: |matches| TabTargetProjection::from_matches(matches).map(Method::TabGet),
    },
    Leaf {
        name: "focus",
        about: "Focus a tab",
        id: "cli:tab:focus",
        arguments: TabTargetProjection::arguments,
        method: |matches| TabTargetProjection::from_matches(matches).map(Method::TabFocus),
    },
    Leaf {
        name: "rename",
        about: "Rename a tab",
        id: "cli:tab:rename",
        arguments: TabRenameProjection::arguments,
        method: |matches| TabRenameProjection::from_matches(matches).map(Method::TabRename),
    },
    Leaf {
        name: "move-to-workspace",
        about: "Move a live tab to another workspace without focusing it",
        id: "cli:tab:move-to-workspace",
        arguments: TabMoveToWorkspaceProjection::arguments,
        method: |matches| {
            TabMoveToWorkspaceProjection::from_matches(matches).map(Method::TabMoveToWorkspace)
        },
    },
    Leaf {
        name: "close",
        about: "Close a tab",
        id: "cli:tab:close",
        arguments: TabTargetProjection::arguments,
        method: |matches| TabTargetProjection::from_matches(matches).map(Method::TabClose),
    },
];

pub(crate) fn command() -> Command {
    Command::new("tab")
        .about("Manage tabs over the socket API")
        .subcommand_required(true)
        .subcommands(LEAVES.iter().map(Leaf::command))
}

pub(crate) fn request(args: &[String]) -> Result<Request, ParseError> {
    projection::group_request(command(), &LEAVES, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Request, ParseError> {
        request(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn tab_projection_preserves_requests_defaults_and_maps() {
        for (args, expected) in [
            (
                vec!["list"],
                serde_json::json!({"id":"cli:tab:list","method":"tab.list","params":{}}),
            ),
            (
                vec!["create"],
                serde_json::json!({"id":"cli:tab:create","method":"tab.create","params":{"focus":false}}),
            ),
            (
                vec![
                    "create",
                    "--workspace",
                    "2",
                    "--cwd",
                    "relative",
                    "--label",
                    "first",
                    "--label",
                    "last",
                    "--focus",
                    "--no-focus",
                    "--env",
                    "K=first",
                    "--env",
                    "K=",
                    "--env",
                    "V=a=b",
                ],
                serde_json::json!({"id":"cli:tab:create","method":"tab.create","params":{"workspace_id":"2","cwd":"relative","label":"last","focus":false,"env":{"K":"","V":"a=b"}}}),
            ),
            (
                vec!["rename", "2", "two", "--words", "here"],
                serde_json::json!({"id":"cli:tab:rename","method":"tab.rename","params":{"tab_id":"2","label":"two --words here"}}),
            ),
            (
                vec!["get", "2"],
                serde_json::json!({"id":"cli:tab:get","method":"tab.get","params":{"tab_id":"2"}}),
            ),
            (
                vec!["focus", "2"],
                serde_json::json!({"id":"cli:tab:focus","method":"tab.focus","params":{"tab_id":"2"}}),
            ),
            (
                vec!["close", "tab-2"],
                serde_json::json!({"id":"cli:tab:close","method":"tab.close","params":{"tab_id":"tab-2"}}),
            ),
        ] {
            assert_eq!(
                serde_json::to_value(parse(&args).unwrap()).unwrap(),
                expected,
                "{args:?}"
            );
        }
        assert!(
            matches!(parse(&["create", "--env", "=value"]), Err(ParseError::Usage(message)) if message == "env key must not be empty")
        );
    }

    #[test]
    fn tab_transfer_projection_requires_both_targets_and_has_no_focus_flag() {
        let request = parse(&["move-to-workspace", "w1:t2", "--workspace", "w3"]).unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "id":"cli:tab:move-to-workspace",
                "method":"tab.move_to_workspace",
                "params":{"tab_id":"w1:t2","workspace_id":"w3"}
            })
        );
        for args in [
            vec!["move-to-workspace"],
            vec!["move-to-workspace", "w1:t2"],
            vec!["move-to-workspace", "w1:t2", "--workspace"],
            vec!["move-to-workspace", "w1:t2", "--workspace", "w3", "--focus"],
            vec!["move-to-workspace", "w1:t2", "--workspace", "w3", "extra"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn tab_projection_clap_grammar_and_ordered_focus() {
        let value = serde_json::to_value(
            parse(&[
                "create",
                "--focus",
                "--no-focus",
                "--focus",
                "--cwd=relative",
            ])
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            value["params"],
            serde_json::json!({"cwd":"relative", "focus":true})
        );
        assert!(
            matches!(parse(&["get", "2", "--help"]), Err(ParseError::Clap(error)) if error.exit_code() == 0)
        );
        for args in [
            vec!["get"],
            vec!["get", "2", "extra"],
            vec!["create", "--cwd"],
        ] {
            assert!(matches!(parse(&args), Err(ParseError::Clap(error)) if error.exit_code() == 2));
        }
        command().debug_assert();
    }
}
