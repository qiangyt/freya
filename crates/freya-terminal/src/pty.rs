use std::{
    cell::RefCell,
    rc::Rc,
};

use alacritty_terminal::{
    event::{
        Event as AlacrittyEvent,
        EventListener,
    },
    grid::Dimensions,
};
use freya_core::notify::ArcNotify;

use crate::handle::SharedBackend;

/// `Dimensions` impl passed to `Term::new` / `Term::resize`.
#[derive(Clone, Copy)]
pub(crate) struct TermSize {
    pub screen_lines: usize,
    pub columns: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

/// Listener proxy passed into alacritty's `Term`. Routes its side-effects
/// (PtyWrite, Title, ClipboardStore) into the freya-side state.
#[derive(Clone)]
pub struct EventProxy {
    pub(crate) backend: SharedBackend,
    pub(crate) title: Rc<RefCell<Option<String>>>,
    pub(crate) title_notifier: ArcNotify,
    pub(crate) clipboard_content: Rc<RefCell<Option<String>>>,
    pub(crate) clipboard_notifier: ArcNotify,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: AlacrittyEvent) {
        match event {
            AlacrittyEvent::PtyWrite(text) => {
                if let Some(backend) = &mut *self.backend.borrow_mut() {
                    let _ = backend.write(text.as_bytes());
                }
            }
            AlacrittyEvent::Title(t) => {
                *self.title.borrow_mut() = Some(t);
                self.title_notifier.notify();
            }
            AlacrittyEvent::ResetTitle => {
                *self.title.borrow_mut() = None;
                self.title_notifier.notify();
            }
            AlacrittyEvent::ClipboardStore(_, text) => {
                *self.clipboard_content.borrow_mut() = Some(text);
                self.clipboard_notifier.notify();
            }
            // Bell, MouseCursorDirty, ChildExit, ColorRequest, etc.
            _ => {}
        }
    }
}
