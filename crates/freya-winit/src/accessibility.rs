// `AccessibilityTask` lives in freya-core (next to the shared
// layout-only-change decision, `on_layout_only_change`); this module
// re-exports it so the crate-internal `use crate::accessibility::...`
// paths keep working.
pub use freya_core::accessibility::task::{
    AccessibilityTask,
    on_layout_only_change,
};
