use rustc_hash::FxHashMap;
use torin::{
    prelude::*,
    test_utils::*,
};

#[test]
pub fn caching() {
    let (mut layout, mut measurer) = test_utils();

    let mut mocked_tree = TestingTree::default();
    mocked_tree.add(
        0,
        None,
        vec![1],
        Node::from_size_and_padding(
            Size::Pixels(Length::new(200.0)),
            Size::Pixels(Length::new(200.0)),
            Gaps::new(5.0, 0.0, 0.0, 0.0),
        ),
    );
    mocked_tree.add(
        1,
        Some(0),
        vec![],
        Node::from_size_and_direction(
            Size::Percentage(Length::new(100.0)),
            Size::Percentage(Length::new(100.0)),
            Direction::Vertical,
        ),
    );

    layout.measure(
        0,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(1000.0, 1000.0)),
        &mut measurer,
        &mut mocked_tree,
    );

    assert_eq!(
        layout.get(&1).unwrap().area,
        Rect::new(Point2D::new(0.0, 5.0), Size2D::new(200.0, 195.0)),
    );

    mocked_tree.set_node(
        1,
        Node::from_size_and_direction(
            Size::Percentage(Length::new(50.0)),
            Size::Percentage(Length::new(100.0)),
            Direction::Vertical,
        ),
    );
    layout.invalidate(1);

    layout.measure(
        0,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(1000.0, 1000.0)),
        &mut measurer,
        &mut mocked_tree,
    );

    assert_eq!(
        layout.get(&1).unwrap().area,
        Rect::new(Point2D::new(0.0, 5.0), Size2D::new(100.0, 195.0)),
    );
}

#[test]
pub fn layout_dirty_nodes() {
    let (mut layout, mut measurer) = test_utils();

    let mut mocked_tree = TestingTree::default();
    mocked_tree.add(
        0,
        None,
        vec![1],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(200.0)),
            Size::Pixels(Length::new(200.0)),
            Direction::Vertical,
        ),
    );
    mocked_tree.add(
        1,
        None,
        vec![2],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(100.0)),
            Size::Pixels(Length::new(100.0)),
            Direction::Vertical,
        ),
    );
    mocked_tree.add(
        2,
        Some(1),
        vec![],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(50.0)),
            Size::Pixels(Length::new(50.0)),
            Direction::Vertical,
        ),
    );

    layout.measure(
        0,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(1000.0, 1000.0)),
        &mut measurer,
        &mut mocked_tree,
    );

    // CASE 1
    // - Root is fixed
    // - Child A is fixed
    // - Child A[0] is fixed

    assert_eq!(
        layout.get(&0).unwrap().area,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(200.0, 200.0)),
    );

    assert_eq!(
        layout.get(&1).unwrap().area,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(100.0, 100.0)),
    );

    assert_eq!(
        layout.get(&2).unwrap().area,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(50.0, 50.0)),
    );

    mocked_tree.set_node(
        2,
        Node::from_size_and_direction(
            Size::Pixels(Length::new(10.0)),
            Size::Pixels(Length::new(10.0)),
            Direction::Vertical,
        ),
    );
    layout.invalidate(2);

    assert_eq!(
        layout.get_dirty_nodes(),
        &FxHashMap::from_iter([(2, DirtyReason::None)])
    );

    // CASE 2
    // Same as Case 1 but we make Child A depend on Child A[0]'s size

    mocked_tree.set_node(
        1,
        Node::from_size_and_direction(
            Size::Inner,
            Size::Pixels(Length::new(10.0)),
            Direction::Vertical,
        ),
    );
    layout.invalidate(1);

    assert_eq!(
        layout.get_dirty_nodes(),
        &FxHashMap::from_iter([(2, DirtyReason::None), (1, DirtyReason::None)])
    );

    // CASE 3
    // Same as Case 2, but triggers a change in Child A[0]

    mocked_tree.set_node(
        2,
        Node::from_size_and_direction(
            Size::Pixels(Length::new(50.0)),
            Size::Pixels(Length::new(50.0)),
            Direction::Vertical,
        ),
    );
    layout.invalidate(2);

    assert_eq!(
        layout.get_dirty_nodes(),
        &FxHashMap::from_iter([(2, DirtyReason::None), (1, DirtyReason::None)])
    );

    // CASE 4
    // Same as Case 3, but triggers a change in the root

    mocked_tree.set_node(
        0,
        Node::from_size_and_direction(
            Size::Pixels(Length::new(150.0)),
            Size::Pixels(Length::new(150.0)),
            Direction::Vertical,
        ),
    );
    layout.invalidate(0);

    assert_eq!(
        layout.get_dirty_nodes(),
        &FxHashMap::from_iter([
            (2, DirtyReason::None),
            (1, DirtyReason::None),
            (0, DirtyReason::None)
        ])
    );
}

#[test]
pub fn node_removal() {
    let (mut layout, mut measurer) = test_utils();

    let mut mocked_tree = TestingTree::default();
    mocked_tree.add(
        0,
        None,
        vec![1],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(200.0)),
            Size::Pixels(Length::new(200.0)),
            Direction::Vertical,
        ),
    );
    mocked_tree.add(
        1,
        Some(0),
        vec![2, 3],
        Node::from_size_and_direction(Size::Inner, Size::Inner, Direction::Vertical),
    );
    mocked_tree.add(
        2,
        Some(1),
        vec![4],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(200.0)),
            Size::Pixels(Length::new(200.0)),
            Direction::Vertical,
        ),
    );
    mocked_tree.add(
        3,
        Some(1),
        vec![],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(200.0)),
            Size::Pixels(Length::new(200.0)),
            Direction::Vertical,
        ),
    );
    mocked_tree.add(
        4,
        Some(2),
        vec![],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(200.0)),
            Size::Pixels(Length::new(200.0)),
            Direction::Vertical,
        ),
    );

    layout.measure(
        0,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(1000.0, 1000.0)),
        &mut measurer,
        &mut mocked_tree,
    );

    assert_eq!(layout.size(), 5);

    assert_eq!(
        layout.get(&0).unwrap().area,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(200.0, 200.0)),
    );

    assert_eq!(
        layout.get(&1).unwrap().area,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(200.0, 400.0)),
    );

    assert_eq!(
        layout.get(&3).unwrap().area,
        Rect::new(Point2D::new(0.0, 200.0), Size2D::new(200.0, 200.0)),
    );

    layout.remove(2, &mut mocked_tree, true);

    mocked_tree.remove(2);

    layout.find_best_root(&mut mocked_tree);

    assert_eq!(
        layout.get_dirty_nodes(),
        &FxHashMap::from_iter([(1, DirtyReason::None)])
    );

    assert_eq!(layout.size(), 3);

    layout.measure(
        0,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(1000.0, 1000.0)),
        &mut measurer,
        &mut mocked_tree,
    );

    assert_eq!(
        layout.get(&0).unwrap().area,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(200.0, 200.0)),
    );

    assert_eq!(
        layout.get(&1).unwrap().area,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(200.0, 200.0)),
    );

    assert_eq!(
        layout.get(&3).unwrap().area,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(200.0, 200.0)),
    );
}

#[test]
pub fn deep_tree() {
    let (mut layout, mut measurer) = test_utils();

    let mut mocked_tree = TestingTree::default();
    mocked_tree.add(
        0,
        None,
        vec![1],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(200.0)),
            Size::Pixels(Length::new(200.0)),
            Direction::Vertical,
        ),
    );
    mocked_tree.add(
        1,
        Some(0),
        vec![2],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(100.0)),
            Size::Pixels(Length::new(100.0)),
            Direction::Vertical,
        ),
    );
    mocked_tree.add(
        2,
        Some(1),
        vec![3],
        Node::from_size_and_direction(
            Size::Percentage(Length::new(100.0)),
            Size::Percentage(Length::new(100.0)),
            Direction::Vertical,
        ),
    );
    mocked_tree.add(
        3,
        Some(2),
        vec![4],
        Node::from_size_and_direction(
            Size::Percentage(Length::new(100.0)),
            Size::Percentage(Length::new(100.0)),
            Direction::Vertical,
        ),
    );
    mocked_tree.add(
        4,
        Some(3),
        vec![5],
        Node::from_size_and_direction(
            Size::Percentage(Length::new(100.0)),
            Size::Percentage(Length::new(100.0)),
            Direction::Vertical,
        ),
    );
    mocked_tree.add(
        5,
        Some(4),
        vec![],
        Node::from_size_and_direction(
            Size::Percentage(Length::new(100.0)),
            Size::Percentage(Length::new(100.0)),
            Direction::Vertical,
        ),
    );

    layout.find_best_root(&mut mocked_tree);
    layout.measure(
        0,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(1000.0, 1000.0)),
        &mut measurer,
        &mut mocked_tree,
    );

    mocked_tree.set_node(
        4,
        Node::from_size_and_direction(
            Size::Percentage(Length::new(200.0)),
            Size::Percentage(Length::new(200.0)),
            Direction::Vertical,
        ),
    );
    layout.invalidate(4);

    layout.find_best_root(&mut mocked_tree);
    assert_eq!(layout.get_root_candidate(), RootNodeCandidate::Valid(4));

    layout.measure(
        0,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(1000.0, 1000.0)),
        &mut measurer,
        &mut mocked_tree,
    );

    assert_eq!(layout.get_root_candidate(), RootNodeCandidate::None);
}

#[test]
pub fn node_reordering() {
    let (mut layout, mut measurer) = test_utils();

    let mut mocked_tree = TestingTree::default();
    mocked_tree.add(
        0,
        None,
        vec![1, 2],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(200.0)),
            Size::Pixels(Length::new(200.0)),
            Direction::Vertical,
        ),
    );
    mocked_tree.add(
        1,
        Some(0),
        vec![],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(200.0)),
            Size::Pixels(Length::new(100.0)),
            Direction::Vertical,
        ),
    );
    mocked_tree.add(
        2,
        Some(0),
        vec![],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(200.0)),
            Size::Pixels(Length::new(100.0)),
            Direction::Vertical,
        ),
    );

    layout.measure(
        0,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(1000.0, 1000.0)),
        &mut measurer,
        &mut mocked_tree,
    );

    assert_eq!(
        layout.get(&0).unwrap().area,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(200.0, 200.0)),
    );

    assert_eq!(
        layout.get(&1).unwrap().area,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(200.0, 100.0)),
    );

    assert_eq!(
        layout.get(&2).unwrap().area,
        Rect::new(Point2D::new(0.0, 100.0), Size2D::new(200.0, 100.0)),
    );

    layout.invalidate_with_reason(1, DirtyReason::Reorder);
    layout.invalidate_with_reason(2, DirtyReason::Reorder);

    mocked_tree.add(
        0,
        None,
        vec![2, 1],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(200.0)),
            Size::Pixels(Length::new(200.0)),
            Direction::Vertical,
        ),
    );

    layout.find_best_root(&mut mocked_tree);

    layout.measure(
        0,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(1000.0, 1000.0)),
        &mut measurer,
        &mut mocked_tree,
    );

    assert_eq!(
        layout.get(&2).unwrap().area,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(200.0, 100.0)),
    );

    assert_eq!(
        layout.get(&1).unwrap().area,
        Rect::new(Point2D::new(0.0, 100.0), Size2D::new(200.0, 100.0)),
    );

    // This will not cause the desired output expected by the user as we are not properly invalidating those reordered nodes
    layout.invalidate_with_reason(1, DirtyReason::None);

    mocked_tree.add(
        0,
        None,
        vec![1, 2],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(200.0)),
            Size::Pixels(Length::new(200.0)),
            Direction::Vertical,
        ),
    );

    layout.find_best_root(&mut mocked_tree);

    // That is why these nodes still have the same positions as before

    assert_eq!(
        layout.get(&2).unwrap().area,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(200.0, 100.0)),
    );

    assert_eq!(
        layout.get(&1).unwrap().area,
        Rect::new(Point2D::new(0.0, 100.0), Size2D::new(200.0, 100.0)),
    );
}

// ============================================================
// C27 红测（2026-11-28）：脏原因升级 + 缓存节点的引用通知
// ============================================================
//
// 背景（scrollview 同帧「滚动 + 追加子项」丢布局的 torin 级钉子）：
// - 原因升级：节点先被 InnerLayout 打脏（滚动偏移变化），同帧内容真变了
//   （普通 invalidate = None）。现状是 None 被 or_insert 丢弃、reason 停在
//   InnerLayout，measure 走「只平移不重测」快路——新尺寸永远不落缓存。
// - 引用通知：干净（缓存）节点因 measurer 要求重测了内部子树、累计
//   inner_sizes 已变，但缓存分支既不 notify_layout_references 也不回写
//   cache_node——滚动条的 content 尺寸停在旧值。

/// 修 A 钉 1：InnerLayout 先到、None 后到——reason 必须升级为 None。
#[test]
pub fn inner_layout_dirty_upgrades_when_plain_invalidate_arrives() {
    let (mut layout, mut measurer) = test_utils();

    let mut mocked_tree = TestingTree::default();
    mocked_tree.add(
        0,
        None,
        vec![1],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(200.0)),
            Size::Pixels(Length::new(200.0)),
            Direction::Vertical,
        ),
    );
    mocked_tree.add(
        1,
        Some(0),
        vec![],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(50.0)),
            Size::Pixels(Length::new(50.0)),
            Direction::Vertical,
        ),
    );

    layout.measure(
        0,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(1000.0, 1000.0)),
        &mut measurer,
        &mut mocked_tree,
    );

    // 同帧两笔：先 InnerLayout（滚动偏移），后普通 invalidate（内容变化）。
    layout.invalidate_with_reason(1, DirtyReason::InnerLayout);
    layout.invalidate(1);

    assert_eq!(
        layout.dirty.get(&1),
        Some(&DirtyReason::None),
        "普通 invalidate 必须把已存的 InnerLayout 升级为 None（内容变化不能被偏移变化的快路吞掉）"
    );
}

/// 修 A 钉 2（行为面）：同帧 InnerLayout + 内容变化后 measure，新尺寸必须落地。
#[test]
pub fn inner_layout_then_content_change_still_remeasures() {
    let (mut layout, mut measurer) = test_utils();

    let mut mocked_tree = TestingTree::default();
    mocked_tree.add(
        0,
        None,
        vec![1],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(200.0)),
            Size::Pixels(Length::new(200.0)),
            Direction::Vertical,
        ),
    );
    mocked_tree.add(
        1,
        Some(0),
        vec![],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(50.0)),
            Size::Pixels(Length::new(50.0)),
            Direction::Vertical,
        ),
    );

    layout.measure(
        0,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(1000.0, 1000.0)),
        &mut measurer,
        &mut mocked_tree,
    );
    assert_eq!(layout.get(&1).unwrap().area.size, Size2D::new(50.0, 50.0),);

    // 同帧：节点 1 尺寸真变了（50→10），且先来了一笔 InnerLayout 打脏。
    mocked_tree.set_node(
        1,
        Node::from_size_and_direction(
            Size::Pixels(Length::new(10.0)),
            Size::Pixels(Length::new(10.0)),
            Direction::Vertical,
        ),
    );
    layout.invalidate_with_reason(1, DirtyReason::InnerLayout);
    layout.invalidate(1);

    layout.measure(
        0,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(1000.0, 1000.0)),
        &mut measurer,
        &mut mocked_tree,
    );

    assert_eq!(
        layout.get(&1).unwrap().area.size,
        Size2D::new(10.0, 10.0),
        "InnerLayout 打脏在先也不能吞掉同帧的内容变化（快路只平移会把新尺寸永远留在缓存外）"
    );
}

/// 记录 notify_layout_references 调用的 measurer（修 B 的观察面）。
struct RecordingMeasurer {
    notified: Vec<(usize, Size2D)>,
}

impl LayoutMeasurer<usize> for RecordingMeasurer {
    fn measure(
        &mut self,
        _node_id: usize,
        _node: &Node,
        _size: &Size2D,
    ) -> Option<(Size2D, std::rc::Rc<dyn std::any::Any>)> {
        None
    }

    fn should_hook_measurement(&mut self, _node_id: usize) -> bool {
        false
    }

    fn should_measure_inner_children(&mut self, _node_id: usize) -> bool {
        true
    }

    fn notify_layout_references(
        &mut self,
        node_id: usize,
        _area: Area,
        _visible_area: Area,
        inner_sizes: Size2D,
    ) {
        self.notified.push((node_id, inner_sizes));
    }
}

/// 修 B 钉：干净（缓存）节点因 measurer 重测了内部子树、累计 inner_sizes 变了
/// ——必须 notify_layout_references 并把新 inner_sizes 回写缓存。
#[test]
pub fn cached_node_remeasured_children_notify_and_recache() {
    let mut measurer = Some(RecordingMeasurer {
        notified: Vec::new(),
    });
    let mut layout = Torin::<usize>::new();

    let mut mocked_tree = TestingTree::default();
    mocked_tree.add(
        0,
        None,
        vec![1],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(200.0)),
            Size::Pixels(Length::new(200.0)),
            Direction::Vertical,
        ),
    );
    let mut content_node = Node::from_size_and_direction(
        Size::Pixels(Length::new(100.0)),
        Size::Pixels(Length::new(100.0)),
        Direction::Vertical,
    );
    content_node.has_layout_references = true;
    mocked_tree.add(1, Some(0), vec![2], content_node);
    mocked_tree.add(
        2,
        Some(1),
        vec![],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(50.0)),
            Size::Pixels(Length::new(50.0)),
            Direction::Vertical,
        ),
    );

    layout.measure(
        0,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(1000.0, 1000.0)),
        &mut measurer,
        &mut mocked_tree,
    );

    // 基线：首次测量 notify 过节点 1（内容 50x50）。
    let first_round = std::mem::take(&mut measurer.as_mut().unwrap().notified);
    assert!(
        first_round
            .iter()
            .any(|(id, size)| *id == 1 && size.width == 50.0 && size.height == 50.0),
        "首次测量应 notify 节点 1 的布局引用（实测 {first_round:?}）"
    );

    // 子节点 2 尺寸变化并打脏；节点 1 本身干净（走缓存分支），但 measurer
    // 要求重测其内部子树（should_measure_inner_children = true）。
    mocked_tree.set_node(
        2,
        Node::from_size_and_direction(
            Size::Pixels(Length::new(10.0)),
            Size::Pixels(Length::new(10.0)),
            Direction::Vertical,
        ),
    );
    layout.invalidate(2);

    layout.measure(
        0,
        Rect::new(Point2D::new(0.0, 0.0), Size2D::new(1000.0, 1000.0)),
        &mut measurer,
        &mut mocked_tree,
    );

    let second_round = &measurer.as_ref().unwrap().notified;
    assert!(
        second_round
            .iter()
            .any(|(id, size)| *id == 1 && size.width == 10.0 && size.height == 10.0),
        "缓存节点内部子树重测、累计尺寸变化后必须再 notify（实测 {second_round:?}）"
    );
    assert_eq!(
        layout.get(&1).unwrap().inner_sizes,
        Size2D::new(10.0, 10.0),
        "新的累计 inner_sizes 必须回写缓存（否则滚动条读到的内容尺寸停在旧值）"
    );
}

/// 修 A 钉 3（入口面）：check_dirty_dependants 的 ignore=true 短路在「已存
/// InnerLayout、新来 None/Reorder」时必须升级覆写并继续父链，不能原样返回。
#[test]
pub fn check_dirty_entry_upgrades_inner_layout_instead_of_returning() {
    let (mut layout, _measurer) = test_utils();

    let mut mocked_tree = TestingTree::default();
    mocked_tree.add(
        0,
        None,
        vec![1],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(200.0)),
            Size::Pixels(Length::new(200.0)),
            Direction::Vertical,
        ),
    );
    mocked_tree.add(
        1,
        Some(0),
        vec![],
        Node::from_size_and_direction(
            Size::Pixels(Length::new(50.0)),
            Size::Pixels(Length::new(50.0)),
            Direction::Vertical,
        ),
    );

    layout.invalidate_with_reason(1, DirtyReason::InnerLayout);
    // 父链递归入口（torin.rs 的 does_depend_on_inner 分支）就是 ignore=true：
    // 已脏 + ignore 时现状直接 return，reason 停在 InnerLayout。
    layout.check_dirty_dependants(1, DirtyReason::None, &mut mocked_tree, true);

    assert_eq!(
        layout.dirty.get(&1),
        Some(&DirtyReason::None),
        "ignore=true 短路必须原因感知：InnerLayout 遇 None/Reorder 要升级覆写再继续父链"
    );
}
