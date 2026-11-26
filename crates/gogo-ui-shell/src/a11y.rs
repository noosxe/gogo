//! The platform accessibility adapter's side of the shell (design 24 §5). The tree itself
//! comes from the application; until `gogo-ui-a11y` is wired in (#533), it is the window.

use accesskit::{Node, NodeId, Role, TreeId, TreeInfo, TreeUpdate};

/// A tree holding only the window.
pub(crate) fn window_only(title: &str) -> TreeUpdate {
    let root = NodeId(0);
    let mut window = Node::new(Role::Window);
    window.set_label(title.to_owned());
    TreeUpdate {
        nodes: vec![(root, window)],
        tree: Some(TreeInfo::new(root)),
        tree_id: TreeId::ROOT,
        focus: root,
    }
}

#[cfg(test)]
mod tests {
    use super::window_only;

    #[test]
    fn the_default_tree_names_the_window() {
        let update = window_only("A title");
        assert_eq!(update.nodes.len(), 1);
        assert_eq!(update.nodes[0].0, update.focus);
        assert_eq!(update.nodes[0].1.label(), Some("A title"));
    }
}
