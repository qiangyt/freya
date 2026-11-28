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

/// C27 第三枚钉（2026-11-28，桌面 C27 第二 facet）：**视口缩水与滚动同帧，
/// 且视口缩水源深于滚动内容矩形**。
///
/// 桌面聊天窗形态：Content::Flex 竖排列里 ScrollView 占 flex(1)，其下是
/// 定高的 hairline/composer；composer 深层节点长高（如审批横幅撑高输入区）
/// 使 ScrollView 实际视口从 440 缩到 327。同一帧滚动偏移也在变（滚轮），
/// 滚动内容矩形带上 InnerLayout 标脏。此时 find_best_root 按树高升序先
/// 处理内容矩形：它对父容器的提案走 closest_common_parent 的 Less 分支
/// （新提案更浅，不挂升级 walker），InnerLayout 存活；后处理的深层触发
/// 节点把候选提到列根并引发全树重测，但它的爬升路径在 composer 分支上，
/// 永不经过内容矩形。于是内容矩形在父链新鲜重测的帧里仍走「只平移不重测」
/// 快路（measure.rs 的 InnerLayout 分支不看 parent_is_dirty）：area 停在旧
/// 视口、Sized 事件不发，ScrollView 的滚轮钳制（shared.rs
/// get_scroll_position_from_wheel 读的 size 状态）从此用旧视口，停在真底
/// 上方（旧视口 - 新视口）px。桌面实证：钳制用 440 而实际已 327，差 113px，
/// 审批钮被排到可视底边之下、点击落进 composer 带。
///
/// 布局：500x500 视口，Content::Flex 竖排：header 32px、ScrollView
/// flex(1)、composer（内嵌三层盒，第 28px 的触发叶同帧点击长高到 141px，
/// 使 ScrollView 实际视口 440 → 327）。触发叶在树深 4，滚动内容矩形在树
/// 深 3——严格更深，保证 find_best_root 的升序遍历确定性先处理内容矩形。
/// 60 个 100px 条目，内容总高 6000。
#[test]
pub fn scroll_view_retracks_viewport_shrunk_in_the_same_frame_as_scrolling() {
    fn shrinking_viewport_app() -> impl IntoElement {
        let mut composer_tall = use_state(|| false);

        // flex 布局（桌面聊天窗形态）：header/composer 按内容占位，
        // ScrollView 用 flex(1) 拿剩余高度。
        rect()
            .content(Content::Flex)
            .child(rect().width(Size::fill()).height(Size::px(32.0)))
            .child(
                ScrollView::new()
                    .height(Size::flex(1.0))
                    .show_scrollbar(false)
                    .children((0..60).map(|_| {
                        rect()
                            .width(Size::fill())
                            .height(Size::px(100.0))
                            .into_element()
                    })),
            )
            // composer 分支：三层盒把触发叶压到树深 4（严格深于树深 3 的
            // 滚动内容矩形）。
            .child(
                rect().vertical().child(
                    rect().vertical().child(
                        rect().vertical().child(
                            rect()
                                .width(Size::fill())
                                // 同帧触发器：点击让这一叶从 28 长高到 141，
                                // ScrollView 的实际视口随之 440 → 327。
                                .height(if composer_tall() {
                                    Size::px(141.0)
                                } else {
                                    Size::px(28.0)
                                })
                                .on_pointer_down(move |_| composer_tall.set(true)),
                        ),
                    ),
                ),
            )
    }

    let mut test = launch_test(shrinking_viewport_app);
    let scrollview = test
        .find(|node, element| {
            Rect::try_downcast(element)
                .filter(|rect| rect.accessibility.builder.role() == AccessibilityRole::ScrollView)
                .map(move |_| node)
        })
        .unwrap();
    let content = scrollview.children().remove(0).children().remove(0);

    // 基线：视口 440，内容高 6000，未滚动。
    assert_eq!(content.layout().inner_sizes.height, 6000.0);
    assert_eq!(content.layout().area.height(), 440.0);
    assert_eq!(content.layout().offset_y.get(), 0.0);

    // 同帧「视口缩水 + 下滚」：按下（composer 触发叶长高）与滚轮两笔事件
    // 先排队，一次 sync_and_update 统一处理。滚动内容矩形在这一帧同时带上
    // InnerLayout 标脏（偏移变了）和被缩小的父链（视口变了）。
    test.send_event(PlatformEvent::Mouse {
        name: MouseEventName::MouseDown,
        cursor: (250.0, 480.0).into(),
        button: Some(MouseButton::Left),
    });
    test.send_event(PlatformEvent::Wheel {
        name: WheelEventName::Wheel,
        scroll: (0.0, -10000.0).into(),
        cursor: (250.0, 250.0).into(),
        source: WheelSource::Device,
    });
    test.sync_and_update();

    // 坏帧断言一（机理面）：实际视口已缩到 327（ScrollView 外层矩形新鲜），
    // 滚动内容矩形自己的 area 必须跟上，不能停在旧视口 440。
    assert_eq!(
        scrollview.layout().area.height(),
        327.0,
        "ScrollView 外层实际视口必须是 327（500 减 header 32 减 composer 141）"
    );
    assert_eq!(
        content.layout().area.height(),
        327.0,
        "同帧视口缩水后，滚动内容矩形的 area 必须跟上新视口（修前停在旧视口 440）"
    );

    // 坏帧断言二（行为面）：继续下滚到底，偏移必须等于内容总高减实际视口
    // （6000 - 327 = 5673），不能按旧视口钳在 6000 - 440 = 5560。
    test.send_event(PlatformEvent::Wheel {
        name: WheelEventName::Wheel,
        scroll: (0.0, -10000.0).into(),
        cursor: (250.0, 250.0).into(),
        source: WheelSource::Device,
    });
    test.sync_and_update();
    assert_eq!(
        content.layout().offset_y.get(),
        -5673.0,
        "滚到底的偏移必须按实际视口 327 钳制（修前按旧视口 440 停在 -5560）"
    );
    let last_bottom = content
        .children()
        .last()
        .unwrap()
        .layout()
        .area
        .to_f64()
        .max_y();
    assert!(
        last_bottom <= 359.0,
        "滚到底后最后一条的布局底边必须落在视口底（359）内（实测 {last_bottom}）"
    );
}

/// C27 第二枚钉（2026-11-28 桌面实证的必要面）：**静默期追加**——追加帧只含
/// ADD 变更（无同帧滚动），滚动位置处于旧范围末端。ADD 只把新节点标脏、不
/// 失效父链（修 C 之前）→ fill 高度的滚动内容矩形从缓存取尺寸、inner_sizes
/// 吞掉新条目高度 → 滚轮钳在旧范围（桌面实证：审批卡在视口外加入后钳在真
/// 底部上方约一个卡高，布局把卡片排在折叠之下、按钮点击落进 composer）。
/// 修 C = ADD 与 REMOVE 对称地失效父节点。
#[test]
pub fn scroll_view_reaches_true_bottom_for_content_appended_while_idle() {
    fn idle_append_app() -> impl IntoElement {
        let mut items = use_state(|| vec![200.0f32; 6]);

        rect()
            .child(
                rect()
                    .width(Size::fill())
                    .height(Size::px(20.0))
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

    let mut test = launch_test(idle_append_app);
    let scrollview = test
        .find(|node, element| {
            Rect::try_downcast(element)
                .filter(|rect| rect.accessibility.builder.role() == AccessibilityRole::ScrollView)
                .map(move |_| node)
        })
        .unwrap();
    let content = scrollview.children().remove(0).children().remove(0);

    // 基线：6 条 × 200 = 1200，视口 480，最大可达偏移 720。先下滚到旧范围
    // 末端（模拟桌面「读到历史中段/旧范围末端」的静默态）。
    test.scroll((250.0, 250.0), (0.0, -10000.0));
    test.sync_and_update();
    assert_eq!(content.layout().offset_y.get(), -720.0);

    // 静默期追加 1 条（追加帧只含 ADD 变更）：内容总高 1200 → 1400，真实
    // 最大偏移 720 → 920。
    test.send_event(PlatformEvent::Mouse {
        name: MouseEventName::MouseDown,
        cursor: (250.0, 10.0).into(),
        button: Some(MouseButton::Left),
    });
    test.sync_and_update();

    // 坏帧断言：追加帧结束时内容缓存必须已含新条目（修 C 前停在 1200）。
    assert_eq!(
        content.layout().inner_sizes.height,
        1400.0,
        "静默期追加帧结束时，内容累计高度必须等于全部条目总高"
    );

    // 收敛断言：持续下滚必须到达真实最大值 1120，最后一条布局底边落回视口。
    test.scroll((250.0, 250.0), (0.0, -10000.0));
    test.scroll((250.0, 250.0), (0.0, -10000.0));
    assert_eq!(
        content.layout().offset_y.get(),
        -920.0,
        "持续下滚必须到达真实最大值 1400 减视口 480"
    );
    let items = content.children();
    let last_bottom = items[6].layout().area.to_f64().max_y();
    assert!(
        last_bottom <= 500.0,
        "滚到底后最后一条的布局底边必须落在视口底内（实测 {last_bottom}）"
    );
}
