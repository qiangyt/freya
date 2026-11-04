use freya::prelude::*;
use freya_testing::prelude::*;

/// P3-1 第五形态修复（布局趟也 arm）的确定性红绿测试。
///
/// `TestingRunner::resize` 忠实镜像 freya-winit 的 `WindowEvent::Resized` ->
/// `RedrawRequested` 序列。断言：resize 调用返回时，被推移节点的新坐标已经
/// 落到 consumer（`emitted_bounds` 读的就是 consumer 手里的 bounds_cache）。
///
/// 修前形态（现场探针 putala-desktop-e2e tools/a11y_transient_probe.py 实测：
/// resize 后 consumer 对全部跟踪元素冻结，最长 ~12s 才被下一次 mutation 帧
/// 解救；headless E1/E4 钉出节流窗 140ms / 静默期无界滞后）：布局趟只
/// measure 不 arm 不排队扫描趟，consumer 对推移后几何一路陈旧。
#[test]
pub fn resize_emits_fresh_bounds_within_the_same_call() {
    fn two_band_app() -> impl IntoElement {
        rect()
            .expanded()
            .child(rect().width(Size::fill()).height(Size::percent(50.)))
            .child(rect().width(Size::fill()).height(Size::percent(50.)))
    }

    let mut test = TestingRunner::new(two_band_app, Size2D::new(300., 300.), |_| {}, 1.0).0;
    test.sync_and_update();

    // fixture 的两个半高叶子 rect（按纵向位置分 top/bottom）。
    let (top, bottom) = {
        let mut leaves = test.find_many(|node, element| {
            Rect::try_downcast(element)
                .filter(|_| node.children().is_empty())
                .map(|_| node)
        });
        assert_eq!(leaves.len(), 2, "fixture: 两个半高叶子 rect");
        leaves.sort_by(|a, b| {
            a.layout()
                .area
                .to_f64()
                .min_y()
                .partial_cmp(&b.layout().area.to_f64().min_y())
                .unwrap()
        });
        let bottom = leaves.pop().unwrap();
        let top = leaves.pop().unwrap();
        (top, bottom)
    };

    // 初始 300 高：top 占 y 0..150，bottom 占 y 150..300。
    assert_eq!(
        bottom.layout().area.to_f64().min_y(),
        150.0,
        "fixture 初始布局"
    );
    assert_eq!(
        test.emitted_bounds(&bottom).map(|rect| rect.y0),
        Some(150.0),
        "初始趟已把初始坐标发射给 consumer"
    );

    // 缩窗到 200 高：top 收缩到 y 0..100，bottom 被推移到 y 100..200。
    test.resize(Size2D::new(300., 200.));

    assert_eq!(
        bottom.layout().area.to_f64().min_y(),
        100.0,
        "像素侧已重排版"
    );
    assert_eq!(
        test.emitted_bounds(&bottom).map(|rect| rect.y0),
        Some(100.0),
        "resize 同一调用内 consumer 已持新坐标（修前这里停留 150.0）"
    );
    assert_eq!(
        test.emitted_bounds(&top).map(|rect| rect.y1),
        Some(100.0),
        "top 的下缘同步收缩"
    );
}
