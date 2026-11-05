//! The accessibility follow-up work a frame owes, and the shared decision
//! for layout-only changes (changes that re-layout without any mutation).

use std::{
    mem,
    ops::BitOrAssign,
};

use crate::{
    accessibility::tree::AccessibilityTree,
    platform::NavigationMode,
};

#[derive(Hash, PartialEq, Eq)]
pub enum AccessibilityTask {
    Init,
    ProcessUpdate { mode: Option<NavigationMode> },
    None,
}

impl AccessibilityTask {
    pub fn take(&mut self) -> Self {
        mem::replace(self, Self::None)
    }
}

impl BitOrAssign for AccessibilityTask {
    fn bitor_assign(&mut self, rhs: Self) {
        if self == &Self::None {
            *self = rhs
        }
    }
}

/// The accessibility follow-up a layout-only change owes: arm the bounds
/// scan (flush moved nodes this frame, bypassing the idle throttle once)
/// and queue a ProcessUpdate task so the redraw emits bounds read from
/// the current frame's layout.
///
/// A layout-only change (window resize, scale-factor/zoom change,
/// hotreload, an Android suspend/resume window rebuild) re-measures every
/// node but carries NO mutation, so nothing marks the accessibility diff
/// and the bounds-sync scan is throttled/deferred -- a quiet UI may never
/// produce the pass that re-emits the fresh geometry, leaving consumers
/// (screen readers, UIA) on stale coordinates. The host calls this on
/// every such path; this frame's RedrawRequested measures first, then
/// runs the queued pass, so emission reads the fresh coordinates.
///
/// The returned task merges into the host's pending-task slot with `|=`,
/// which keeps an already-queued task (e.g. `Init` from an
/// InitialTreeRequested) instead of overwriting it.
pub fn on_layout_only_change(accessibility: &mut AccessibilityTree) -> AccessibilityTask {
    accessibility.arm_bounds_scan();
    AccessibilityTask::ProcessUpdate { mode: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessibility::tree::ACCESSIBILITY_ROOT_ID;

    #[test]
    fn on_layout_only_change_arms_the_bounds_scan() {
        let mut accessibility = AccessibilityTree::new(ACCESSIBILITY_ROOT_ID);
        assert!(
            !accessibility.force_next_bounds_scan,
            "fixture: a fresh tree must start un-armed"
        );

        on_layout_only_change(&mut accessibility);

        assert!(
            accessibility.force_next_bounds_scan,
            "the follow-up must arm the bounds scan (the throttle bypass)"
        );
    }

    #[test]
    fn on_layout_only_change_returns_a_plain_process_update_task() {
        let mut accessibility = AccessibilityTree::new(ACCESSIBILITY_ROOT_ID);

        let task = on_layout_only_change(&mut accessibility);

        assert!(
            matches!(task, AccessibilityTask::ProcessUpdate { mode: None }),
            "the follow-up is a ProcessUpdate pass with no navigation-mode side effect"
        );
    }
}
