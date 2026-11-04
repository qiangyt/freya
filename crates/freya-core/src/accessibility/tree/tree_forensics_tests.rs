//! P3-1 质量收口第一阶段取证实验（2026-11-04，主会话审查通过后收编为机制契约钉）。
//!
//! 目的：把「freya 无障碍层布局推移回灌瞬态」（第五形态）从结果反推的定性钉成
//! 直接实证。全部实验使用 `process_updates_at` 注入确定性时钟（毫秒刻度），
//! 输出发射序列时间表（`cargo test -p freya-core forensics_ -- --nocapture`）。
//!
//! 这些实验钉的是机制契约（节流窗、arm 语义、泵不产生扫描趟），不是回归面；
//! 修复面的红绿测试见 freya-testing 的 `tests/a11y_resize_bounds_sync.rs`
//! （方向 X：布局趟也 arm，resize/缩放帧同帧补发）。
//!
//! 背景约定：`process_updates_at(now)` = 一次无障碍扫描趟（winit 侧只有
//! mutation 帧 / 焦点请求 / Init 会排队趟；150ms 泵只发 PollRunner，
//! 静默趟不排队趟——所以「趟是否存在」由宿主语义决定，「趟到了是否扫描」
//! 由 scan_due 判定决定）。`arm_bounds_scan()` = 宿主对本帧的强制扫描信号
//! （0adec5e8 起 mutation 帧带 LAYOUT/ACCESSIBILITY 旗标时打上）。
//!
//! 实验一览：
//! - E1 emission_lag_window：无 arm + 节流窗内，布局推移后坐标发射被推迟到
//!   节流窗界（150ms），期间 13 个趟全部空转，consumer 持陈旧边界。
//! - E2 armed_same_pass（对照）：同场景 arm 后同趟立即发射（复核 0adec5e8）。
//! - E3 mixed_generation_window：跨扫描批次构造 consumer 树两节点坐标分属
//!   两代布局的实测窗，并模拟 UIA consumer 侧命中测试的漂移。
//! - E4 quiet_ui_unbounded：静默 UI（无后续 mutation）下发射无界滞后——
//!   泵不产生扫描趟，发射只能等下一个 armed 趟。
use std::rc::Rc;

use accesskit::Rect;
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

/// A plain rect with no explicit id.
fn rect_plain() -> Rc<dyn ElementExt> {
    Rc::new(RectElement::default())
}

/// Two stacked siblings: `a` occupies the top band, `b` the band right below
/// it -- the everyday sidebar-row arrangement (same fixture as the `tests`
/// module in tree.rs).
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

/// One a11y pass at deterministic time `at_ms`. Returns the emitted
/// (a11y_id, Rect) pairs (the face the accesskit consumer applies).
fn run_pass(
    a11y_tree: &mut AccessibilityTree,
    tree: &mut Tree,
    events_sender: &futures_channel::mpsc::UnboundedSender<EventsChunk>,
    at_ms: u64,
) -> Vec<(AccessibilityId, Rect)> {
    let update = a11y_tree.process_updates_at(
        tree,
        events_sender,
        std::time::Instant::now() + std::time::Duration::from_millis(at_ms),
    );
    update
        .nodes
        .into_iter()
        .filter_map(|(id, node)| node.bounds().map(|bounds| (id, bounds)))
        .collect()
}

/// The bounds the CONSUMER currently holds for `node` (what a UIA
/// BoundingRectangle / ElementFromPoint query resolves against).
fn consumer_bounds(a11y_tree: &AccessibilityTree, node: NodeId) -> Option<Rect> {
    a11y_tree.bounds_cache.get(&node).copied()
}

fn rect_contains(rect: &Rect, x: f64, y: f64) -> bool {
    x >= rect.x0 && x <= rect.x1 && y >= rect.y0 && y <= rect.y1
}

/// E1: 布局推移回灌的发射滞后窗（无 arm、节流窗内）。
/// A 长高、B 被下推（真实布局引擎同一趟 measure 同时产出两者新坐标，
/// 但布局趟不 arm、时钟不推过 150ms）→ process_updates 一路空转，
/// 直到节流窗界才发射。记录「布局完成时刻 → 坐标发射时刻」的差。
#[test]
fn forensics_e1_emission_lag_window_for_an_unarmed_layout_shift() {
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
    let b_id = tree.accessibility_state.get(&b).unwrap().a11y_id;

    a11y_tree.init(&mut tree);
    println!("== E1 发射滞后窗（无 arm，节流窗 150ms）==");
    println!(
        "t=0ms    初始趟: emitted={:?} (G0 落册)",
        run_pass(&mut a11y_tree, &mut tree, &events_sender, 0).len()
    );

    // t=10ms: 布局趟完成（G1）——A 长高到 40，B 被推移到 y=40。
    // 不 arm（对应 winit 的 resize/缩放布局路径：只设重排版旗标）。
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
    println!("t=10ms   布局趟完成 G1（A: h20->40, B: y20->40），无 arm、无 diff 标记");

    // 之后每 10ms 一个趟（模拟 pump/mutation 驱动的趟流）。
    let mut lag_emitted_at = None;
    for t in (20..=200u64).step_by(10) {
        let emitted = run_pass(&mut a11y_tree, &mut tree, &events_sender, t);
        let b_emitted = emitted
            .iter()
            .find(|(id, _)| *id == b_id)
            .map(|(_, rect)| (rect.y0, rect.y1));
        let consumer_y0 = consumer_bounds(&a11y_tree, b).map(|rect| rect.y0);
        println!(
            "t={:>3}ms  趟: emitted={} 个; B consumer.y0={:?}{}",
            t,
            emitted.len(),
            consumer_y0,
            b_emitted
                .map(|(y0, _)| format!(" <-- B 发射 y0={}", y0))
                .unwrap_or_default()
        );
        if b_emitted.is_some() && lag_emitted_at.is_none() {
            lag_emitted_at = Some(t);
        }
    }

    let lag = lag_emitted_at.expect("B 的新坐标最终必须被发射") - 10;
    println!(
        ">> 实测：布局完成(t=10ms) → B 新坐标发射(t={}ms)，发射滞后窗 = {}ms，期间趟全部空转",
        lag_emitted_at.unwrap(),
        lag
    );
    assert_eq!(
        lag_emitted_at,
        Some(150),
        "节流窗界（距上次扫描 150ms）才补发"
    );
    assert_eq!(lag, 140);
    assert_eq!(
        consumer_bounds(&a11y_tree, b).map(|rect| rect.y0),
        Some(40.0),
        "发射后 consumer 持新坐标"
    );
}

/// E2（对照，复核 0adec5e8）: 同场景，布局趟 arm 后同趟立即全表发射。
#[test]
fn forensics_e2_armed_layout_pass_emits_in_the_same_pass() {
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
    let b_id = tree.accessibility_state.get(&b).unwrap().a11y_id;

    a11y_tree.init(&mut tree);
    println!("== E2 对照：布局趟 arm 后同趟发射（0adec5e8 复核）==");
    println!(
        "t=0ms    初始趟: emitted={:?} (G0 落册)",
        run_pass(&mut a11y_tree, &mut tree, &events_sender, 0).len()
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
    // winit 的 mutation 帧语义：needs_accessibility -> arm。
    a11y_tree.arm_bounds_scan();
    println!("t=10ms   布局趟完成 G1，arm_bounds_scan() 打上");

    let emitted = run_pass(&mut a11y_tree, &mut tree, &events_sender, 20);
    let b_emitted = emitted
        .iter()
        .find(|(id, _)| *id == b_id)
        .map(|(_, rect)| rect.y0);
    println!(
        "t=20ms   趟: emitted={} 个; B y0={:?}",
        emitted.len(),
        b_emitted
    );
    println!(">> 实测：arm 后发射滞后 = 10ms（一个趟内），B 以 y0=40 新坐标发射");
    assert_eq!(b_emitted, Some(40.0), "arm 后同趟立即发射推移结果");
}

/// E3: 跨扫描批次混合代际窗——consumer 树两节点坐标分属两代布局，
/// 且 consumer 侧命中测试在该窗内漂移（第五形态的点击误落机理）。
#[test]
fn forensics_e3_mixed_generation_consumer_window_and_hit_drift() {
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
    let b_id = tree.accessibility_state.get(&b).unwrap().a11y_id;

    a11y_tree.init(&mut tree);
    println!("== E3 跨扫描批次混合代际窗 + 命中漂移 ==");
    println!(
        "t=0ms    初始趟: emitted={:?} (G0: A@(0..100,0..20), B@(0..100,20..40))",
        run_pass(&mut a11y_tree, &mut tree, &events_sender, 0).len()
    );

    // G1: 只横移 A（x0 0->50），B 不动。无 arm。
    tree.layout.cache_node(
        a,
        LayoutNode {
            area: Area::new(Point2D::new(50.0, 0.0), Size2D::new(100.0, 20.0)),
            ..Default::default()
        },
    );
    println!("t=10ms   布局趟 G1：A 横移 x0=50，B 不动（无 arm）");

    let emitted = run_pass(&mut a11y_tree, &mut tree, &events_sender, 150);
    println!(
        "t=150ms  趟（节流窗界）: emitted {} 个 -> consumer: A@G1, B 仍 G0（对 B 而言 G0=现势）",
        emitted.len()
    );
    assert_eq!(
        consumer_bounds(&a11y_tree, a).map(|rect| rect.x0),
        Some(50.0)
    );
    assert_eq!(
        consumer_bounds(&a11y_tree, b).map(|rect| rect.y0),
        Some(20.0)
    );

    // G2: A 长高（y1 20->40），B 被推移到 y=40。无 arm（t=160）。
    tree.layout.cache_node(
        a,
        LayoutNode {
            area: Area::new(Point2D::new(50.0, 0.0), Size2D::new(100.0, 40.0)),
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
    println!("t=160ms  布局趟 G2：A 长高 h20->40，B 被推移 y20->40（无 arm）");

    // 混合代际窗内：consumer = A@G1（现势） + B@G0（落后一代）。
    let emitted_mid = run_pass(&mut a11y_tree, &mut tree, &events_sender, 200);
    let a_rect = consumer_bounds(&a11y_tree, a).expect("A 已在册");
    let b_rect = consumer_bounds(&a11y_tree, b).expect("B 已在册");
    let b_layout = tree.layout.get(&b).unwrap().area.to_f64();
    println!(
        "t=200ms  趟: emitted {} 个（空转）；consumer A=(x{:.0}..{:.0}, y{:.0}..{:.0}) [G1=现势], B=(x{:.0}..{:.0}, y{:.0}..{:.0}) [G0=陈旧]",
        emitted_mid.len(),
        a_rect.x0,
        a_rect.x1,
        a_rect.y0,
        a_rect.y1,
        b_rect.x0,
        b_rect.x1,
        b_rect.y0,
        b_rect.y1
    );
    println!(
        "         像素侧（tree.layout）B 实际已在 y{:.0}..{:.0}",
        b_layout.min_y(),
        b_layout.max_y()
    );
    assert_eq!(b_rect.y0, 20.0, "窗内 consumer 仍持 B 的 G0 坐标");
    assert_eq!(b_layout.min_y(), 40.0, "像素侧 B 已在 G2 位置");
    assert_eq!(a_rect.x0, 50.0, "窗内 consumer 的 A 坐标是现势（G1）");

    // consumer 侧命中测试漂移：UIA ElementFromPoint 以 consumer 树几何作答。
    // 点 (75, 30)：consumer 判给 B（B 的陈旧矩形包含它），像素侧该点在 A 的
    // 下半部（A 已长高覆盖 y0..40）——UIA 报告目标=B，物理落点=A。
    let probe = (75.0, 30.0);
    let consumer_hit_b = rect_contains(&b_rect, probe.0, probe.1);
    let a_pixel = tree.layout.get(&a).unwrap().area.to_f64();
    let pixel_hit_a = probe.0 >= a_pixel.min_x()
        && probe.0 <= a_pixel.max_x()
        && probe.1 >= a_pixel.min_y()
        && probe.1 <= a_pixel.max_y();
    println!(
        "         命中漂移：ElementFromPoint({}, {}) -> consumer 判 B={}；像素侧实际命中 A={}",
        probe.0, probe.1, consumer_hit_b, pixel_hit_a
    );
    assert!(
        consumer_hit_b && pixel_hit_a,
        "consumer 判 B 而像素在 A：点击闸门按 consumer 几何放行、物理落在被推移行"
    );

    // t=300ms 趟（节流窗界）补发 B@G2，窗关合。窗长 = 300-160 = 140ms。
    let emitted_close = run_pass(&mut a11y_tree, &mut tree, &events_sender, 300);
    let b_closed = consumer_bounds(&a11y_tree, b).map(|rect| rect.y0);
    println!(
        "t=300ms  趟（节流窗界）: emitted {} 个 -> B consumer.y0={:?}（窗关合，窗长 140ms）",
        emitted_close.len(),
        b_closed
    );
    assert_eq!(b_closed, Some(40.0));
    assert!(!emitted_close.is_empty(), "关合趟必须发射推移结果");
    let _ = b_id;
}

/// E4: 静默 UI 的无界滞后——布局推移后不再有任何趟（泵不产生扫描趟：
/// PollRunner 静默趟无 mutation 就不排队 accessibility 任务），直到
/// 990ms 后下一个 mutation 帧 armed 趟才发射。驳「等 150ms 泵即可」。
#[test]
fn forensics_e4_quiet_ui_emission_waits_for_the_next_armed_pass() {
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
    let b_id = tree.accessibility_state.get(&b).unwrap().a11y_id;

    a11y_tree.init(&mut tree);
    println!("== E4 静默 UI：发射只能等下一个 armed 趟（泵不扫描）==");
    println!(
        "t=0ms    初始趟: emitted={:?}",
        run_pass(&mut a11y_tree, &mut tree, &events_sender, 0).len()
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
    println!("t=10ms   布局趟完成 G1（A 长高、B 推移），无 arm；此后 UI 静默");

    // 150ms 泵照常打点（每 150ms 一个 PollRunner），但静默趟不排队
    // accessibility 任务 -> 不存在 process_updates 调用；此处以
    // 「不发趟」表达 winit 语义。consumer 陈旧边界贯穿整个静默期。
    for t in [160u64, 310, 460, 610, 760, 910] {
        let _pump_tick = t; // PollRunner 打点（无 mutation -> 无趟）
        assert_eq!(
            consumer_bounds(&a11y_tree, b).map(|rect| rect.y0),
            Some(20.0),
            "静默期内 consumer 一直持 B 的陈旧 y0=20（泵打点不起作用）"
        );
    }
    println!(
        "t=160..910ms 泵每 150ms 打点，但静默趟不排队任务 -> 无扫描、无发射，consumer 恒持 B y0=20"
    );

    // t=1000ms：终于来了一个 mutation 帧（A 内容变化）-> winit arm + 排队趟。
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
    a11y_tree.arm_bounds_scan();

    let emitted = run_pass(&mut a11y_tree, &mut tree, &events_sender, 1000);
    let b_emitted = emitted
        .iter()
        .find(|(id, _)| *id == b_id)
        .map(|(_, rect)| rect.y0);
    println!(
        "t=1000ms mutation 帧 armed 趟: emitted {} 个 -> B y0={:?}",
        emitted.len(),
        b_emitted
    );
    println!(">> 实测：布局完成(t=10) → 发射(t=1000)，滞后 990ms；泵打点 5 次无一发射");
    assert_eq!(b_emitted, Some(40.0), "armed 趟把推移结果一次补发");
}
