use freya::prelude::*;
use freya_testing::prelude::*;

/// C27 回归钉子：ScrollView 同帧「追加条目 + 向下滚」的最终布局必须跟上新内容。
///
/// 缺陷机理（2026-11-28 修复，torin 两处）：同一帧里内容节点被滚动偏移的
/// InnerLayout 打脏时，内容变化经父链入口的打脏会被丢弃（修 A：脏原因升级），
/// 干净缓存节点重测内部子树后累计 inner_sizes 的变化也不通知不回写
/// （修 B：缓存分支补发通知并回写）。该帧结束时 ScrollView 读到的内容尺寸
/// 会停在旧值，滚动被钳在旧范围。
///
/// 取证说明（2026-11-28 红测核验）：在本测试的应用形态下，修前代码并不红，
/// 因为 torin 既存的根候选升级走线（propose_new_candidate 沿提案路径把
/// InnerLayout 升级为 None，torin.rs 的候选变更闭包）已把同帧的 InnerLayout
/// 升级掉；修 A 钉住的是候选不变时 check_dirty_dependants 入口吞打脏的窗口，
/// 该窗口由 torin/tests/other.rs 的单元红测钉住（check_dirty_entry_upgrades_
/// inner_layout_instead_of_returning 与 cached_node_remeasured_children_
/// notify_and_recache，修前实测红）。本测试作为同帧滚动+追加的用户可见行为
/// 回归钉子保留。
///
/// 布局：500x500 视口，顶部 20px 追加按钮，下方 ScrollView 占 480px 高，
/// 每个条目 200px 高。四轮后共 6 个条目，内容总高 1200，滚动可达最大值
/// 720（1200 减视口 480）。
#[test]
pub fn scroll_view_tracks_content_appended_in_the_same_frame_as_scrolling() {
    fn same_frame_scroll_app() -> impl IntoElement {
        let mut items = use_state(|| vec![200.0f32, 200.0]);

        rect()
            .child(
                rect()
                    .width(Size::fill())
                    .height(Size::px(20.0))
                    // 用 pointer_down 而非 press：press 需要跨事件块的
                    // MouseDown/MouseUp 配对状态，无法在同一帧排队触发。
                    .on_pointer_down(move |_| items.write().push(200.0))
                    .child("append"),
            )
            .child(
                ScrollView::new()
                    .show_scrollbar(false)
                    .children(items.read().iter().map(|height| {
                        rect()
                            .width(Size::fill())
                            .height(Size::px(*height))
                            .into_element()
                    })),
            )
    }

    let mut test = launch_test(same_frame_scroll_app);
    let scrollview = test
        .find(|node, element| {
            Rect::try_downcast(element)
                .filter(|rect| rect.accessibility.builder.role() == AccessibilityRole::ScrollView)
                .map(move |_| node)
        })
        .unwrap();
    let content = scrollview.children().remove(0).children().remove(0);

    // 基线：两个条目，内容高 400，未滚动。
    assert_eq!(content.layout().inner_sizes.height, 400.0);
    assert_eq!(content.layout().offset_y.get(), 0.0);

    // 四轮「同帧追加 + 下滚」：按下、滚轮两笔事件先排队，再一次
    // sync_and_update 统一处理，追加与滚动落在同一帧。
    for _ in 0..4 {
        test.send_event(PlatformEvent::Mouse {
            name: MouseEventName::MouseDown,
            cursor: (250.0, 10.0).into(),
            button: Some(MouseButton::Left),
        });
        test.send_event(PlatformEvent::Wheel {
            name: WheelEventName::Wheel,
            scroll: (0.0, -500.0).into(),
            cursor: (250.0, 250.0).into(),
            source: WheelSource::Device,
        });
        test.sync_and_update();
    }

    // 坏帧断言：最后一轮同帧追加+滚动结束时，内容缓存必须已跟上全部条目。
    let items = content.children();
    assert_eq!(items.len(), 6, "四轮追加后共 6 个条目");
    assert_eq!(
        content.layout().inner_sizes.height,
        1200.0,
        "同帧追加+滚动那一帧结束时，内容累计高度必须等于全部条目总高（修前停在上一轮新鲜测量的旧值）"
    );
    assert_eq!(
        content.layout().offset_y.get(),
        -520.0,
        "滚动位置已按当时已知的内容尺寸推进（下滚为负偏移）"
    );

    // 收敛断言：停止追加后持续下滚，滚动必须能到真实最大值 720，
    // 最后一条的布局底边落到视口底（500）内。
    test.scroll((250.0, 250.0), (0.0, -10000.0));
    test.scroll((250.0, 250.0), (0.0, -10000.0));
    assert_eq!(
        content.layout().offset_y.get(),
        -720.0,
        "停止追加后持续下滚必须到达真实最大值 1200 减视口 480"
    );
    let last_bottom = items[5].layout().area.to_f64().max_y();
    assert!(
        last_bottom <= 500.0,
        "滚到底后最后一条的布局底边必须落在视口底内（实测 {last_bottom}）"
    );
}
