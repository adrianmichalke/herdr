use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::layout::PaneId;
use crate::workspace::Workspace;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneAliasesSnapshot {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub raw: HashMap<u32, u32>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub public: HashMap<String, u32>,
}

#[derive(Clone, Default)]
pub(crate) struct RestoredPaneAliases {
    pub raw: HashMap<u32, PaneId>,
    pub public: HashMap<String, PaneId>,
}

impl PaneAliasesSnapshot {
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty() && self.public.is_empty()
    }

    pub(crate) fn capture(
        workspaces: &[Workspace],
        raw: &HashMap<u32, PaneId>,
        public: &HashMap<String, PaneId>,
    ) -> Self {
        let live = live_panes(workspaces);
        let canonical = canonical_panes(workspaces);
        Self {
            raw: raw
                .iter()
                .filter(|(_, pane)| live.contains(*pane))
                .map(|(raw, pane)| (*raw, pane.raw()))
                .collect(),
            public: public
                .iter()
                .filter(|(alias, pane)| {
                    live.contains(*pane) && public_alias_is_safe(alias, **pane, &canonical)
                })
                .map(|(alias, pane)| (alias.clone(), pane.raw()))
                .collect(),
        }
    }

    pub(crate) fn rebind(
        &self,
        pane_ids: &HashMap<u32, PaneId>,
        workspaces: &mut [Workspace],
    ) -> RestoredPaneAliases {
        let live = live_panes(workspaces);
        let canonical = canonical_panes(workspaces);
        let rebound = |raw: &u32| {
            pane_ids
                .get(raw)
                .copied()
                .filter(|pane| live.contains(pane))
        };
        let mut raw: HashMap<_, _> = pane_ids
            .iter()
            .filter(|(raw, pane)| **raw != pane.raw() && live.contains(*pane))
            .map(|(raw, pane)| (*raw, *pane))
            .collect();
        for (alias, target) in &self.raw {
            raw.remove(alias);
            if let Some(pane) = rebound(target) {
                raw.insert(*alias, pane);
            }
        }
        let public: HashMap<_, _> = self
            .public
            .iter()
            .filter_map(|(alias, target)| rebound(target).map(|pane| (alias, pane)))
            .filter(|(alias, pane)| public_alias_is_safe(alias, *pane, &canonical))
            .map(|(alias, pane)| (alias.clone(), pane))
            .collect();
        reserve_pane_alias_namespaces(workspaces, public.keys());
        RestoredPaneAliases { raw, public }
    }
}

impl super::SessionSnapshot {
    pub(crate) fn reserve_pane_alias_namespaces(&mut self) {
        for alias in self.pane_aliases.public.keys() {
            let Some((workspace_id, next)) = public_alias_namespace(alias) else {
                continue;
            };
            for workspace in &mut self.workspaces {
                if workspace.id.as_deref() == Some(workspace_id) {
                    workspace.next_public_pane_number = workspace.next_public_pane_number.max(next);
                }
            }
        }
    }
}

pub(crate) fn reserve_pane_alias_namespaces<'a>(
    workspaces: &mut [Workspace],
    aliases: impl IntoIterator<Item = &'a String>,
) {
    for alias in aliases {
        let Some((workspace_id, next)) = public_alias_namespace(alias) else {
            continue;
        };
        crate::workspace::reserve_workspace_id(workspace_id);
        for workspace in workspaces
            .iter_mut()
            .filter(|workspace| workspace.id == workspace_id)
        {
            workspace.next_public_pane_number = workspace.next_public_pane_number.max(next);
        }
    }
}

pub(super) fn public_alias_namespace(alias: &str) -> Option<(&str, usize)> {
    let (workspace_id, encoded) = alias.rsplit_once(":p")?;
    if workspace_id.is_empty()
        || workspace_id.contains(':')
        || workspace_id.chars().any(char::is_whitespace)
    {
        return None;
    }
    let number = crate::workspace::decode_public_number(encoded)?;
    if number == 0 || crate::workspace::public_pane_id_for_number(workspace_id, number) != alias {
        return None;
    }
    Some((workspace_id, number.checked_add(1)?))
}

fn live_panes(workspaces: &[Workspace]) -> HashSet<PaneId> {
    workspaces
        .iter()
        .flat_map(|workspace| &workspace.tabs)
        .flat_map(|tab| tab.panes.keys().copied())
        .collect()
}

fn canonical_panes(workspaces: &[Workspace]) -> HashMap<String, Option<PaneId>> {
    let mut canonical = HashMap::new();
    for workspace in workspaces {
        for (&pane, &number) in &workspace.public_pane_numbers {
            if workspace.pane_state(pane).is_some() {
                canonical
                    .entry(crate::workspace::public_pane_id_for_number(
                        &workspace.id,
                        number,
                    ))
                    .and_modify(|existing| {
                        if *existing != Some(pane) {
                            *existing = None;
                        }
                    })
                    .or_insert(Some(pane));
            }
        }
    }
    canonical
}

fn public_alias_is_safe(
    alias: &str,
    pane: PaneId,
    canonical: &HashMap<String, Option<PaneId>>,
) -> bool {
    public_alias_namespace(alias).is_some()
        && canonical
            .get(alias)
            .is_none_or(|existing| *existing == Some(pane))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::app::AppState;

    fn workspaces() -> Vec<Workspace> {
        let mut workspaces = vec![
            Workspace::test_new("source"),
            Workspace::test_new("destination"),
        ];
        workspaces[0].id = "w1".into();
        workspaces[1].id = "w2".into();
        workspaces
    }

    #[test]
    fn move_aliases_rebind_to_actual_restored_panes_and_reserve_source_numbers() {
        let old = workspaces();
        let moved = old[1].tabs[0].root_pane;
        let mut restored = workspaces();
        let actual = restored[1].tabs[0].root_pane;
        let snapshot = PaneAliasesSnapshot {
            raw: HashMap::from([(91, moved.raw())]),
            public: HashMap::from([("w1:p7".into(), moved.raw())]),
        };
        let aliases = snapshot.rebind(&HashMap::from([(moved.raw(), actual)]), &mut restored);

        assert_eq!(aliases.raw.get(&91), Some(&actual));
        assert_eq!(aliases.public.get("w1:p7"), Some(&actual));
        assert_eq!(restored[0].next_public_pane_number, 8);
        assert!(restored[1].pane_state(actual).is_some());
    }

    #[test]
    fn absent_alias_workspaces_are_reserved_before_new_workspaces_are_allocated() {
        let mut restored = workspaces();
        let target = restored[1].tabs[0].root_pane;
        let snapshot = PaneAliasesSnapshot {
            raw: HashMap::new(),
            public: HashMap::from([("wZ:p7".into(), 17)]),
        };
        let aliases = snapshot.rebind(&HashMap::from([(17, target)]), &mut restored);
        assert_eq!(aliases.public.get("wZ:p7"), Some(&target));
        let next = crate::workspace::generate_workspace_id();
        assert!(
            crate::workspace::public_workspace_number(&next).unwrap()
                > crate::workspace::public_workspace_number("wZ").unwrap()
        );
    }

    #[test]
    fn two_successive_handoffs_compose_raw_aliases_instead_of_overwriting_them() {
        let first = workspaces();
        let first_source = first[0].tabs[0].root_pane;
        let first_target = first[1].tabs[0].root_pane;
        let mut second = workspaces();
        let second_source = second[0].tabs[0].root_pane;
        let second_target = second[1].tabs[0].root_pane;
        let original = PaneAliasesSnapshot {
            raw: HashMap::from([
                (first_source.raw(), first_target.raw()),
                (90, first_target.raw()),
            ]),
            public: HashMap::from([("w1:p7".into(), first_target.raw())]),
        };
        let rebound = original.rebind(
            &HashMap::from([
                (first_source.raw(), second_source),
                (first_target.raw(), second_target),
            ]),
            &mut second,
        );
        assert_eq!(rebound.raw.get(&first_source.raw()), Some(&second_target));
        let captured = PaneAliasesSnapshot::capture(&second, &rebound.raw, &rebound.public);
        let serialized = serde_json::to_string(&captured).unwrap();
        let captured: PaneAliasesSnapshot = serde_json::from_str(&serialized).unwrap();
        let mut third = workspaces();
        let third_source = third[0].tabs[0].root_pane;
        let third_target = third[1].tabs[0].root_pane;
        let aliases = captured.rebind(
            &HashMap::from([
                (second_source.raw(), third_source),
                (second_target.raw(), third_target),
            ]),
            &mut third,
        );

        for raw in [
            90,
            first_source.raw(),
            first_target.raw(),
            second_target.raw(),
        ] {
            assert_eq!(
                aliases.raw.get(&raw),
                Some(&third_target),
                "raw alias {raw}"
            );
        }
        assert_eq!(aliases.public.get("w1:p7"), Some(&third_target));
        assert_eq!(third[0].next_public_pane_number, 8);
    }

    #[test]
    fn invalid_targets_and_canonical_public_collisions_are_dropped_without_raw_fallback() {
        let mut restored = workspaces();
        let source = restored[0].tabs[0].root_pane;
        let target = restored[1].tabs[0].root_pane;
        let snapshot = PaneAliasesSnapshot {
            raw: HashMap::from([(41, target.raw()), (42, 999)]),
            public: HashMap::from([
                ("w1:p1".into(), 17),
                ("w1:p7".into(), 999),
                ("not-a-pane".into(), 17),
                ("w1:p".into(), 17),
                (
                    format!("w1:p{}", crate::workspace::encode_public_number(usize::MAX)),
                    17,
                ),
                ("w1:p8".into(), 17),
            ]),
        };
        let aliases = snapshot.rebind(&HashMap::from([(17, target)]), &mut restored);

        assert!(!aliases.raw.contains_key(&41));
        assert!(!aliases.raw.contains_key(&42));
        assert_eq!(aliases.public, HashMap::from([("w1:p8".into(), target)]));
        assert_ne!(source, target);
        assert_eq!(restored[0].next_public_pane_number, 9);
    }

    #[test]
    fn capture_ignores_stale_aliases_and_retains_raw_keys_overlapping_live_panes() {
        let mut state = AppState::test_with_adversarial_identity_state();
        let first = state.workspaces[0].tabs[0].root_pane;
        let second = state.workspaces[0].tabs[1].root_pane;
        state.pane_id_aliases.insert(first.raw(), second);
        state.pane_id_aliases.insert(99, PaneId::from_raw(u32::MAX));
        state.public_pane_id_aliases.insert("w9:p5".into(), second);
        state
            .public_pane_id_aliases
            .insert("w9:p6".into(), PaneId::from_raw(u32::MAX));
        let snapshot = PaneAliasesSnapshot::capture(
            &state.workspaces,
            &state.pane_id_aliases,
            &state.public_pane_id_aliases,
        );

        assert_eq!(snapshot.raw, HashMap::from([(first.raw(), second.raw())]));
        assert_eq!(
            snapshot.public,
            HashMap::from([("w9:p5".into(), second.raw())])
        );
        state.pane_id_aliases.remove(&99);
        state.public_pane_id_aliases.remove("w9:p6");
        state.assert_invariants_for_test();
    }
}
