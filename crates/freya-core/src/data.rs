use std::{
    borrow::Cow,
    hash::Hash,
    ops::{
        Deref,
        DerefMut,
    },
    rc::Rc,
};

use torin::{
    prelude::Area,
    torin::Torin,
};

use crate::{
    accessibility::{
        dirty_nodes::AccessibilityDirtyNodes,
        focusable::Focusable,
        groups::AccessibilityGroups,
        id::{
            AccessibilityGenerator,
            AccessibilityId,
        },
        tree::ACCESSIBILITY_ROOT_ID,
    },
    element::ElementExt,
    layers::{
        Layer,
        Layers,
    },
    node_id::NodeId,
    prelude::{
        AccessibilityFocusStrategy,
        CursorStyle,
    },
    style::{
        border::Border,
        color::Color,
        corner_radius::CornerRadius,
        fill::Fill,
        font_size::FontSize,
        font_slant::FontSlant,
        font_weight::FontWeight,
        font_width::FontWidth,
        scale::Scale,
        shadow::Shadow,
        text_align::TextAlign,
        text_decoration::TextDecoration,
        text_height::TextHeightBehavior,
        text_overflow::TextOverflow,
        text_shadow::TextShadow,
        transform_origin::TransformOrigin,
    },
};

#[derive(Debug, Default, Clone, PartialEq)]
pub struct LayoutData {
    pub layout: torin::node::Node,
}

impl From<torin::node::Node> for LayoutData {
    fn from(layout: torin::node::Node) -> Self {
        LayoutData { layout }
    }
}

impl Deref for LayoutData {
    type Target = torin::node::Node;

    fn deref(&self) -> &Self::Target {
        &self.layout
    }
}

impl DerefMut for LayoutData {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.layout
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct EffectData {
    pub overflow: Overflow,
    pub rotation: Option<f32>,
    pub scale: Option<Scale>,
    pub transform_origin: TransformOrigin,
    pub opacity: Option<f32>,
    pub blur: Option<f32>,
    pub scrollable: bool,
    pub interactive: Interactive,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Default, Clone, PartialEq)]
pub struct StyleState {
    pub background: Fill,
    pub corner_radius: CornerRadius,
    pub borders: Vec<Border>,
    pub shadows: Vec<Shadow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CursorStyleData {
    pub color: Color,
    pub highlight_color: Color,
    pub style: CursorStyle,
}

impl Default for CursorStyleData {
    fn default() -> Self {
        Self {
            color: Color::BLACK,
            highlight_color: Color::from_rgb(87, 108, 188),
            style: CursorStyle::default(),
        }
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Hash)]
pub struct TextStyleState {
    pub font_size: FontSize,
    pub color: Fill,
    pub text_align: TextAlign,
    pub font_families: Vec<Cow<'static, str>>,
    pub text_height: TextHeightBehavior,
    pub text_overflow: TextOverflow,
    pub text_shadows: Vec<TextShadow>,
    pub text_decoration: TextDecoration,
    pub font_slant: FontSlant,
    pub font_weight: FontWeight,
    pub font_width: FontWidth,
}

impl Default for TextStyleState {
    fn default() -> Self {
        Self {
            font_size: FontSize::default(),
            color: Fill::Color(Color::BLACK),
            text_align: TextAlign::default(),
            font_families: Vec::new(),
            text_height: TextHeightBehavior::default(),
            text_overflow: TextOverflow::default(),
            text_shadows: Vec::new(),
            text_decoration: TextDecoration::default(),
            font_slant: FontSlant::default(),
            font_weight: FontWeight::default(),
            font_width: FontWidth::default(),
        }
    }
}

impl TextStyleState {
    pub fn from_data(parent: &TextStyleState, data: &TextStyleData) -> Self {
        let color = data.color.as_ref().unwrap_or(&parent.color).clone();

        let text_align = data.text_align.unwrap_or_default();
        let text_height = data.text_height.unwrap_or_default();
        let text_overflow = data.text_overflow.clone().unwrap_or_default();
        let text_shadows = data.text_shadows.clone();
        let text_decoration = data.text_decoration.unwrap_or_default();

        // Font values can be inherited
        let font_size = data.font_size.unwrap_or(parent.font_size);
        let font_slant = data.font_slant.unwrap_or(parent.font_slant);
        let font_weight = data.font_weight.unwrap_or(parent.font_weight);
        let font_width = data.font_width.unwrap_or(parent.font_width);
        let mut font_families = data.font_families.clone();
        font_families.extend_from_slice(&parent.font_families);

        Self {
            color,
            text_align,
            text_height,
            text_overflow,
            text_shadows,
            text_decoration,
            font_size,
            font_slant,
            font_weight,
            font_width,
            font_families,
        }
    }

    pub fn update(
        &mut self,
        node_id: NodeId,
        parent_text_style: &Self,
        element: &Rc<dyn ElementExt>,
        layout: &mut Torin<NodeId>,
    ) -> bool {
        let text_style_data = element.text_style();

        let text_style = Self::from_data(parent_text_style, &text_style_data);
        let is_equal = *self == text_style;

        *self = text_style;

        if !is_equal {
            // TODO: Only invalidate label and paragraphs
            layout.invalidate(node_id);
        }

        !is_equal
    }
}

#[derive(Debug, Clone, PartialEq, Default, Hash)]
pub struct TextStyleData {
    pub color: Option<Fill>,
    pub font_size: Option<FontSize>,
    pub font_families: Vec<Cow<'static, str>>,
    pub text_align: Option<TextAlign>,
    pub text_height: Option<TextHeightBehavior>,
    pub text_overflow: Option<TextOverflow>,
    pub text_shadows: Vec<TextShadow>,
    pub text_decoration: Option<TextDecoration>,
    pub font_slant: Option<FontSlant>,
    pub font_weight: Option<FontWeight>,
    pub font_width: Option<FontWidth>,
}

#[derive(Debug, Default)]
pub struct LayerState {
    pub layer: i16,
}

impl LayerState {
    pub fn create_for_root(node_id: NodeId, layers: &mut Layers) -> Self {
        let layer = 0;

        layers.insert_node_in_layer(node_id, layer);

        Self { layer }
    }

    pub fn remove(self, node_id: NodeId, layers: &mut Layers) {
        layers.remove_node_from_layer(&node_id, self.layer);
    }

    pub fn update(
        &mut self,
        parent_layer: &Self,
        node_id: NodeId,
        element: &Rc<dyn ElementExt>,
        layers: &mut Layers,
    ) {
        let relative_layer = element.layer();

        // Old
        layers.remove_node_from_layer(&node_id, self.layer);

        // New
        self.layer = match relative_layer {
            Layer::Relative(relative_layer) => parent_layer
                .layer
                .saturating_add(relative_layer)
                .saturating_add(1),
            Layer::Overlay => parent_layer.layer.saturating_add(i16::MAX / 16),
            Layer::OverlayLevel(overlay_level) => {
                (overlay_level.max(1) as i16).saturating_mul(i16::MAX / 16)
            }
        };
        layers.insert_node_in_layer(node_id, self.layer);
    }
}

/// Whether content overflowing an element's bounds is shown or clipped.
#[derive(Clone, Debug, PartialEq, Eq, Default, Copy)]
pub enum Overflow {
    /// Let children paint outside the element's bounds. This is the default.
    #[default]
    None,
    /// Clip children to the element's bounds.
    Clip,
}

/// Whether an element (and its descendants) responds to pointer events.
///
/// Converts from a `bool`, where `true` is [`Interactive::Yes`].
#[derive(Clone, Debug, PartialEq, Eq, Default, Copy)]
pub enum Interactive {
    /// The element receives pointer events. This is the default.
    #[default]
    Yes,
    /// The element ignores pointer events, letting them pass through.
    No,
}

impl From<bool> for Interactive {
    fn from(value: bool) -> Self {
        match value {
            true => Interactive::Yes,
            false => Interactive::No,
        }
    }
}

#[derive(PartialEq, Default, Debug, Clone)]
pub struct EffectState {
    pub overflow: Overflow,
    pub clips: Rc<[NodeId]>,

    pub rotations: Rc<[NodeId]>,
    pub rotation: Option<f32>,

    pub scales: Rc<[NodeId]>,
    pub scale: Option<Scale>,

    pub transform_origin: TransformOrigin,

    pub opacities: Rc<[f32]>,

    pub blur: Option<f32>,

    pub scrollables: Rc<[NodeId]>,

    pub interactive: Interactive,
}

impl EffectState {
    pub fn update(
        &mut self,
        parent_node_id: NodeId,
        parent_effect_state: &Self,
        node_id: NodeId,
        effect_data: Option<Cow<'_, EffectData>>,
        layer: Layer,
    ) {
        *self = Self {
            overflow: Overflow::default(),
            blur: None,
            rotation: None,
            scale: None,
            transform_origin: TransformOrigin::default(),
            ..parent_effect_state.clone()
        };

        match layer {
            Layer::Overlay => {
                self.clips = Rc::default();
            }
            Layer::Relative(_) if parent_effect_state.overflow == Overflow::Clip => {
                let mut clips = parent_effect_state.clips.to_vec();
                clips.push(parent_node_id);
                if self.clips.as_ref() != clips {
                    self.clips = Rc::from(clips);
                }
            }
            _ => {}
        }

        if let Some(effect_data) = effect_data {
            self.overflow = effect_data.overflow;
            self.blur = effect_data.blur;
            self.transform_origin = effect_data.transform_origin;

            if let Some(rotation) = effect_data.rotation {
                let mut rotations = parent_effect_state.rotations.to_vec();
                rotations.push(node_id);
                self.rotation = Some(rotation);
                if self.rotations.as_ref() != rotations {
                    self.rotations = Rc::from(rotations);
                }
            }

            if let Some(scale) = effect_data.scale {
                let mut scales = parent_effect_state.scales.to_vec();
                scales.push(node_id);
                self.scale = Some(scale);
                if self.scales.as_ref() != scales {
                    self.scales = Rc::from(scales);
                }
            }

            if let Some(opacity) = effect_data.opacity {
                let mut opacities = parent_effect_state.opacities.to_vec();
                opacities.push(opacity);
                if self.opacities.as_ref() != opacities {
                    self.opacities = Rc::from(opacities);
                }
            }

            if effect_data.scrollable {
                let mut scrolls = parent_effect_state.scrollables.to_vec();
                scrolls.push(node_id);
                if self.scrollables.as_ref() != scrolls {
                    self.scrollables = Rc::from(scrolls);
                }
            }

            if effect_data.interactive == Interactive::No {
                self.interactive = Interactive::No;
            }
        }
    }

    pub fn is_visible(&self, layout: &Torin<NodeId>, area: &Area) -> bool {
        // Skip elements that are completely out of any their parent's viewport
        for viewport_id in self.clips.iter() {
            let viewport = layout.get(viewport_id).unwrap().visible_area();
            if !viewport.intersects(area) {
                return false;
            }
        }
        true
    }
}

#[derive(PartialEq, Clone)]
pub struct AccessibilityState {
    pub a11y_id: AccessibilityId,
    pub a11y_focusable: Focusable,
    pub a11y_member_of: Option<AccessibilityId>,
    /// Whether the current `a11y_id` came from the element's explicit
    /// automation id (task-32, 2026-10-24). freya's sibling diff matches
    /// keyless children positionally, so inserting a conditional child
    /// reuses an existing node for a *different* element; when that new
    /// element carries no explicit automation id, the node must re-derive a
    /// counter id instead of keeping the previous element's id -- otherwise
    /// the stale id lingers beside the new element that now claims it, one
    /// accesskit TreeUpdate ends up listing the same NodeId under two
    /// parents, and accesskit_consumer panics ("TreeUpdate includes
    /// duplicate child"), which freya-winit's release panic hook turns into
    /// a blocking Fatal Error dialog.
    pub a11y_id_from_element: bool,
}

impl AccessibilityState {
    pub fn create(
        node_id: NodeId,
        element: &Rc<dyn ElementExt>,
        accessibility_diff: &mut AccessibilityDirtyNodes,
        accessibility_generator: &AccessibilityGenerator,
        accessibility_groups: &mut AccessibilityGroups,
    ) -> Self {
        let data = element.accessibility();

        let (a11y_id, a11y_id_from_element) = if node_id == NodeId::ROOT {
            (ACCESSIBILITY_ROOT_ID, false)
        } else {
            match data.a11y_id {
                Some(id) => (id, true),
                None => (AccessibilityId(accessibility_generator.new_id()), false),
            }
        };

        accessibility_diff.add_or_update(node_id);

        if let Some(member_of) = data.builder.member_of() {
            let group = accessibility_groups.entry(member_of).or_default();
            // This is not perfect as it assumes that order of creation is the same as the UI order
            // But we can't either assume that all the members are from the same parent so knowing their UI order gets trickier
            // So for no we just push to the end of the vector
            group.push(a11y_id);
        }

        if data.a11y_auto_focus {
            accessibility_diff.request_focus(AccessibilityFocusStrategy::Node(a11y_id));
        }

        Self {
            a11y_id,
            a11y_focusable: data.a11y_focusable.clone(),
            a11y_member_of: data.builder.member_of(),
            a11y_id_from_element,
        }
    }

    pub fn remove(
        self,
        node_id: NodeId,
        parent_id: NodeId,
        accessibility_diff: &mut AccessibilityDirtyNodes,
        accessibility_groups: &mut AccessibilityGroups,
    ) {
        accessibility_diff.remove(node_id, parent_id);

        if let Some(member_of) = self.a11y_member_of {
            let group = accessibility_groups.get_mut(&member_of).unwrap();
            group.retain(|id| *id != self.a11y_id);
        }
    }

    pub fn update(
        &mut self,
        node_id: NodeId,
        element: &Rc<dyn ElementExt>,
        accessibility_diff: &mut AccessibilityDirtyNodes,
        accessibility_groups: &mut AccessibilityGroups,
        accessibility_generator: &AccessibilityGenerator,
    ) {
        let data = element.accessibility();

        if let Some(member_of) = self.a11y_member_of
            && self.a11y_member_of != data.builder.member_of()
        {
            let group = accessibility_groups.get_mut(&member_of).unwrap();
            group.retain(|id| *id != self.a11y_id);
        }

        match data.a11y_id {
            Some(a11y_id) => {
                if self.a11y_id != a11y_id {
                    accessibility_diff.add_or_update(node_id);
                    self.a11y_id = a11y_id;
                }
                self.a11y_id_from_element = true;
            }
            // The element no longer carries an explicit automation id while
            // this node still holds one (task-32: the positional sibling diff
            // reused the node for a different element). Re-derive a counter
            // id: keeping the stale id would leave two nodes claiming one
            // accesskit NodeId in the same tree update, which
            // accesskit_consumer rejects with a panic.
            None if self.a11y_id_from_element && node_id != NodeId::ROOT => {
                self.a11y_id = AccessibilityId(accessibility_generator.new_id());
                self.a11y_id_from_element = false;
                accessibility_diff.add_or_update(node_id);
            }
            None => {}
        }

        if let Some(member_of) = data.builder.member_of() {
            let group = accessibility_groups.entry(member_of).or_default();
            // This is not perfect as it assumes that order of creation is the same as the UI order
            // But we can't either assume that all the members are from the same parent so knowing their UI order gets trickier
            // So for no we just push to the end of the vector
            group.push(self.a11y_id);

            self.a11y_member_of = Some(member_of);
        }

        self.a11y_focusable = data.a11y_focusable.clone();
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Default, Clone, PartialEq)]
pub struct AccessibilityData {
    pub a11y_id: Option<AccessibilityId>,
    pub a11y_auto_focus: bool,
    pub a11y_focusable: Focusable,
    pub builder: accesskit::Node,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elements::rect::RectElement;

    /// A rect carrying an explicit automation id (the `automation_id(...)`
    /// shape; the app's contract ids land here).
    fn rect_with_id(id: AccessibilityId) -> Rc<dyn ElementExt> {
        let mut element = RectElement::default();
        element.accessibility.a11y_id = Some(id);
        Rc::new(element)
    }

    /// A plain rect with no explicit id (counter-id territory).
    fn rect_plain() -> Rc<dyn ElementExt> {
        Rc::new(RectElement::default())
    }

    /// task-32 regression: freya's sibling diff matches keyless children
    /// positionally, so a conditional child insertion reuses an existing
    /// node for a different element. When the new element carries no
    /// explicit automation id, the node must re-derive a counter id --
    /// keeping the old element's id left two nodes claiming one accesskit
    /// NodeId in the same tree update, and accesskit_consumer panicked
    /// ("TreeUpdate includes duplicate child").
    #[test]
    fn update_rederives_the_id_when_the_element_loses_its_explicit_id() {
        let generator = AccessibilityGenerator::default();
        let mut diff = AccessibilityDirtyNodes::default();
        let mut groups = AccessibilityGroups::default();
        let node = NodeId::PLACEHOLDER;

        let tagged = rect_with_id(AccessibilityId(0xDEAD_BEEF));
        let mut state =
            AccessibilityState::create(node, &tagged, &mut diff, &generator, &mut groups);
        assert_eq!(state.a11y_id, AccessibilityId(0xDEAD_BEEF));
        assert!(state.a11y_id_from_element);

        let plain = rect_plain();
        state.update(node, &plain, &mut diff, &mut groups, &generator);
        assert_ne!(
            state.a11y_id,
            AccessibilityId(0xDEAD_BEEF),
            "a node reused for an id-less element must not keep the previous element's id"
        );
        assert!(!state.a11y_id_from_element);

        // The derived id is stable across later updates of the same element.
        let derived = state.a11y_id;
        state.update(node, &plain, &mut diff, &mut groups, &generator);
        assert_eq!(state.a11y_id, derived);
    }

    /// The everyday path must not churn: a stable explicit id survives
    /// frame after frame, and a *changed* explicit id is adopted.
    #[test]
    fn update_tracks_explicit_ids_without_churn() {
        let generator = AccessibilityGenerator::default();
        let mut diff = AccessibilityDirtyNodes::default();
        let mut groups = AccessibilityGroups::default();
        let node = NodeId::PLACEHOLDER;

        let tagged = rect_with_id(AccessibilityId(7));
        let mut state =
            AccessibilityState::create(node, &tagged, &mut diff, &generator, &mut groups);
        state.update(node, &tagged, &mut diff, &mut groups, &generator);
        assert_eq!(state.a11y_id, AccessibilityId(7));

        let retagged = rect_with_id(AccessibilityId(9));
        state.update(node, &retagged, &mut diff, &mut groups, &generator);
        assert_eq!(state.a11y_id, AccessibilityId(9));
        assert!(state.a11y_id_from_element);
    }

    /// The root node keeps its reserved id regardless of element swaps.
    #[test]
    fn root_keeps_its_reserved_id() {
        let generator = AccessibilityGenerator::default();
        let mut diff = AccessibilityDirtyNodes::default();
        let mut groups = AccessibilityGroups::default();

        let tagged = rect_with_id(AccessibilityId(7));
        let mut state =
            AccessibilityState::create(NodeId::ROOT, &tagged, &mut diff, &generator, &mut groups);
        assert_eq!(state.a11y_id, ACCESSIBILITY_ROOT_ID);

        state.update(
            NodeId::ROOT,
            &rect_plain(),
            &mut diff,
            &mut groups,
            &generator,
        );
        assert_eq!(state.a11y_id, ACCESSIBILITY_ROOT_ID);
    }
}
