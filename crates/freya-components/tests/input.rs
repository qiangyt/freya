use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    time::Duration,
};

use freya::prelude::*;
use freya_testing::prelude::*;

#[test]
pub fn input_test() {
    fn input_app() -> impl IntoElement {
        let value = use_state(String::new);

        rect()
            .spacing(6.)
            .child(Input::new(value).placeholder("Type your name"))
            .child(format!("Your name is {}", value.read()))
    }

    let mut test = launch_test(input_app);

    let placeholder = test.find(|_, element| {
        Paragraph::try_downcast(element)
            .filter(|paragraph| paragraph.spans.iter().any(|s| s.text == "Type your name"))
    });
    assert!(placeholder.is_some());
    let label = test.find(|_, element| {
        Label::try_downcast(element).filter(|label| label.text.as_ref() == "Your name is ")
    });
    assert!(label.is_some());

    // Focus
    test.click_cursor((15.0, 15.0));
    // Type
    test.write_text("Rust");

    let text = test.find(|_, element| {
        Paragraph::try_downcast(element)
            .filter(|paragraph| paragraph.spans.iter().any(|s| s.text == "Rust"))
    });
    assert!(text.is_some());
    let label = test.find(|_, element| {
        Label::try_downcast(element).filter(|label| label.text.as_ref() == "Your name is Rust")
    });
    assert!(label.is_some());
}

#[test]
pub fn input_password_mode_test() {
    fn password_app() -> impl IntoElement {
        let value = use_state(String::new);

        rect()
            .child(Input::new(value).mode(InputMode::new_password()))
            .child(format!("value={}", value.read()))
    }

    let mut test = launch_test(password_app);

    // Focus and type
    test.click_cursor((15.0, 15.0));
    test.write_text("secret");

    // The rendered paragraph should show masked characters
    let masked = test.find(|_, element| {
        Paragraph::try_downcast(element)
            .filter(|paragraph| paragraph.spans.iter().any(|s| s.text == "******"))
    });
    assert!(masked.is_some());

    // But the underlying state should hold the real value
    let label = test.find(|_, element| {
        Label::try_downcast(element).filter(|label| label.text.as_ref() == "value=secret")
    });
    assert!(label.is_some());
}

#[test]
pub fn input_validator_test() {
    fn validator_app() -> impl IntoElement {
        let value = use_state(String::new);

        rect()
            .child(
                Input::new(value).on_validate(move |validator: InputValidator| {
                    // Only allow numeric input
                    if !validator.text().chars().all(|c| c.is_ascii_digit()) {
                        validator.set_valid(false);
                    }
                }),
            )
            .child(format!("value={}", value.read()))
    }

    let mut test = launch_test(validator_app);

    // Focus
    test.click_cursor((15.0, 15.0));

    // Type invalid text (letters), should be rejected
    test.write_text("abc");

    let label = test.find(|_, element| {
        Label::try_downcast(element).filter(|label| label.text.as_ref() == "value=")
    });
    assert!(label.is_some());

    // Type valid text (digits), should be accepted
    test.write_text("123");

    let label = test.find(|_, element| {
        Label::try_downcast(element).filter(|label| label.text.as_ref() == "value=123")
    });
    assert!(label.is_some());
}

#[test]
pub fn input_submit_test() {
    fn submit_app() -> impl IntoElement {
        let value = use_state(String::new);
        let mut submitted = use_state(String::new);

        rect()
            .child(Input::new(value).on_submit(move |text: String| {
                submitted.set(text);
            }))
            .child(format!("submitted={}", submitted.read()))
    }

    let mut test = launch_test(submit_app);

    // Focus and type
    test.click_cursor((15.0, 15.0));
    test.write_text("hello");

    // Not yet submitted
    let label = test.find(|_, element| {
        Label::try_downcast(element).filter(|label| label.text.as_ref() == "submitted=")
    });
    assert!(label.is_some());

    // Press Enter to submit
    test.press_key(Key::Named(NamedKey::Enter));

    let label = test.find(|_, element| {
        Label::try_downcast(element).filter(|label| label.text.as_ref() == "submitted=hello")
    });
    assert!(label.is_some());
}

#[test]
pub fn input_disabled_test() {
    fn disabled_app() -> impl IntoElement {
        let value = use_state(String::new);

        rect()
            .child(Input::new(value).enabled(false))
            .child(format!("value={}", value.read()))
    }

    let mut test = launch_test(disabled_app);

    // Try to focus and type
    test.click_cursor((15.0, 15.0));
    test.write_text("hello");

    // Value should remain empty since input is disabled
    let label = test.find(|_, element| {
        Label::try_downcast(element).filter(|label| label.text.as_ref() == "value=")
    });
    assert!(label.is_some());
}

#[test]
pub fn input_escape_unfocus_test() {
    fn escape_app() -> impl IntoElement {
        let value = use_state(String::new);

        rect()
            .child(Input::new(value))
            .child(format!("value={}", value.read()))
    }

    let mut test = launch_test(escape_app);

    // Focus and type
    test.click_cursor((15.0, 15.0));
    test.write_text("hello");

    let label = test.find(|_, element| {
        Label::try_downcast(element).filter(|label| label.text.as_ref() == "value=hello")
    });
    assert!(label.is_some());

    // Press Escape to unfocus.
    test.press_key(Key::Named(NamedKey::Escape));
    test.sync_and_update();

    // Type more text, should not be captured since the input lost focus
    test.write_text("world");

    let label = test.find(|_, element| {
        Label::try_downcast(element).filter(|label| label.text.as_ref() == "value=hello")
    });
    assert!(label.is_some());
}

#[test]
pub fn input_shift_wheel_scroll_test() {
    fn scroll_app() -> impl IntoElement {
        let value = use_state(String::new);

        rect().child(Input::new(value).width(Size::px(150.)))
    }

    let mut test = launch_test(scroll_app);

    // Focus and fill with text wider than the input
    test.click_cursor((15.0, 15.0));
    test.write_text("this is a very long text that overflows the input width");

    let paragraph_x = |test: &TestingRunner| {
        test.find(|node, element| {
            Paragraph::try_downcast(element).map(|_| node.layout().area.min_x())
        })
        .unwrap()
    };
    let initial_x = paragraph_x(&test);

    // Hold Shift and wheel over the input to scroll it horizontally while focused
    test.send_event(PlatformEvent::Keyboard {
        name: KeyboardEventName::KeyDown,
        key: Key::Named(NamedKey::Shift),
        code: Code::ShiftLeft,
        modifiers: Modifiers::SHIFT,
    });
    test.sync_and_update();

    test.send_event(PlatformEvent::Wheel {
        name: WheelEventName::Wheel,
        scroll: (0.0, -50.0).into(),
        cursor: (75.0, 15.0).into(),
        source: WheelSource::Device,
    });
    test.sync_and_update();

    let scrolled_x = paragraph_x(&test);
    assert!(scrolled_x < initial_x);
}

#[test]
pub fn input_auto_focus_test() {
    fn auto_focus_app() -> impl IntoElement {
        let value = use_state(String::new);

        rect()
            .child(Input::new(value).auto_focus(true))
            .child(format!("value={}", value.read()))
    }

    let mut test = launch_test(auto_focus_app);

    // Type without clicking, auto_focus should have focused the input
    test.write_text("typed");

    let label = test.find(|_, element| {
        Label::try_downcast(element).filter(|label| label.text.as_ref() == "value=typed")
    });
    assert!(label.is_some());
}

// Shared app for the selection tests: an `Input` that appends every
// `on_selection_change` payload as `from:to;` into the given log.
macro_rules! selection_log_app {
    ($log:expr) => {{
        let log = $log.clone();
        move || {
            let value = use_state(String::new);
            let log = log.clone();
            rect().child(Input::new(value).on_selection_change(
                move |(from, to): (usize, usize)| {
                    log.borrow_mut().push_str(&format!("{from}:{to};"));
                },
            ))
        }
    }};
}

fn parse_selection_log(log: &str) -> Vec<(usize, usize)> {
    log.split(';')
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (from, to) = entry.split_once(':').unwrap();
            (from.parse().unwrap(), to.parse().unwrap())
        })
        .collect()
}

fn press_ctrl_a(test: &mut TestingRunner) {
    test.send_event(PlatformEvent::Keyboard {
        name: KeyboardEventName::KeyDown,
        key: Key::Character("a".to_string()),
        code: Code::KeyA,
        modifiers: Modifiers::CONTROL,
    });
    test.sync_and_update();
}

fn press_shift_arrow(test: &mut TestingRunner, key: NamedKey, code: Code) {
    test.send_event(PlatformEvent::Keyboard {
        name: KeyboardEventName::KeyDown,
        key: Key::Named(key),
        code,
        modifiers: Modifiers::SHIFT,
    });
    test.sync_and_update();
}

#[test]
pub fn input_selection_click_collapse_test() {
    let log = Rc::new(RefCell::new(String::new()));
    let mut test = launch_test(selection_log_app!(log));

    // Focus: the caret stays (0,0), same as the mount-time snapshot -> no notification.
    test.click_cursor((5.0, 15.0));
    test.write_text("hello"); // caret moves to the end -> "5:5;"
    press_ctrl_a(&mut test); // select all -> "0:5;"

    // Age out the multi-press combo window so the next click is a single press.
    test.poll_n(Duration::from_millis(550), 1);
    test.click_cursor((5.0, 15.0)); // single click collapses the selection -> "0:0;"

    // Age out again and click the same spot: nothing changes -> no notification.
    test.poll_n(Duration::from_millis(550), 1);
    test.click_cursor((5.0, 15.0));

    assert_eq!(log.borrow().as_str(), "5:5;0:5;0:0;");
}

#[test]
pub fn input_selection_shift_arrow_test() {
    let log = Rc::new(RefCell::new(String::new()));
    let mut test = launch_test(selection_log_app!(log));

    // Focus: no notification (caret (0,0) equals the mount-time snapshot).
    test.click_cursor((5.0, 15.0));
    test.write_text("hi"); // caret at the end -> "2:2;"

    test.press_key(Key::Named(NamedKey::ArrowLeft)); // -> "1:1;"
    test.press_key(Key::Named(NamedKey::ArrowLeft)); // -> "0:0;"

    press_shift_arrow(&mut test, NamedKey::ArrowRight, Code::ArrowRight); // -> "0:1;"
    press_shift_arrow(&mut test, NamedKey::ArrowRight, Code::ArrowRight); // -> "0:2;"

    assert_eq!(log.borrow().as_str(), "2:2;1:1;0:0;0:1;0:2;");
}

#[test]
pub fn input_selection_drag_test() {
    let log = Rc::new(RefCell::new(String::new()));
    let mut test = launch_test(selection_log_app!(log));

    // Deterministic glyph metrics for the pointer hit-testing below.
    test.set_fonts(HashMap::from_iter([(
        "NotoSans",
        include_bytes!("../../freya-edit/tests/NotoSans-Regular.ttf").as_slice(),
    )]));
    test.set_default_fonts(&["NotoSans".into()]);

    // Focus and type.
    test.click_cursor((5.0, 15.0)); // no notification
    test.write_text("Hello Rustaceans"); // caret at the end -> "16:16;"

    // Press, drag forward, release.
    test.press_cursor((35.0, 15.0)); // collapsed caret at the press point
    test.move_cursor((80.0, 15.0)); // forward range anchored at the press point
    test.release_cursor((80.0, 15.0)); // Release does not alter the selection

    let entries = parse_selection_log(&log.borrow());
    assert_eq!(entries[0], (16, 16));
    assert_eq!(entries[1].0, entries[1].1, "press collapses the caret");
    assert_eq!(entries[2].0, entries[1].0, "drag keeps the anchor");
    assert!(entries[2].1 > entries[2].0, "drag is forward: (from, to)");
}

#[test]
pub fn input_selection_select_all_test() {
    let log = Rc::new(RefCell::new(String::new()));
    let mut test = launch_test(selection_log_app!(log));

    // Focus: no notification (caret (0,0) equals the mount-time snapshot).
    test.click_cursor((5.0, 15.0));
    test.write_text("hello world"); // caret at the end -> "11:11;"

    press_ctrl_a(&mut test); // select all -> "0:11;"

    assert_eq!(log.borrow().as_str(), "11:11;0:11;");
}

#[test]
pub fn input_selection_debounce_test() {
    let log = Rc::new(RefCell::new(String::new()));
    let mut test = launch_test(selection_log_app!(log));

    // Focus: no notification (caret (0,0) equals the mount-time snapshot).
    test.click_cursor((5.0, 15.0));
    test.write_text("hi"); // -> "2:2;"

    // Collapse the caret with a click.
    test.poll_n(Duration::from_millis(550), 1);
    test.click_cursor((5.0, 15.0)); // -> "0:0;"

    // Age out and click the same spot again: no change -> no notification.
    test.poll_n(Duration::from_millis(550), 1);
    test.click_cursor((5.0, 15.0));

    // Moving to an already-visited position does not re-notify either.
    test.press_key(Key::Named(NamedKey::ArrowLeft)); // no movement -> none

    // Going back and forth over the same position notifies: the debounce only
    // suppresses *consecutive* duplicates, not revisited values.
    press_shift_arrow(&mut test, NamedKey::ArrowRight, Code::ArrowRight); // -> "0:1;"
    press_shift_arrow(&mut test, NamedKey::ArrowLeft, Code::ArrowLeft); // -> "0:0;"

    assert_eq!(log.borrow().as_str(), "2:2;0:0;0:1;0:0;");
}
