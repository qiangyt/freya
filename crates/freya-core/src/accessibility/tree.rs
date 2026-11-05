use std::any::Any;

use accesskit::{
    Action,
    Affine,
    Node,
    Rect,
    Role,
    TreeId,
    TreeUpdate,
};
use ragnarok::ProcessedEvents;
use rustc_hash::{
    FxHashMap,
    FxHashSet,
};
use torin::prelude::{
    CursorPoint,
    LayoutNode,
};

use crate::{
    accessibility::{
        focus_strategy::AccessibilityFocusStrategy,
        focusable::Focusable,
        id::AccessibilityId,
    },
    data::Overflow,
    elements::{
        label::LabelElement,
        paragraph::ParagraphElement,
    },
    events::emittable::EmmitableEvent,
    integration::{
        EventName,
        EventsChunk,
    },
    node_id::NodeId,
    prelude::{
        AccessibilityFocusMovement,
        Color,
        EventType,
        FontSlant,
        TextAlign,
        TextDecoration,
        WheelEventData,
        WheelSource,
    },
    tree::Tree,
};

pub const ACCESSIBILITY_ROOT_ID: AccessibilityId = AccessibilityId(0);

pub struct AccessibilityTree {
    pub map: FxHashMap<AccessibilityId, NodeId>,
    // Current focused Accessibility Node.
    pub focused_id: AccessibilityId,
    /// The accesskit bounds each node was last emitted with (task-42). The
    /// sibling diff only marks nodes whose CONTENT changed, so a node merely
    /// SHIFTED by a sibling's size change never re-enters the diff on its
    /// own -- this cache is what lets `process_updates` detect the movers
    /// and re-emit them with fresh bounds. Without it the a11y tree serves
    /// yesterday's geometry and AT/UIA clicks land on the neighbour.
    pub bounds_cache: FxHashMap<NodeId, Rect>,
    /// When the bounds sync last ran. The host (freya-winit) guarantees a
    /// steady pass cadence (the PollRunner pumper), so a time-throttled scan
    /// is safe here: a skipped scan is re-run by a pass at most one interval
    /// later -- it is never lost.
    last_bounds_flush: Option<std::time::Instant>,
    /// Set by the host when a frame carried an actual tree change (a LAYOUT or
    /// ACCESSIBILITY mutation). Such a frame must synchronize the moved bounds
    /// IMMEDIATELY, not wait for the throttle: a layout-only change (e.g. a
    /// terminal tab's title widening and pushing its trailing close button
    /// right) produces NO accessibility-diff mark, so without this override the
    /// scan is throttled away and the moved node's stale bounds are served to
    /// AT/UIA until some later pass happens to re-scan -- which a structurally
    /// quiet UI never does (the putala-desktop terminal close-button defect:
    /// the a11y rect kept the narrow-tab position while the real layout had the
    /// close button ~580px to the right, so the UIA click landed on the tab
    /// body). Reset by the scan that consumes it.
    pub(crate) force_next_bounds_scan: bool,
}

/// The bounds-sync throttle window. Bounds only need "eventually correct"
/// (~150ms latency is imperceptible for AT pointing), while capping the
/// re-emission storm in the worst streaming frame.
const BOUNDS_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

/// The accesskit-space bounds a layout node emits (the single construction
/// point shared by `create_node` and the bounds-sync scan, so the cached
/// value and the emitted one can never drift apart).
fn layout_area_to_rect(layout_node: &LayoutNode) -> Rect {
    let area = layout_node.area.to_f64();
    Rect {
        x0: area.min_x(),
        x1: area.max_x(),
        y0: area.min_y(),
        y1: area.max_y(),
    }
}

impl Default for AccessibilityTree {
    fn default() -> Self {
        Self::new(ACCESSIBILITY_ROOT_ID)
    }
}

impl AccessibilityTree {
    pub fn new(focused_id: AccessibilityId) -> Self {
        Self {
            focused_id,
            map: FxHashMap::default(),
            bounds_cache: FxHashMap::default(),
            last_bounds_flush: None,
            force_next_bounds_scan: false,
        }
    }

    /// Arm the bounds-sync scan for the next pass: the host calls this when a
    /// frame carried an actual tree change (LAYOUT or ACCESSIBILITY mutation),
    /// so a layout-only mover (a node that SHIFTED without an accessibility-diff
    /// mark) is re-emitted with fresh bounds immediately instead of being
    /// throttled away. See the `force_next_bounds_scan` field.
    pub fn arm_bounds_scan(&mut self) {
        self.force_next_bounds_scan = true;
    }

    pub fn focused_node_id(&self) -> Option<NodeId> {
        self.map.get(&self.focused_id).cloned()
    }

    /// Initialize the Accessibility Tree
    pub fn init(&mut self, tree: &mut Tree) -> TreeUpdate {
        tree.accessibility_diff.clear();

        let mut nodes = vec![];

        tree.traverse_depth(|node_id| {
            let accessibility_state = tree.accessibility_state.get(&node_id).unwrap();
            let layout_node = tree.layout.get(&node_id).unwrap();
            let accessibility_node = Self::create_node(node_id, layout_node, tree);
            self.bounds_cache
                .insert(node_id, layout_area_to_rect(layout_node));
            nodes.push((accessibility_state.a11y_id, accessibility_node));
            self.map.insert(accessibility_state.a11y_id, node_id);
        });

        #[cfg(debug_assertions)]
        tracing::info!(
            "Initialized the Accessibility Tree with {} nodes.",
            nodes.len()
        );

        if !self.map.contains_key(&self.focused_id) {
            self.focused_id = ACCESSIBILITY_ROOT_ID;
        }

        TreeUpdate {
            tree_id: TreeId::ROOT,
            nodes,
            tree: Some(accesskit::Tree::new(ACCESSIBILITY_ROOT_ID)),
            focus: self.focused_id,
        }
    }

    /// Process any pending Accessibility Tree update
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn process_updates(
        &mut self,
        tree: &mut Tree,
        events_sender: &futures_channel::mpsc::UnboundedSender<EventsChunk>,
    ) -> TreeUpdate {
        self.process_updates_at(tree, events_sender, std::time::Instant::now())
    }

    fn process_updates_at(
        &mut self,
        tree: &mut Tree,
        events_sender: &futures_channel::mpsc::UnboundedSender<EventsChunk>,
        now: std::time::Instant,
    ) -> TreeUpdate {
        let requested_focus = tree.accessibility_diff.requested_focus.take();
        let removed_ids = tree
            .accessibility_diff
            .removed
            .drain()
            .collect::<FxHashMap<_, _>>();
        let mut added_or_updated_ids = tree
            .accessibility_diff
            .added_or_updated
            .drain()
            .collect::<FxHashSet<_>>();

        #[cfg(debug_assertions)]
        if !removed_ids.is_empty() || !added_or_updated_ids.is_empty() {
            tracing::info!(
                "Updating the Accessibility Tree with {} removals and {} additions/modifications",
                removed_ids.len(),
                added_or_updated_ids.len()
            );
        }

        // Remove all the removed nodes from the update list. A node that was
        // ALSO re-created in the same batch (removed from one parent and
        // added under another -- the session row collapsing between overview
        // sections, a dropdown closing and reopening) must STAY in the
        // update set: dropping it emits only the removal, the consumer drops
        // the node, and nothing ever re-adds it (the re-creation's marks were
        // swallowed here; the re-created node carries no listener-based
        // re-mark and its bounds-cache entry was wiped with the removal --
        // the putala-desktop 2026-10-27 active_overview red: a row present
        // at 0.2s vanished for good after the disconnect collapse). The
        // re-created node is recognized by its live accessibility state.
        for node_id in removed_ids.keys() {
            if tree.accessibility_state.contains_key(node_id) {
                continue;
            }
            added_or_updated_ids.remove(node_id);
            self.map.retain(|_, id| id != node_id);
            self.bounds_cache.remove(node_id);
        }

        // Mark the parent of the removed nodes as updated
        for parent_id in removed_ids.values() {
            if !removed_ids.contains_key(parent_id) {
                added_or_updated_ids.insert(*parent_id);
            }
        }

        // task-42 bounds sync (the putala-desktop 2026-10-27 proposal's
        // option A): the sibling diff only marks nodes whose CONTENT
        // changed, so a node merely SHIFTED by a sibling's size change
        // never re-enters the diff on its own. Compare every tracked node's
        // recorded bounds against the current layout and flush the movers --
        // otherwise the a11y tree keeps serving yesterday's geometry and
        // AT/UIA presses land on the neighbour row.
        //
        // Unconditional on purpose: ANY node kind can be a pointing or focus
        // target (rows/buttons via listeners, text inputs via focusability,
        // plain containers via position), and every filtering variant tried
        // on 2026-10-27 broke a consumer family (listeners-only broke the
        // whole paste family -- an editable carries no per-node listener and
        // its stale bounds made dialog pastes miss focus; the focusable
        // widening did not cure it). Cost: an O(cache) area compare per
        // armed pass, plus re-emission of the movers -- accepted as the
        // price of correct AT geometry.
        // Throttled: idle periodic scans may be deferred, but frames with actual
        // changes must synchronize bounds immediately because a click can arrive
        // before the next periodic scan. `force_next_bounds_scan` is the host's
        // "this frame carried a LAYOUT or ACCESSIBILITY mutation" signal: a
        // layout-only mover carries no diff mark (has_changes stays false), so
        // without it the throttle would swallow the mover on a quiet UI that
        // never re-scans (the terminal close-button defect).
        let has_changes = !removed_ids.is_empty() || !added_or_updated_ids.is_empty();
        let forced = std::mem::take(&mut self.force_next_bounds_scan);
        let scan_due = has_changes
            || forced
            || self
                .last_bounds_flush
                .is_none_or(|last| now.saturating_duration_since(last) >= BOUNDS_FLUSH_INTERVAL);
        if scan_due {
            self.last_bounds_flush = Some(now);
            for (node_id, cached_bounds) in self.bounds_cache.iter() {
                if added_or_updated_ids.contains(node_id) {
                    continue;
                }
                if let Some(layout_node) = tree.layout.get(node_id) {
                    let fresh_bounds = layout_area_to_rect(layout_node);
                    if fresh_bounds != *cached_bounds {
                        added_or_updated_ids.insert(*node_id);
                    }
                }
            }
        }

        // Drop the current mappings of every node that is about to be
        // re-registered before inserting their (possibly new) ids. A node
        // whose id was re-derived (see `AccessibilityState::update`) must
        // not keep its old id mapped: the stale entry made the focus
        // fallback below believe a dead focused id was still alive, and
        // `TreeUpdate::focus` then pointed accesskit at a removed node,
        // which makes accesskit_consumer panic ("Focused ID is not in the
        // node list").
        if !added_or_updated_ids.is_empty() {
            self.map
                .retain(|_, mapped_node_id| !added_or_updated_ids.contains(mapped_node_id));
        }

        // Register the created/updated nodes
        for node_id in added_or_updated_ids.clone() {
            let accessibility_state = tree.accessibility_state.get(&node_id).unwrap();
            self.map.insert(accessibility_state.a11y_id, node_id);

            let node_parent_id = tree.parents.get(&node_id).unwrap_or(&NodeId::ROOT);
            added_or_updated_ids.insert(*node_parent_id);
        }

        // task-42 subtree closure: a node whose ancestors were restructured
        // (sections collapsing, containers re-derived) can lose reachability
        // in the consumer between two updates -- the consumer prunes
        // unreachable nodes on every applied update, and a LATER children-list
        // reference to a pruned node is dropped by the lenient vendored
        // skip. Re-emitting the full subtree of every touched node makes each
        // update self-consistent: node bodies always accompany the children
        // lists that reference them.
        {
            let mut stack: Vec<NodeId> = added_or_updated_ids.iter().copied().collect();
            while let Some(node_id) = stack.pop() {
                if let Some(children) = tree.children.get(&node_id) {
                    for child in children.clone() {
                        if added_or_updated_ids.insert(child) {
                            stack.push(child);
                        }
                    }
                }
            }
            // Ancestor closure: the accesskit protocol requires every node's
            // parent to be "already in the tree or another node in this
            // update". Re-emitting restructured subtrees invalidates the
            // "already in the tree" half (the consumer pruned the old
            // structure in an earlier update), so without the ancestor chain
            // a re-emitted node can arrive as an orphan -- the consumer
            // panics on it ("neither in the tree nor a child of another node
            // from the update"), and freya-winit's panic hook turns that
            // into the blocking fatal-error dialog.
            for node_id in added_or_updated_ids.clone() {
                let mut current = node_id;
                while let Some(parent) = tree.parents.get(&current) {
                    let parent = *parent;
                    if parent == NodeId::ROOT || !added_or_updated_ids.insert(parent) {
                        break;
                    }
                    current = parent;
                }
            }
        }

        // Create the updated nodes
        let mut nodes = Vec::new();
        for node_id in added_or_updated_ids {
            let accessibility_state = tree.accessibility_state.get(&node_id).unwrap();
            let layout_node = tree.layout.get(&node_id).unwrap();
            let accessibility_node = Self::create_node(node_id, layout_node, tree);
            self.bounds_cache
                .insert(node_id, layout_area_to_rect(layout_node));
            nodes.push((accessibility_state.a11y_id, accessibility_node));
        }

        let has_request_focus = requested_focus.is_some();

        // Fallback the focused id to the root if the focused node no longer exists
        if !self.map.contains_key(&self.focused_id) {
            self.focused_id = ACCESSIBILITY_ROOT_ID;
        }

        // Focus the requested node id if there is one
        if let Some(requested_focus) = requested_focus {
            self.focus_node_with_strategy(requested_focus, tree);
        }

        if let Some(node_id) = self.focused_node_id()
            && has_request_focus
        {
            self.scroll_to(node_id, tree, events_sender);
        }

        TreeUpdate {
            tree_id: TreeId::ROOT,
            nodes,
            tree: Some(accesskit::Tree::new(ACCESSIBILITY_ROOT_ID)),
            focus: self.focused_id,
        }
    }

    /// Focus a Node given the strategy.
    pub fn focus_node_with_strategy(
        &mut self,
        strategy: AccessibilityFocusStrategy,
        tree: &mut Tree,
    ) {
        if let AccessibilityFocusStrategy::Node(id) = strategy {
            if self.map.contains_key(&id) {
                self.focused_id = id;
            }
            return;
        }

        let (navigable_nodes, focused_id) = if strategy.mode()
            == Some(AccessibilityFocusMovement::InsideGroup)
        {
            // Get all accessible nodes in the current group
            let mut group_nodes = Vec::new();

            let node_id = self.map.get(&self.focused_id).unwrap();
            let accessibility_state = tree.accessibility_state.get(node_id).unwrap();
            let member_accessibility_id = accessibility_state.a11y_member_of;
            if let Some(member_accessibility_id) = member_accessibility_id {
                group_nodes = tree
                    .accessibility_groups
                    .get(&member_accessibility_id)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|id| {
                        let node_id = self.map.get(id).unwrap();
                        let accessibility_state = tree.accessibility_state.get(node_id).unwrap();
                        accessibility_state.a11y_focusable == Focusable::Enabled
                    })
                    .collect();
            }
            (group_nodes, self.focused_id)
        } else {
            let mut nodes = Vec::new();

            tree.traverse_depth(|node_id| {
                let accessibility_state = tree.accessibility_state.get(&node_id).unwrap();
                let member_accessibility_id = accessibility_state.a11y_member_of;

                // Exclude nodes that are members of groups except for the parent of the group
                if let Some(member_accessibility_id) = member_accessibility_id
                    && member_accessibility_id != accessibility_state.a11y_id
                {
                    return;
                }
                if accessibility_state.a11y_focusable == Focusable::Enabled {
                    nodes.push(accessibility_state.a11y_id);
                }
            });

            (nodes, self.focused_id)
        };

        let node_index = navigable_nodes
            .iter()
            .position(|accessibility_id| *accessibility_id == focused_id);

        let target_node = match strategy {
            AccessibilityFocusStrategy::Forward(_) => {
                // Find the next Node
                if let Some(node_index) = node_index {
                    if node_index == navigable_nodes.len() - 1 {
                        navigable_nodes.first().cloned()
                    } else {
                        navigable_nodes.get(node_index + 1).cloned()
                    }
                } else {
                    navigable_nodes.first().cloned()
                }
            }
            AccessibilityFocusStrategy::Backward(_) => {
                // Find the previous Node
                if let Some(node_index) = node_index {
                    if node_index == 0 {
                        navigable_nodes.last().cloned()
                    } else {
                        navigable_nodes.get(node_index - 1).cloned()
                    }
                } else {
                    navigable_nodes.last().cloned()
                }
            }
            _ => unreachable!(),
        };

        self.focused_id = target_node.unwrap_or(focused_id);

        #[cfg(debug_assertions)]
        tracing::info!("Focused {:?} node.", self.focused_id);
    }

    /// Send the necessary wheel events to scroll views so that the given focused [NodeId] is visible on screen.
    fn scroll_to(
        &self,
        node_id: NodeId,
        tree: &mut Tree,
        events_sender: &futures_channel::mpsc::UnboundedSender<EventsChunk>,
    ) {
        let Some(effect_state) = tree.effect_state.get(&node_id) else {
            return;
        };
        let mut target_node = node_id;
        let mut emmitable_events = Vec::new();
        // Iterate over the inherited scrollables from the closes to the farthest
        for closest_scrollable in effect_state.scrollables.iter().rev() {
            // Every scrollable has a target node, the first scrollable target is the focused node that we want to make visible,
            // the rest scrollables will in the other hand just have the previous scrollable as target
            let target_layout_node = tree.layout.get(&target_node).unwrap();
            let target_area = target_layout_node.area;
            let scrollable_layout_node = tree.layout.get(closest_scrollable).unwrap();
            let scrollable_target_area = scrollable_layout_node.area;

            // We only want to scroll if it is not visible
            if !effect_state.is_visible(&tree.layout, &target_area) {
                let element = tree.elements.get(closest_scrollable).unwrap();
                let scroll_x = element
                    .accessibility()
                    .builder
                    .scroll_x()
                    .unwrap_or_default() as f32;
                let scroll_y = element
                    .accessibility()
                    .builder
                    .scroll_y()
                    .unwrap_or_default() as f32;

                // Get the relative diff from where the scrollable scroll starts
                let diff_x = target_area.min_x() - scrollable_target_area.min_x() - scroll_x;
                let diff_y = target_area.min_y() - scrollable_target_area.min_y() - scroll_y;

                // And get the distance it needs to scroll in order to make the target visible
                let delta_y = -(scroll_y + diff_y);
                let delta_x = -(scroll_x + diff_x);
                emmitable_events.push(EmmitableEvent {
                    name: EventName::Wheel,
                    source_event: EventName::Wheel,
                    node_id: *closest_scrollable,
                    data: EventType::Wheel(WheelEventData::new(
                        delta_x as f64,
                        delta_y as f64,
                        WheelSource::Custom,
                        CursorPoint::default(),
                        CursorPoint::default(),
                    )),
                    bubbles: false,
                });
                // Change the target to the current scrollable, so that the next scrollable makes sure this one is visible
                target_node = *closest_scrollable;
            }
        }
        events_sender
            .unbounded_send(EventsChunk::Processed(ProcessedEvents {
                emmitable_events,
                ..Default::default()
            }))
            .unwrap();
    }

    /// Create an accessibility node
    pub fn create_node(node_id: NodeId, layout_node: &LayoutNode, tree: &Tree) -> Node {
        let element = tree.elements.get(&node_id).unwrap();
        let mut accessibility_data = element.accessibility().into_owned();

        if node_id == NodeId::ROOT {
            accessibility_data.builder.set_role(Role::Window);
        }

        // Set children
        let children = tree
            .children
            .get(&node_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|child| tree.accessibility_state.get(&child).unwrap().a11y_id)
            .collect::<Vec<_>>();
        accessibility_data.builder.set_children(children);

        // Set the area
        accessibility_data
            .builder
            .set_bounds(layout_area_to_rect(layout_node));

        // Set inner text
        if let Some(children) = tree.children.get(&node_id) {
            for child in children {
                let child_element = tree.elements.get(child).unwrap().as_ref() as &dyn Any;
                if let Some(label) = child_element.downcast_ref::<LabelElement>() {
                    accessibility_data.builder.set_label(label.text.as_ref());
                } else if let Some(paragraph) = child_element.downcast_ref::<ParagraphElement>() {
                    accessibility_data.builder.set_label(
                        paragraph
                            .spans
                            .iter()
                            .map(|span| span.text.as_ref())
                            .collect::<String>(),
                    );
                }
            }
        }

        // Set focusable action
        // This will cause assistive technology to offer the user an option
        // to focus the current element if it supports it.
        if accessibility_data.a11y_focusable.is_enabled() {
            accessibility_data.builder.add_action(Action::Focus);
        }

        let builder = &mut accessibility_data.builder;

        if let Some(effect_state) = tree.effect_state.get(&node_id) {
            if let Some(rotation) = effect_state.rotation {
                let rotation = (rotation as f64).to_radians();
                let (sin, cos) = rotation.sin_cos();
                builder.set_transform(Affine::new([cos, sin, -sin, cos, 0.0, 0.0]));
            }

            if effect_state.overflow == Overflow::Clip {
                builder.set_clips_children();
            }
        }

        if let Some(background) = element.style().background.as_color() {
            builder.set_background_color(color_to_accesskit(background));
        }

        let element = element.as_ref() as &dyn Any;
        let is_text_element = element.is::<LabelElement>() || element.is::<ParagraphElement>();
        if !is_text_element {
            builder.set_is_line_breaking_object();
        }

        if let Some(text_style) = tree.text_style_state.get(&node_id) {
            if let Some(color) = text_style.color.as_color() {
                builder.set_foreground_color(color_to_accesskit(color));
            }

            builder.set_font_size(f32::from(text_style.font_size));
            builder.set_font_weight(f32::from(text_style.font_weight));
            builder.set_font_family(text_style.font_families.join(", "));

            if matches!(
                text_style.font_slant,
                FontSlant::Italic | FontSlant::Oblique
            ) {
                builder.set_italic();
            }

            builder.set_text_align(match text_style.text_align {
                TextAlign::Center => accesskit::TextAlign::Center,
                TextAlign::Justify => accesskit::TextAlign::Justify,
                // TODO: change the representation of `Start` and `End` once writing modes are supported.
                TextAlign::Left | TextAlign::Start => accesskit::TextAlign::Left,
                TextAlign::Right | TextAlign::End => accesskit::TextAlign::Right,
            });

            // TODO: adjust this once text directions other than left to right are supported.
            builder.set_text_direction(accesskit::TextDirection::LeftToRight);

            let decoration = accesskit::TextDecoration {
                style: accesskit::TextDecorationStyle::Solid,
                color: text_style
                    .color
                    .as_color()
                    .map(color_to_accesskit)
                    .unwrap_or(color_to_accesskit(Color::BLACK)),
            };
            match text_style.text_decoration {
                TextDecoration::Underline => builder.set_underline(decoration),
                TextDecoration::Overline => builder.set_overline(decoration),
                TextDecoration::LineThrough => builder.set_strikethrough(decoration),
                TextDecoration::None => {}
            }
        }

        accessibility_data.builder
    }
}

/// Convert a Freya [Color] into its [accesskit::Color] equivalent.
fn color_to_accesskit(color: Color) -> accesskit::Color {
    accesskit::Color {
        red: color.r(),
        green: color.g(),
        blue: color.b(),
        alpha: color.a(),
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use torin::prelude::{
        Area,
        Point2D,
        Size2D,
    };

    use super::*;
    use crate::{
        data::AccessibilityState,
        element::ElementExt,
        elements::rect::RectElement,
    };

    /// A rect carrying an explicit automation id.
    fn rect_with_id(id: AccessibilityId) -> Rc<dyn ElementExt> {
        let mut element = RectElement::default();
        element.accessibility.a11y_id = Some(id);
        Rc::new(element)
    }

    /// A plain rect with no explicit id (counter-id territory).
    fn rect_plain() -> Rc<dyn ElementExt> {
        Rc::new(RectElement::default())
    }

    /// A minimal tree: ROOT with a single child `node` bound to `element`,
    /// with accessibility states registered through the regular
    /// `AccessibilityState::create` path (so the diff is populated the same
    /// way the real renderer does).
    fn mini_tree(node: NodeId, element: &Rc<dyn ElementExt>) -> Tree {
        let mut tree = Tree::default();

        let root_element: Rc<dyn ElementExt> = Rc::new(RectElement::default());
        let root_state = AccessibilityState::create(
            NodeId::ROOT,
            &root_element,
            &mut tree.accessibility_diff,
            &tree.accessibility_generator,
            &mut tree.accessibility_groups,
        );
        tree.elements.insert(NodeId::ROOT, root_element);
        tree.accessibility_state.insert(NodeId::ROOT, root_state);
        tree.children.insert(NodeId::ROOT, vec![node]);
        tree.heights.insert(NodeId::ROOT, 0);
        tree.layout.cache_node(NodeId::ROOT, LayoutNode::default());

        let state = AccessibilityState::create(
            node,
            element,
            &mut tree.accessibility_diff,
            &tree.accessibility_generator,
            &mut tree.accessibility_groups,
        );
        tree.elements.insert(node, element.clone());
        tree.accessibility_state.insert(node, state);
        tree.parents.insert(node, NodeId::ROOT);
        tree.heights.insert(node, 1);
        tree.layout.cache_node(node, LayoutNode::default());

        tree
    }

    /// task-41 C1 regression: when a node's element loses its explicit
    /// automation id, `AccessibilityState::update` re-derives a counter id.
    /// Registering that update must drop the node's old id from the map --
    /// leaving it in place made the focus fallback believe a dead focused
    /// id was still alive, and `TreeUpdate::focus` then pointed accesskit
    /// at a removed node, which makes accesskit_consumer panic
    /// ("Focused ID is not in the node list").
    #[test]
    fn id_rederivation_drops_the_stale_mapping_and_the_stale_focus() {
        let node = NodeId::PLACEHOLDER;
        let explicit_id = AccessibilityId(0xDEAD_BEEF);

        let mut tree = mini_tree(node, &rect_with_id(explicit_id));
        let mut a11y_tree = AccessibilityTree::new(ACCESSIBILITY_ROOT_ID);
        let (events_sender, _events_receiver) = futures_channel::mpsc::unbounded();

        let update = a11y_tree.process_updates(&mut tree, &events_sender);
        assert_eq!(update.focus, ACCESSIBILITY_ROOT_ID);
        assert_eq!(a11y_tree.map.get(&explicit_id), Some(&node));

        // The node holds focus through its explicit id.
        a11y_tree.focused_id = explicit_id;

        // The element is swapped for one without an explicit id: the state
        // re-derives a new counter id and marks the node as updated.
        let plain = rect_plain();
        tree.accessibility_state.get_mut(&node).unwrap().update(
            node,
            &plain,
            &mut tree.accessibility_diff,
            &mut tree.accessibility_groups,
            &tree.accessibility_generator,
        );
        let rederived_id = tree.accessibility_state.get(&node).unwrap().a11y_id;
        assert_ne!(rederived_id, explicit_id);

        let update = a11y_tree.process_updates(&mut tree, &events_sender);

        // (a) The old id must no longer be mapped to the node: exactly one
        // mapping per node, pointing at the node's current id.
        assert!(
            !a11y_tree.map.contains_key(&explicit_id),
            "the re-derived id must replace the old mapping, not live beside it"
        );
        assert_eq!(a11y_tree.map.get(&rederived_id), Some(&node));

        // (b) Focus must no longer point at the dead id, and the emitted
        // focus must be an id that is actually registered in the map (the
        // exact condition accesskit_consumer panics on when violated).
        assert_ne!(a11y_tree.focused_id, explicit_id);
        assert!(
            a11y_tree.map.contains_key(&update.focus),
            "TreeUpdate::focus must point at a registered node"
        );
    }

    /// The everyday path must not churn: re-registering an updated node
    /// whose id did not change keeps the mapping intact (also for the
    /// focus, which stays on the unchanged id).
    #[test]
    fn unchanged_id_keeps_the_mapping_and_the_focus() {
        let node = NodeId::PLACEHOLDER;
        let explicit_id = AccessibilityId(0xAB_CD);

        let mut tree = mini_tree(node, &rect_with_id(explicit_id));
        let mut a11y_tree = AccessibilityTree::new(ACCESSIBILITY_ROOT_ID);
        let (events_sender, _events_receiver) = futures_channel::mpsc::unbounded();

        a11y_tree.process_updates(&mut tree, &events_sender);
        a11y_tree.focused_id = explicit_id;

        // Same element again: no id change, but the node is still marked as
        // updated (e.g. an attribute changed elsewhere).
        let tagged = rect_with_id(explicit_id);
        tree.accessibility_state.get_mut(&node).unwrap().update(
            node,
            &tagged,
            &mut tree.accessibility_diff,
            &mut tree.accessibility_groups,
            &tree.accessibility_generator,
        );
        tree.accessibility_diff.add_or_update(node);

        let update = a11y_tree.process_updates(&mut tree, &events_sender);

        assert_eq!(a11y_tree.map.get(&explicit_id), Some(&node));
        assert_eq!(a11y_tree.focused_id, explicit_id);
        assert_eq!(update.focus, explicit_id);
    }

    /// A tree with two stacked siblings: `a` occupies the top band, `b` the
    /// band right below it -- `b`'s area is derived from `a`'s height, the
    /// everyday sidebar-row arrangement.
    fn stacked_tree(a: NodeId, b: NodeId, area_a: Area, area_b: Area) -> Tree {
        let mut tree = Tree::default();

        let root_element: Rc<dyn ElementExt> = Rc::new(RectElement::default());
        let root_state = AccessibilityState::create(
            NodeId::ROOT,
            &root_element,
            &mut tree.accessibility_diff,
            &tree.accessibility_generator,
            &mut tree.accessibility_groups,
        );
        tree.elements.insert(NodeId::ROOT, root_element);
        tree.accessibility_state.insert(NodeId::ROOT, root_state);
        tree.children.insert(NodeId::ROOT, vec![a, b]);
        tree.heights.insert(NodeId::ROOT, 0);
        tree.layout.cache_node(NodeId::ROOT, LayoutNode::default());

        for (node, element, area) in [(a, &rect_plain(), area_a), (b, &rect_plain(), area_b)] {
            let state = AccessibilityState::create(
                node,
                element,
                &mut tree.accessibility_diff,
                &tree.accessibility_generator,
                &mut tree.accessibility_groups,
            );
            tree.elements.insert(node, element.clone());
            tree.accessibility_state.insert(node, state);
            tree.parents.insert(node, NodeId::ROOT);
            tree.heights.insert(node, 1);
            tree.layout.cache_node(
                node,
                LayoutNode {
                    area,
                    ..Default::default()
                },
            );
        }

        tree
    }

    /// task-42 (putala-desktop 2026-10-27 proposal, option A): a node shifted
    /// by its sibling's layout change carries no a11y diff mark -- B only
    /// moved because A grew. The emitted TreeUpdate must still carry B's
    /// FRESH bounds: the a11y tree serving stale geometry makes UIA/AT click
    /// at yesterday's position and hit the neighbour row.
    #[test]
    fn process_updates_flushes_shifted_bounds() {
        let a = NodeId::from(2);
        let b = NodeId::from(3);
        let mut tree = stacked_tree(
            a,
            b,
            Area::new(Point2D::new(0.0, 0.0), Size2D::new(100.0, 20.0)),
            Area::new(Point2D::new(0.0, 20.0), Size2D::new(100.0, 20.0)),
        );
        let mut a11y_tree = AccessibilityTree::new(ACCESSIBILITY_ROOT_ID);
        let (events_sender, _events_receiver) = futures_channel::mpsc::unbounded();

        a11y_tree.init(&mut tree);

        // A grows, B shifts down. No a11y diff marks at all.
        tree.layout.cache_node(
            a,
            LayoutNode {
                area: Area::new(Point2D::new(0.0, 0.0), Size2D::new(100.0, 40.0)),
                ..Default::default()
            },
        );
        tree.layout.cache_node(
            b,
            LayoutNode {
                area: Area::new(Point2D::new(0.0, 40.0), Size2D::new(100.0, 20.0)),
                ..Default::default()
            },
        );

        let update = a11y_tree.process_updates(&mut tree, &events_sender);

        let b_id = tree.accessibility_state.get(&b).unwrap().a11y_id;
        let b_node = update
            .nodes
            .iter()
            .find(|(id, _)| *id == b_id)
            .map(|(_, node)| node)
            .expect("the shifted node must be re-emitted with fresh bounds");
        let bounds = b_node.bounds().expect("bounds must be set");
        assert_eq!(
            bounds.y0, 40.0,
            "B 的边界必须反映推移后的新位置，不能停留在旧值"
        );
    }

    #[test]
    fn process_updates_flushes_moved_nodes_when_another_node_changes() {
        let a = NodeId::from(2);
        let b = NodeId::from(3);
        let parent_a = NodeId::from(4);
        let parent_b = NodeId::from(5);
        let mut tree = stacked_tree(
            a,
            b,
            Area::new(Point2D::new(0.0, 0.0), Size2D::new(100.0, 20.0)),
            Area::new(Point2D::new(0.0, 20.0), Size2D::new(100.0, 20.0)),
        );
        tree.children.insert(NodeId::ROOT, vec![parent_a, parent_b]);
        tree.children.insert(parent_a, vec![a]);
        tree.children.insert(parent_b, vec![b]);
        tree.parents.insert(a, parent_a);
        tree.parents.insert(b, parent_b);
        tree.heights.insert(a, 2);
        tree.heights.insert(b, 2);
        for (wrapper, area) in [
            (
                parent_a,
                Area::new(Point2D::new(0.0, 0.0), Size2D::new(100.0, 20.0)),
            ),
            (
                parent_b,
                Area::new(Point2D::new(0.0, 20.0), Size2D::new(100.0, 20.0)),
            ),
        ] {
            let element = rect_plain();
            let state = AccessibilityState::create(
                wrapper,
                &element,
                &mut tree.accessibility_diff,
                &tree.accessibility_generator,
                &mut tree.accessibility_groups,
            );
            tree.elements.insert(wrapper, element);
            tree.accessibility_state.insert(wrapper, state);
            tree.parents.insert(wrapper, NodeId::ROOT);
            tree.heights.insert(wrapper, 1);
            tree.layout.cache_node(
                wrapper,
                LayoutNode {
                    area,
                    ..Default::default()
                },
            );
        }

        let mut a11y_tree = AccessibilityTree::new(ACCESSIBILITY_ROOT_ID);
        let (events_sender, _events_receiver) = futures_channel::mpsc::unbounded();
        a11y_tree.init(&mut tree);
        let base_time = std::time::Instant::now();
        a11y_tree.process_updates_at(&mut tree, &events_sender, base_time);
        let b_id = tree.accessibility_state.get(&b).unwrap().a11y_id;
        assert_eq!(
            a11y_tree.bounds_cache.get(&b).map(|rect| rect.y0),
            Some(20.0)
        );

        tree.layout.cache_node(
            a,
            LayoutNode {
                area: Area::new(Point2D::new(0.0, 0.0), Size2D::new(100.0, 40.0)),
                ..Default::default()
            },
        );
        tree.layout.cache_node(
            b,
            LayoutNode {
                area: Area::new(Point2D::new(0.0, 40.0), Size2D::new(100.0, 20.0)),
                ..Default::default()
            },
        );
        let mut changed_element = RectElement::default();
        changed_element.accessibility.builder.set_label("changed");
        let changed: Rc<dyn ElementExt> = Rc::new(changed_element);
        tree.elements.insert(a, changed.clone());
        tree.accessibility_state.get_mut(&a).unwrap().update(
            a,
            &changed,
            &mut tree.accessibility_diff,
            &mut tree.accessibility_groups,
            &tree.accessibility_generator,
        );

        let update = a11y_tree.process_updates_at(
            &mut tree,
            &events_sender,
            base_time + std::time::Duration::from_millis(1),
        );
        let b_node = update
            .nodes
            .iter()
            .find(|(id, _)| *id == b_id)
            .map(|(_, node)| node)
            .expect("a content update must not leave a moved sibling at stale bounds");
        assert_eq!(b_node.bounds().expect("bounds must be set").y0, 40.0);
    }

    /// A group-member rect: an explicit automation id plus a group
    /// membership (the `member_of` face the group focus navigation reads).
    fn rect_with_id_in_group(id: AccessibilityId, group: AccessibilityId) -> Rc<dyn ElementExt> {
        let mut element = RectElement::default();
        element.accessibility.a11y_id = Some(id);
        element.accessibility.builder.set_member_of(group);
        Rc::new(element)
    }

    /// The cold-sibling tree: ROOT -> [a, b]. `a` is REGISTERED (state via
    /// create, carrying its group membership); `b` exists structurally but
    /// carries NO accessibility state yet -- the cold path the eviction arm
    /// serves (a re-created element whose state was never created).
    fn cold_sibling_tree(
        a: NodeId,
        a_element: &Rc<dyn ElementExt>,
        b: NodeId,
        b_element: &Rc<dyn ElementExt>,
    ) -> Tree {
        let mut tree = Tree::default();

        let root_element: Rc<dyn ElementExt> = Rc::new(RectElement::default());
        let root_state = AccessibilityState::create(
            NodeId::ROOT,
            &root_element,
            &mut tree.accessibility_diff,
            &tree.accessibility_generator,
            &mut tree.accessibility_groups,
        );
        tree.elements.insert(NodeId::ROOT, root_element);
        tree.accessibility_state.insert(NodeId::ROOT, root_state);
        tree.children.insert(NodeId::ROOT, vec![a, b]);
        tree.heights.insert(NodeId::ROOT, 0);
        tree.layout.cache_node(NodeId::ROOT, LayoutNode::default());

        let a_state = AccessibilityState::create(
            a,
            a_element,
            &mut tree.accessibility_diff,
            &tree.accessibility_generator,
            &mut tree.accessibility_groups,
        );
        tree.elements.insert(a, a_element.clone());
        tree.accessibility_state.insert(a, a_state);
        tree.parents.insert(a, NodeId::ROOT);
        tree.heights.insert(a, 1);
        tree.layout.cache_node(a, LayoutNode::default());

        tree.elements.insert(b, b_element.clone());
        let b_state = AccessibilityState::create(
            b,
            b_element,
            &mut tree.accessibility_diff,
            &tree.accessibility_generator,
            &mut tree.accessibility_groups,
        );
        tree.accessibility_state.insert(b, b_state);
        tree.parents.insert(b, NodeId::ROOT);
        tree.heights.insert(b, 1);
        tree.layout.cache_node(b, LayoutNode::default());

        tree
    }

    /// 2026-10-30 review M-1 regression: the eviction reassigns the stale
    /// holder's explicit id to a fresh counter id, but the group list kept
    /// the DEAD id -- group focus navigation unwraps `map.get(member)` per
    /// entry, so a dead member id panics there. The re-derived id must keep
    /// the membership.
    #[test]
    fn eviction_retires_the_stale_id_from_its_accessibility_group() {
        use std::collections::HashMap;

        use crate::{
            runner::{
                MutationModified,
                Mutations,
            },
            tree::DiffModifies,
        };

        let a = NodeId::from(2);
        let b = NodeId::from(3);
        let explicit_id = AccessibilityId(0xAB12_CD34);
        let group_id = AccessibilityId(0x77AA);

        let mut tree = cold_sibling_tree(
            a,
            &rect_with_id_in_group(explicit_id, group_id),
            b,
            &rect_plain(),
        );
        let mut a11y_tree = AccessibilityTree::new(ACCESSIBILITY_ROOT_ID);
        let (events_sender, _events_receiver) = futures_channel::mpsc::unbounded();
        let _ = a11y_tree.process_updates(&mut tree, &events_sender);

        assert_eq!(
            tree.accessibility_groups.get(&group_id).map(Vec::as_slice),
            Some(&[explicit_id][..]),
            "precondition: the member's explicit id is registered in its group"
        );

        // Enter the cold window: b's state is dropped (the structural node
        // outlives its state), so the next ACCESSIBILITY mutation on b takes
        // the creation arm the eviction serves.
        tree.accessibility_state.remove(&b);

        // b re-creates claiming the SAME explicit id -> the cold-path arm
        // evicts a's hold on it.
        tree.apply_mutations(Mutations {
            added: vec![],
            modified: vec![MutationModified {
                node_id: b,
                element: rect_with_id(explicit_id),
                flags: DiffModifies::ACCESSIBILITY,
            }],
            removed: vec![],
            moved: HashMap::default(),
        });
        let _ = a11y_tree.process_updates(&mut tree, &events_sender);

        let a_new_id = tree.accessibility_state.get(&a).unwrap().a11y_id;
        assert_ne!(
            a_new_id, explicit_id,
            "precondition: the eviction re-derived a's id"
        );
        let members = tree
            .accessibility_groups
            .get(&group_id)
            .cloned()
            .unwrap_or_default();
        assert!(
            !members.contains(&explicit_id),
            "the evicted id must leave its group list (a dead member id panics group focus navigation): {members:?}"
        );
        assert!(
            members.contains(&a_new_id),
            "the re-derived id keeps the membership: {members:?}"
        );
    }

    /// 2026-10-30 review M-2 regression: a re-created element claiming the
    /// RESERVED root id (0) must not evict the ROOT state -- the eviction
    /// would reassign ROOT's id while `TreeUpdate::tree` still declares 0 as
    /// the root, desyncing the protocol.
    #[test]
    fn eviction_never_reassigns_the_root_id() {
        use std::collections::HashMap;

        use crate::{
            runner::{
                MutationModified,
                Mutations,
            },
            tree::DiffModifies,
        };

        let a = NodeId::from(2);
        let b = NodeId::from(3);

        let mut tree = cold_sibling_tree(a, &rect_plain(), b, &rect_plain());

        tree.apply_mutations(Mutations {
            added: vec![],
            modified: vec![MutationModified {
                node_id: b,
                element: rect_with_id(ACCESSIBILITY_ROOT_ID),
                flags: DiffModifies::ACCESSIBILITY,
            }],
            removed: vec![],
            moved: HashMap::default(),
        });

        assert_eq!(
            tree.accessibility_state.get(&NodeId::ROOT).unwrap().a11y_id,
            ACCESSIBILITY_ROOT_ID,
            "a re-created element claiming the reserved root id must not reassign ROOT's id"
        );
    }

    /// task-42 gating half: a LAYOUT-only mutation (no ACCESSIBILITY flag)
    /// must still arm the accessibility update pass -- it is the pass where
    /// the shifted bounds get flushed; without it the scan never runs.
    #[test]
    fn layout_only_mutations_flag_accessibility() {
        use std::collections::HashMap;

        use crate::{
            runner::{
                MutationModified,
                Mutations,
            },
            tree::DiffModifies,
        };

        let node = NodeId::from(2);
        let mut tree = mini_tree(node, &rect_plain());

        let result = tree.apply_mutations(Mutations {
            added: vec![],
            modified: vec![MutationModified {
                node_id: node,
                element: rect_plain(),
                flags: DiffModifies::LAYOUT,
            }],
            removed: vec![],
            moved: HashMap::default(),
        });

        assert!(
            result.needs_accessibility,
            "LAYOUT-only mutations must arm the accessibility pass (the bounds flush rides it)"
        );
    }

    /// UI-scaling pre-work (2026-11-05): an inherited text-style change (a
    /// font-size change) moves text bounds the same way a LAYOUT change
    /// does -- `TextStyleState::update` already invalidates the layout and
    /// `needs_render` includes TEXT_STYLE -- so a TEXT_STYLE-only frame must
    /// arm the accessibility pass too. Without it the staleness is
    /// PERMANENT, not self-healing: after this frame is consumed a quiet UI
    /// produces no further change frame, and the 150ms pump can never
    /// schedule the pass (its tick is gated on needs_accessibility), so the
    /// bounds cache keeps serving the pre-change geometry forever.
    #[test]
    fn text_style_only_mutations_flag_accessibility() {
        use std::collections::HashMap;

        use crate::{
            runner::{
                MutationModified,
                Mutations,
            },
            tree::DiffModifies,
        };

        let node = NodeId::from(2);
        let mut tree = mini_tree(node, &rect_plain());

        let result = tree.apply_mutations(Mutations {
            added: vec![],
            modified: vec![MutationModified {
                node_id: node,
                element: rect_plain(),
                flags: DiffModifies::TEXT_STYLE,
            }],
            removed: vec![],
            moved: HashMap::default(),
        });

        assert!(
            result.needs_accessibility,
            "TEXT_STYLE-only mutations must arm the accessibility pass (a pure font-size \
             change moves text bounds; without the arm the bounds cache serves the \
             pre-change geometry forever -- the 150ms pump does not cover this gap)"
        );
    }

    /// UI-scaling pre-work (2026-11-05), consumer-visible half: a frame whose
    /// only mutation is TEXT_STYLE (inherited font-size change) re-measures
    /// A taller and pushes B down. Mirroring the renderer's arm-on-
    /// needs_accessibility wiring, the SAME frame must flush B's fresh
    /// bounds -- the defect shape (pre-fix) defers the flush forever because
    /// the frame never arms the scan and a quiet UI never produces the next
    /// change frame.
    #[test]
    fn text_style_frame_flushes_pushed_bounds_same_frame() {
        use std::collections::HashMap;

        use crate::{
            runner::{
                MutationModified,
                Mutations,
            },
            tree::DiffModifies,
        };

        let a = NodeId::from(2);
        let b = NodeId::from(3);
        let mut tree = stacked_tree(
            a,
            b,
            Area::new(Point2D::new(0.0, 0.0), Size2D::new(100.0, 20.0)),
            Area::new(Point2D::new(0.0, 20.0), Size2D::new(100.0, 20.0)),
        );
        let mut a11y_tree = AccessibilityTree::new(ACCESSIBILITY_ROOT_ID);
        let (events_sender, _events_receiver) = futures_channel::mpsc::unbounded();
        a11y_tree.init(&mut tree);

        let base = std::time::Instant::now();
        // First pass caches B's bounds at y0=20.
        a11y_tree.process_updates_at(&mut tree, &events_sender, base);
        let b_id = tree.accessibility_state.get(&b).unwrap().a11y_id;
        assert_eq!(
            a11y_tree.bounds_cache.get(&b).map(|rect| rect.y0),
            Some(20.0)
        );

        // The frame: A's inherited font size grows. The only mutation flag is
        // TEXT_STYLE (no LAYOUT, no ACCESSIBILITY) -- the shape a pure
        // font-scale change produces.
        let result = tree.apply_mutations(Mutations {
            added: vec![],
            modified: vec![MutationModified {
                node_id: a,
                element: rect_plain(),
                flags: DiffModifies::TEXT_STYLE,
            }],
            removed: vec![],
            moved: HashMap::default(),
        });

        // The renderer arms the scan exactly when apply_mutations flags the
        // pass (renderer.rs's needs_accessibility branch).
        if result.needs_accessibility {
            a11y_tree.arm_bounds_scan();
        }

        // The re-measure A's bigger font triggers: A grows, B shifts down.
        // (The render path measures layout BEFORE the accessibility pass, so
        // the fresh areas are already cached when the pass runs.)
        tree.layout.cache_node(
            a,
            LayoutNode {
                area: Area::new(Point2D::new(0.0, 0.0), Size2D::new(100.0, 40.0)),
                ..Default::default()
            },
        );
        tree.layout.cache_node(
            b,
            LayoutNode {
                area: Area::new(Point2D::new(0.0, 40.0), Size2D::new(100.0, 20.0)),
                ..Default::default()
            },
        );

        // The same frame's pass, inside the throttle window: B must be
        // re-emitted with the pushed-down bounds.
        let update = a11y_tree.process_updates_at(
            &mut tree,
            &events_sender,
            base + std::time::Duration::from_millis(10),
        );
        let b_bounds = update
            .nodes
            .iter()
            .find(|(id, _)| *id == b_id)
            .map(|(_, node)| node.bounds().expect("bounds must be set"))
            .unwrap_or_else(|| {
                panic!(
                    "a TEXT_STYLE-only frame must re-emit the pushed node with fresh bounds \
                     (stale bounds survive otherwise: {:#?})",
                    a11y_tree.bounds_cache.get(&b)
                )
            });
        assert_eq!(
            b_bounds.y0, 40.0,
            "the flushed bounds must reflect the pushed-down position (y0=40), not y0=20"
        );
        assert_eq!(
            a11y_tree.bounds_cache.get(&b).map(|rect| rect.y0),
            Some(40.0),
            "the bounds cache must be refreshed by the same frame's pass"
        );
    }

    /// The putala-desktop terminal close-button defect (2026-10-31): a LAYOUT-only
    /// mover -- a node that SHIFTED because a sibling grew, carrying NO
    /// accessibility-diff mark -- was throttled away and never re-emitted, so the
    /// a11y tree kept serving yesterday's geometry (the tab's trailing close
    /// button stayed at the narrow-tab position while the real layout had it
    /// ~580px right; the UIA click landed on the tab body). The fix arms the scan
    /// (`arm_bounds_scan`) when a frame carries a LAYOUT or ACCESSIBILITY
    /// mutation, so the mover is flushed even inside the throttle window. This
    /// test pins BOTH halves: un-armed inside the window the mover is deferred
    /// (the defect shape), armed it is re-emitted with fresh bounds.
    #[test]
    fn armed_scan_flushes_a_layout_only_mover_inside_the_throttle_window() {
        let a = NodeId::from(2);
        let b = NodeId::from(3);
        let mut tree = stacked_tree(
            a,
            b,
            Area::new(Point2D::new(0.0, 0.0), Size2D::new(100.0, 20.0)),
            Area::new(Point2D::new(0.0, 20.0), Size2D::new(100.0, 20.0)),
        );
        let mut a11y_tree = AccessibilityTree::new(ACCESSIBILITY_ROOT_ID);
        let (events_sender, _events_receiver) = futures_channel::mpsc::unbounded();
        a11y_tree.init(&mut tree);

        let base = std::time::Instant::now();
        // First pass: the initial scan runs (last_bounds_flush is None) and
        // records the flush time; B's bounds are cached at y0=20.
        a11y_tree.process_updates_at(&mut tree, &events_sender, base);
        let b_id = tree.accessibility_state.get(&b).unwrap().a11y_id;
        assert_eq!(
            a11y_tree.bounds_cache.get(&b).map(|rect| rect.y0),
            Some(20.0)
        );

        // A grows, B shifts down. NO accessibility-diff mark (a pure layout move),
        // exactly the shape a widening tab title produces for its close button.
        tree.layout.cache_node(
            a,
            LayoutNode {
                area: Area::new(Point2D::new(0.0, 0.0), Size2D::new(100.0, 40.0)),
                ..Default::default()
            },
        );
        tree.layout.cache_node(
            b,
            LayoutNode {
                area: Area::new(Point2D::new(0.0, 40.0), Size2D::new(100.0, 20.0)),
                ..Default::default()
            },
        );

        // Inside the throttle window (10ms < 150ms) and UN-armed: the scan is
        // deferred, so B is not re-emitted -- its stale bounds survive (defect).
        let update = a11y_tree.process_updates_at(
            &mut tree,
            &events_sender,
            base + std::time::Duration::from_millis(10),
        );
        assert!(
            update.nodes.iter().all(|(id, _)| *id != b_id),
            "un-armed inside the throttle window, a layout-only mover is deferred \
             (its stale bounds survive -- the defect shape)"
        );

        // The host arms the scan (the frame carried a LAYOUT mutation). The very
        // next pass, still inside the window, must re-emit B with fresh bounds.
        a11y_tree.arm_bounds_scan();
        let update = a11y_tree.process_updates_at(
            &mut tree,
            &events_sender,
            base + std::time::Duration::from_millis(20),
        );
        let b_bounds = update
            .nodes
            .iter()
            .find(|(id, _)| *id == b_id)
            .map(|(_, node)| node.bounds().expect("bounds must be set"))
            .expect("an armed pass must re-emit the layout-only mover with fresh bounds");
        assert_eq!(
            b_bounds.y0, 40.0,
            "the armed scan must flush B's moved bounds (y0=40), not the stale y0=20"
        );
    }
}

// P3-1 第五形态取证实验模块（2026-11-04 主会话审查通过，收编为机制契约钉；
// 修复面红绿测试在 freya-testing tests/a11y_resize_bounds_sync.rs）。
#[cfg(test)]
mod tree_forensics_tests;
