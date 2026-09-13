//! Deterministic world-space layout for the subagent hierarchy.

use orvek_subagents::AgentId;
use std::collections::{HashMap, HashSet};

pub(super) const NODE_WIDTH: i32 = 24;
pub(super) const NODE_HEIGHT: i32 = 4;
pub(super) const HORIZONTAL_GAP: i32 = 6;
pub(super) const VERTICAL_GAP: i32 = 5;

#[derive(Clone, Copy)]
pub(super) struct LayoutNode {
    pub(super) id: AgentId,
    pub(super) parent: Option<AgentId>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct WorldPoint {
    pub(super) x: f64,
    pub(super) y: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct NodePosition {
    pub(super) center_x: i32,
    pub(super) top: i32,
}

pub(super) struct TreeLayout {
    positions: HashMap<AgentId, NodePosition>,
    parents: HashMap<AgentId, AgentId>,
    children: HashMap<AgentId, Vec<AgentId>>,
    roots: Vec<AgentId>,
    order: Vec<(AgentId, usize)>,
    bounds: Option<LayoutBounds>,
}

struct LayoutBounds {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl TreeLayout {
    pub(super) fn new(nodes: &[LayoutNode]) -> Self {
        let ids = nodes.iter().map(|node| node.id).collect::<HashSet<_>>();
        let mut parents = HashMap::new();
        let mut children = HashMap::<AgentId, Vec<AgentId>>::new();

        for node in nodes {
            let parent = node.parent.filter(|parent| ids.contains(parent));
            if let Some(parent) = parent {
                parents.insert(node.id, parent);
                children.entry(parent).or_default().push(node.id);
            }
            children.entry(node.id).or_default();
        }
        for descendants in children.values_mut() {
            descendants.sort_unstable();
        }
        break_parent_cycles(&ids, &mut parents, &mut children);
        let mut roots = ids
            .iter()
            .copied()
            .filter(|id| !parents.contains_key(id))
            .collect::<Vec<_>>();
        roots.sort_unstable();

        let mut measured = HashMap::new();
        let mut visiting = HashSet::new();
        for &root in &roots {
            measure_subtree(root, &children, &mut measured, &mut visiting);
        }
        for node in nodes {
            if !measured.contains_key(&node.id) {
                roots.push(node.id);
                parents.remove(&node.id);
                measure_subtree(node.id, &children, &mut measured, &mut visiting);
            }
        }

        let forest_width = roots
            .iter()
            .map(|id| measured.get(id).copied().unwrap_or(NODE_WIDTH))
            .sum::<i32>()
            + HORIZONTAL_GAP * i32::try_from(roots.len().saturating_sub(1)).unwrap_or(i32::MAX);
        let mut left = -forest_width / 2;
        let mut positions = HashMap::new();
        let mut placed = HashSet::new();
        for &root in &roots {
            let width = measured.get(&root).copied().unwrap_or(NODE_WIDTH);
            place_subtree(
                root,
                left,
                0,
                &children,
                &measured,
                &mut positions,
                &mut placed,
            );
            left += width + HORIZONTAL_GAP;
        }

        let bounds = positions
            .values()
            .fold(None::<LayoutBounds>, |bounds, position| {
                let left = position.center_x - NODE_WIDTH / 2;
                let right = left + NODE_WIDTH;
                let bottom = position.top + NODE_HEIGHT;
                Some(match bounds {
                    None => LayoutBounds {
                        left,
                        top: position.top,
                        right,
                        bottom,
                    },
                    Some(bounds) => LayoutBounds {
                        left: bounds.left.min(left),
                        top: bounds.top.min(position.top),
                        right: bounds.right.max(right),
                        bottom: bounds.bottom.max(bottom),
                    },
                })
            });
        let mut order = Vec::with_capacity(positions.len());
        let mut stack = roots.iter().rev().map(|id| (*id, 0)).collect::<Vec<_>>();
        let mut visited = HashSet::new();
        while let Some((id, depth)) = stack.pop() {
            if !visited.insert(id) {
                continue;
            }
            order.push((id, depth));
            stack.extend(
                children
                    .get(&id)
                    .into_iter()
                    .flatten()
                    .rev()
                    .map(|child| (*child, depth + 1)),
            );
        }
        Self {
            positions,
            parents,
            children,
            roots,
            order,
            bounds,
        }
    }

    pub(super) fn overview_center(&self, width: u16, height: u16) -> Option<WorldPoint> {
        let bounds = self.bounds.as_ref()?;
        let forest_width = bounds.right - bounds.left;
        let forest_height = bounds.bottom - bounds.top;
        if forest_width > i32::from(width) || forest_height > i32::from(height) {
            return None;
        }
        Some(WorldPoint {
            x: f64::from(bounds.left + forest_width / 2),
            y: f64::from(bounds.top + forest_height / 2),
        })
    }

    pub(super) fn ordered_nodes(&self) -> &[(AgentId, usize)] {
        &self.order
    }

    pub(super) fn position(&self, id: AgentId) -> Option<NodePosition> {
        self.positions.get(&id).copied()
    }

    pub(super) fn center(&self, id: AgentId) -> Option<WorldPoint> {
        self.position(id).map(|position| WorldPoint {
            x: f64::from(position.center_x),
            y: f64::from(position.top + NODE_HEIGHT / 2),
        })
    }

    pub(super) fn parent(&self, id: AgentId) -> Option<AgentId> {
        self.parents.get(&id).copied()
    }

    pub(super) fn children(&self, id: AgentId) -> &[AgentId] {
        self.children.get(&id).map_or(&[], Vec::as_slice)
    }

    pub(super) fn roots(&self) -> &[AgentId] {
        &self.roots
    }

    pub(super) fn positioned_nodes(&self) -> impl Iterator<Item = (AgentId, NodePosition)> + '_ {
        self.positions.iter().map(|(&id, &position)| (id, position))
    }
}

fn break_parent_cycles(
    ids: &HashSet<AgentId>,
    parents: &mut HashMap<AgentId, AgentId>,
    children: &mut HashMap<AgentId, Vec<AgentId>>,
) {
    let mut ordered = ids.iter().copied().collect::<Vec<_>>();
    ordered.sort_unstable();
    for start in ordered {
        let mut seen = HashSet::new();
        let mut current = start;
        seen.insert(current);
        while let Some(parent) = parents.get(&current).copied() {
            if !seen.insert(parent) {
                parents.remove(&current);
                if let Some(siblings) = children.get_mut(&parent) {
                    siblings.retain(|&child| child != current);
                }
                break;
            }
            current = parent;
        }
    }
}

fn measure_subtree(
    id: AgentId,
    children: &HashMap<AgentId, Vec<AgentId>>,
    measured: &mut HashMap<AgentId, i32>,
    visiting: &mut HashSet<AgentId>,
) -> i32 {
    if let Some(&width) = measured.get(&id) {
        return width;
    }
    if !visiting.insert(id) {
        return NODE_WIDTH;
    }

    let descendants = children.get(&id).map_or(&[][..], Vec::as_slice);
    let children_width = descendants
        .iter()
        .map(|&child| measure_subtree(child, children, measured, visiting))
        .sum::<i32>()
        + HORIZONTAL_GAP * i32::try_from(descendants.len().saturating_sub(1)).unwrap_or(i32::MAX);
    let width = NODE_WIDTH.max(children_width);
    visiting.remove(&id);
    measured.insert(id, width);
    width
}

#[allow(clippy::too_many_arguments)]
fn place_subtree(
    id: AgentId,
    left: i32,
    depth: i32,
    children: &HashMap<AgentId, Vec<AgentId>>,
    measured: &HashMap<AgentId, i32>,
    positions: &mut HashMap<AgentId, NodePosition>,
    placed: &mut HashSet<AgentId>,
) {
    if !placed.insert(id) {
        return;
    }
    let width = measured.get(&id).copied().unwrap_or(NODE_WIDTH);
    positions.insert(
        id,
        NodePosition {
            center_x: left + width / 2,
            top: depth * (NODE_HEIGHT + VERTICAL_GAP),
        },
    );

    let descendants = children.get(&id).map_or(&[][..], Vec::as_slice);
    let children_width = descendants
        .iter()
        .map(|child| measured.get(child).copied().unwrap_or(NODE_WIDTH))
        .sum::<i32>()
        + HORIZONTAL_GAP * i32::try_from(descendants.len().saturating_sub(1)).unwrap_or(i32::MAX);
    let mut child_left = left + (width - children_width) / 2;
    for &child in descendants {
        let child_width = measured.get(&child).copied().unwrap_or(NODE_WIDTH);
        place_subtree(
            child,
            child_left,
            depth + 1,
            children,
            measured,
            positions,
            placed,
        );
        child_left += child_width + HORIZONTAL_GAP;
    }
}

#[cfg(test)]
mod tests {
    use super::{HORIZONTAL_GAP, LayoutNode, NODE_WIDTH, TreeLayout};
    use orvek_subagents::AgentId;

    fn node(id: u64, parent: Option<u64>) -> LayoutNode {
        LayoutNode {
            id: AgentId::new(id),
            parent: parent.map(AgentId::new),
        }
    }

    #[test]
    fn parent_is_centered_over_an_even_set_of_children() {
        let layout = TreeLayout::new(&[
            node(1, None),
            node(2, Some(1)),
            node(3, Some(1)),
            node(4, Some(1)),
            node(5, Some(1)),
        ]);
        let parent = layout.position(AgentId::new(1)).unwrap();
        let first = layout.position(AgentId::new(2)).unwrap();
        let last = layout.position(AgentId::new(5)).unwrap();

        assert_eq!(parent.center_x * 2, first.center_x + last.center_x);
    }

    #[test]
    fn sibling_subtrees_have_an_even_minimum_gap() {
        let layout = TreeLayout::new(&[
            node(1, None),
            node(2, Some(1)),
            node(3, Some(1)),
            node(4, Some(2)),
            node(5, Some(2)),
            node(6, Some(3)),
        ]);
        let left_grandchild = layout.position(AgentId::new(5)).unwrap();
        let right_grandchild = layout.position(AgentId::new(6)).unwrap();

        assert!(
            right_grandchild.center_x - left_grandchild.center_x >= NODE_WIDTH + HORIZONTAL_GAP
        );
    }

    #[test]
    fn missing_parents_become_stable_roots() {
        let layout = TreeLayout::new(&[node(1, Some(99)), node(2, None)]);

        assert_eq!(layout.roots(), [AgentId::new(1), AgentId::new(2)]);
        assert!(layout.position(AgentId::new(1)).is_some());
        assert!(layout.position(AgentId::new(2)).is_some());
    }

    #[test]
    fn update_arrival_order_does_not_change_the_layout() {
        let ordered = [
            node(1, None),
            node(2, Some(1)),
            node(3, Some(1)),
            node(4, Some(2)),
        ];
        let shuffled = [ordered[3], ordered[2], ordered[0], ordered[1]];
        let ordered = TreeLayout::new(&ordered);
        let shuffled = TreeLayout::new(&shuffled);

        for id in 1..=4 {
            assert_eq!(
                ordered.position(AgentId::new(id)),
                shuffled.position(AgentId::new(id))
            );
        }
    }

    #[test]
    fn every_node_at_a_depth_shares_the_same_vertical_position() {
        let layout = TreeLayout::new(&[
            node(1, None),
            node(2, Some(1)),
            node(3, Some(1)),
            node(4, Some(2)),
            node(5, Some(3)),
        ]);

        assert_eq!(
            layout.position(AgentId::new(2)).unwrap().top,
            layout.position(AgentId::new(3)).unwrap().top
        );
        assert_eq!(
            layout.position(AgentId::new(4)).unwrap().top,
            layout.position(AgentId::new(5)).unwrap().top
        );
    }

    #[test]
    fn malformed_parent_cycles_are_broken_into_a_tree() {
        let layout = TreeLayout::new(&[node(1, Some(3)), node(2, Some(1)), node(3, Some(2))]);

        assert_eq!(layout.roots().len(), 1);
        assert_eq!(layout.positioned_nodes().count(), 3);
        let root = layout.roots()[0];
        assert!(layout.parent(root).is_none());
    }
    #[test]
    fn overview_uses_the_complete_forest_and_exact_terminal_cell_bounds() {
        let layout = TreeLayout::new(&[
            node(1, None),
            node(2, Some(1)),
            node(3, Some(1)),
            node(4, Some(1)),
        ]);
        assert_eq!(
            layout.overview_center(84, 13),
            Some(super::WorldPoint { x: 0.0, y: 6.0 })
        );
        assert!(layout.overview_center(83, 13).is_none());
        assert!(layout.overview_center(84, 12).is_none());
        assert!(TreeLayout::new(&[]).overview_center(100, 100).is_none());
    }

    #[test]
    fn compact_order_keeps_parents_before_children_after_cycle_and_orphan_repair() {
        let layout = TreeLayout::new(&[
            node(7, Some(99)),
            node(3, Some(2)),
            node(2, Some(1)),
            node(1, Some(3)),
            node(8, Some(7)),
        ]);
        let order = layout.ordered_nodes();
        assert_eq!(order.len(), 5);
        for (position, (id, depth)) in order.iter().enumerate() {
            if let Some(parent) = layout.parent(*id) {
                let (parent_position, (_, parent_depth)) = order
                    .iter()
                    .enumerate()
                    .find(|(_, (id, _))| *id == parent)
                    .unwrap();
                assert!(parent_position < position);
                assert_eq!(*depth, parent_depth + 1);
            } else {
                assert_eq!(*depth, 0);
            }
        }
        assert_eq!(
            order
                .iter()
                .map(|(id, _)| *id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            5
        );
    }
}
