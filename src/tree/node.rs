use std::sync::Arc;

use crate::rope::Measurable;
use crate::slice_utils::{start_width_to_index, index_to_width, end_width_to_index};
use crate::tree::{
    BranchChildren, Count, LeafSlice, SliceInfo, MAX_LEN, MAX_CHILDREN, MIN_LEN, MIN_CHILDREN,
};

#[derive(Debug, Clone)]
#[repr(u8, C)]
pub(crate) enum Node<M>
where
    M: Measurable,
{
    Leaf(LeafSlice<M>),
    Branch(BranchChildren<M>),
}

impl<M> Node<M>
where
    M: Measurable,
{
    /// Creates an empty [Node<M>].
    #[inline(always)]
    pub fn new() -> Self {
        Node::Leaf(LeafSlice::from_slice(&[]))
    }

    /// Total number of items in the [Node<M>].
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.slice_info().len as usize
    }

    /// Total number of items in the [Node<M>].
    #[inline(always)]
    pub fn width(&self) -> usize {
        self.slice_info().width as usize
    }

    /// Fetches a chunk mutably, and allows it to be edited via a closure.
    ///
    /// There are three parameters:
    /// - width: the chunk that contains this width is fetched,
    /// - node_info: this is the [SliceInfo] of the node it's being called on.
    ///              This makes it a little awkward to call, but is needed since
    ///              it's actually the parent node that contains the [SliceInfo],
    ///              so the info needs to be passed in.
    /// - edit: the closure that receives the chunk and does the edits.
    ///
    /// The closure is effectively the termination case for the recursion,
    /// and takes essentially same parameters and returns the same things as
    /// the method itself. In particular, the closure receives the width offset
    /// of the width within the given chunk and the [SliceInfo] of the chunk.
    /// The main difference is that it receives a [LeafSlice<M>] instead of a node.
    ///
    /// The closure is expected to return the updated [SliceInfo] of the [Node<M>],
    /// and if the node had to be split, then it also returns the right-hand
    /// [Node<M>] along with its [SliceInfo] as well.
    ///
    /// The main method call will then return the total updated [SliceInfo] for
    /// the whole tree, and a new [Node<M>] only if the whole tree had to be split.
    /// It is up to the caller to check for that new [Node<M>], and handle it by
    /// creating a new root with both the original [Node<M>] and the new node as
    /// children.
    pub fn edit_chunk_at_width<F>(
        &mut self,
        width: usize,
        node_info: SliceInfo,
        mut edit: F,
    ) -> (SliceInfo, Option<(SliceInfo, Arc<Node<M>>)>)
    where
        F: FnMut(
            usize,
            SliceInfo,
            &mut LeafSlice<M>,
        ) -> (SliceInfo, Option<(SliceInfo, Arc<Node<M>>)>),
    {
        match *self {
            Node::Leaf(ref mut slice) => edit(width, node_info, slice),
            Node::Branch(ref mut children) => {
                // Compact leaf children if we're very close to maximum leaf
                // fragmentation. This basically guards against excessive memory
                // ballooning when repeatedly appending to the end of a rope.
                // The constant here was arrived at experimentally, and is otherwise
                // fairly arbitrary.
                const FRAG_MIN_BYTES: usize = (MAX_LEN * MIN_CHILDREN) + (MAX_LEN / 32);
                if children.is_full()
                    && children.nodes()[0].is_leaf()
                    && (children.combined_info().len as usize) < FRAG_MIN_BYTES
                {
                    children.compact_leaves();
                }

                // Find the child we care about.
                let (child_i, acc_width) = children.search_width_only(width);
                let (info, _) = children.info()[child_i];

                // Recurse into the child.
                let (l_info, residual) = Arc::make_mut(&mut children.nodes_mut()[child_i])
                    .edit_chunk_at_width(width - acc_width, info, edit);

                let zero_width_end = children.nodes()[child_i].zero_width_end();
                children.info_mut()[child_i] = (l_info, zero_width_end);

                // Handle the residual node if there is one and return.
                if let Some((r_info, r_node)) = residual {
                    if children.len() < MAX_CHILDREN {
                        children.insert(child_i + 1, (r_info, r_node));
                        (node_info - info + l_info + r_info, None)
                    } else {
                        let r = children.insert_split(child_i + 1, (r_info, r_node));
                        let r_info = r.combined_info();
                        (
                            children.combined_info(),
                            Some((r_info, Arc::new(Node::Branch(r)))),
                        )
                    }
                } else {
                    (node_info - info + l_info, None)
                }
            }
        }
    }

    /// Removes elements in the range `start_index..end_index`.
    ///
    /// Returns (in this order):
    /// - The updated [SliceInfo] for the node.
    /// - Whether [fix_tree_seam()][Node::fix_tree_seam] needs to be run after this.
    ///
    /// WARNING: does not correctly handle all slice being removed. That
    /// should be special-cased in calling code.
    pub fn remove_range(
        &mut self,
        start_width: usize,
        end_width: usize,
        node_info: SliceInfo,
    ) -> (SliceInfo, bool) {
        match *self {
            // If it's a leaf
            Node::Leaf(ref mut slice) => {
                let start_index = start_width_to_index(slice, start_width);

                // In this circumstance, nothing needs to be done, since we're removing
                // in the middle of an element.
                if start_width == end_width && slice[start_index].width() > 0 {
                    return (SliceInfo::from_slice(slice), false);
                }

                let end_index = end_width_to_index(slice, end_width);

                // Remove slice and calculate new info & seam info.
                if start_index > 0 || end_index < slice.len() {
                    let seg_len = end_index - start_index; // Length of removal segement
                    if seg_len < (slice.len() - seg_len) {
                        #[allow(unused_mut)]
                        let info =
                            node_info - SliceInfo::from_slice(&slice[start_index..end_index]);

                        // Remove the slice.
                        slice.remove_range(start_index, end_index);

                        (info, false)
                    } else {
                        // Remove the slice.
                        slice.remove_range(start_index, end_index);

                        (SliceInfo::from_slice(slice), false)
                    }
                } else {
                    // Remove the whole slice.
                    slice.remove_range(start_index, end_index);

                    (SliceInfo::new(), false)
                }
            }

            // If it's internal, it's much more complicated
            Node::Branch(ref mut children) => {
                // Shared code for handling children.
                // Returns (in this order):
                // - Whether there's a possible CRLF seam that needs fixing.
                // - Whether the tree may need invariant fixing.
                // - Updated [SliceInfo] of the node.
                let handle_child = |children: &mut BranchChildren<M>,
                                    child_i: usize,
                                    c_width_acc: usize|
                 -> (bool, SliceInfo) {
                    // Recurse into child
                    let (tmp_info, _) = children.info()[child_i];
                    let tmp_width = children.info()[child_i].0.width as usize;
                    let (new_info, needs_fix) = Arc::make_mut(&mut children.nodes_mut()[child_i])
                        .remove_range(
                            start_width - c_width_acc.min(start_width),
                            (end_width - c_width_acc).min(tmp_width),
                            tmp_info,
                        );

                    // Handle result
                    if new_info.len == 0 {
                        children.remove(child_i);
                    } else {
                        let zero_width_end = children.nodes()[child_i].zero_width_end();
                        children.info_mut()[child_i] = (new_info, zero_width_end);
                    }

                    (needs_fix, new_info)
                };

                // Shared code for merging children
                let merge_child = |children: &mut BranchChildren<M>, child_i: usize| {
                    if child_i < children.len()
                        && children.len() > 1
                        && children.nodes()[child_i].is_undersized()
                    {
                        if child_i == 0 {
                            children.merge_distribute(child_i, child_i + 1);
                        } else {
                            children.merge_distribute(child_i - 1, child_i);
                        }
                    }
                };

                // Get child info for the two widths
                let ((l_child_i, l_width_acc), (r_child_i, r_width_acc)) =
                    children.search_width_range(start_width, end_width);

                // Both indices point into the same child
                if l_child_i == r_child_i {
                    let (info, _) = children.info()[l_child_i];
                    let (mut needs_fix, new_info) = handle_child(children, l_child_i, l_width_acc);

                    if children.len() > 0 {
                        merge_child(children, l_child_i);

                        // If we couldn't get all children >= minimum size, then
                        // we'll need to fix that later.
                        if children.nodes()[l_child_i.min(children.len() - 1)].is_undersized() {
                            needs_fix = true;
                        }
                    }

                    return (node_info - info + new_info, needs_fix);
                }
                // We're dealing with more than one child.
                else {
                    let mut needs_fix = false;

                    // Calculate the start..end range of nodes to be removed.
                    let r_child_exists: bool;
                    let start_i = l_child_i + 1;
                    let end_i =
                        if r_width_acc + children.info()[r_child_i].0.width as usize == end_width {
                            r_child_exists = false;
                            r_child_i + 1
                        } else {
                            r_child_exists = true;
                            r_child_i
                        };

                    // Remove the children
                    for _ in start_i..end_i {
                        children.remove(start_i);
                    }

                    // Handle right child
                    if r_child_exists {
                        let (fix, _) = handle_child(children, l_child_i + 1, r_width_acc);
                        needs_fix |= fix;
                    }

                    // Handle left child
                    let (fix, _) = handle_child(children, l_child_i, l_width_acc);
                    needs_fix |= fix;

                    if children.len() > 0 {
                        // Handle merging
                        let merge_extent = 1 + if r_child_exists { 1 } else { 0 };
                        for i in (l_child_i..(l_child_i + merge_extent)).rev() {
                            merge_child(children, i);
                        }

                        // If we couldn't get all children >= minimum size, then
                        // we'll need to fix that later.
                        if children.nodes()[l_child_i.min(children.len() - 1)].is_undersized() {
                            needs_fix = true;
                        }
                    }

                    // Return
                    return (children.combined_info(), needs_fix);
                }
            }
        }
    }

    pub fn append_at_depth(&mut self, mut other: Arc<Node<M>>, depth: usize) -> Option<Arc<Self>> {
        if depth == 0 {
            if let Node::Branch(ref mut children_l) = *self {
                if let Node::Branch(ref mut children_r) = *Arc::make_mut(&mut other) {
                    if (children_l.len() + children_r.len()) <= MAX_CHILDREN {
                        for _ in 0..children_r.len() {
                            children_l.push(children_r.remove(0));
                        }
                        return None;
                    } else {
                        children_l.distribute_with(children_r);
                        // Return lower down, to avoid borrow-checker.
                    }
                } else {
                    panic!("Tree-append siblings have differing types.");
                }
            } else if !other.is_leaf() {
                panic!("Tree-append siblings have differing types.");
            }

            return Some(other);
        } else if let Node::Branch(ref mut children) = *self {
            let last_i = children.len() - 1;
            let residual =
                Arc::make_mut(&mut children.nodes_mut()[last_i]).append_at_depth(other, depth - 1);
            children.update_child_info(last_i);
            if let Some(extra_node) = residual {
                if children.len() < MAX_CHILDREN {
                    children.push((extra_node.slice_info(), extra_node));
                    return None;
                } else {
                    let r_children = children.push_split((extra_node.slice_info(), extra_node));
                    return Some(Arc::new(Node::Branch(r_children)));
                }
            } else {
                return None;
            }
        } else {
            panic!("Reached leaf before getting to target depth.");
        }
    }

    pub fn prepend_at_depth(&mut self, other: Arc<Node<M>>, depth: usize) -> Option<Arc<Self>> {
        if depth == 0 {
            match *self {
                Node::Leaf(_) => {
                    if !other.is_leaf() {
                        panic!("Tree-append siblings have differing types.");
                    } else {
                        return Some(other);
                    }
                }
                Node::Branch(ref mut children_r) => {
                    let mut other = other;
                    if let Node::Branch(ref mut children_l) = *Arc::make_mut(&mut other) {
                        if (children_l.len() + children_r.len()) <= MAX_CHILDREN {
                            for _ in 0..children_l.len() {
                                children_r.insert(0, children_l.pop());
                            }
                            return None;
                        } else {
                            children_l.distribute_with(children_r);
                            // Return lower down, to avoid borrow-checker.
                        }
                    } else {
                        panic!("Tree-append siblings have differing types.");
                    }
                    return Some(other);
                }
            }
        } else if let Node::Branch(ref mut children) = *self {
            let residual =
                Arc::make_mut(&mut children.nodes_mut()[0]).prepend_at_depth(other, depth - 1);
            children.update_child_info(0);
            if let Some(extra_node) = residual {
                if children.len() < MAX_CHILDREN {
                    children.insert(0, (extra_node.slice_info(), extra_node));
                    return None;
                } else {
                    let mut r_children =
                        children.insert_split(0, (extra_node.slice_info(), extra_node));
                    std::mem::swap(children, &mut r_children);
                    return Some(Arc::new(Node::Branch(r_children)));
                }
            } else {
                return None;
            }
        } else {
            panic!("Reached leaf before getting to target depth.");
        }
    }

    /// Splits the [Node<M>] at `width`, returning the right side of the split.
    pub fn end_split(&mut self, width: usize) -> Self {
        debug_assert!(width != 0);
        debug_assert!(width != (self.slice_info().width as usize));
        match *self {
            Node::Leaf(ref mut slice) => {
                let index = end_width_to_index(slice, width);
                Node::Leaf(slice.split_off(index))
            }
            Node::Branch(ref mut children) => {
                let (child_i, acc_info) = children.search_end_width(width);
                let (child_info, _) = children.info()[child_i];

                if width == acc_info.width as usize {
                    Node::Branch(children.split_off(child_i))
                } else if width == (acc_info.width as usize + child_info.width as usize) {
                    Node::Branch(children.split_off(child_i + 1))
                } else {
                    let mut r_children = children.split_off(child_i + 1);

                    // Recurse
                    let r_node = Arc::make_mut(&mut children.nodes_mut()[child_i])
                        .end_split(width - acc_info.width as usize);

                    r_children.insert(0, (r_node.slice_info(), Arc::new(r_node)));

                    children.update_child_info(child_i);
                    r_children.update_child_info(0);

                    Node::Branch(r_children)
                }
            }
        }
    }

    /// Splits the [Node<M>] index `width`, returning the right side of the split.
    pub fn start_split(&mut self, width: usize) -> Self {
        debug_assert!(width != 0);
        debug_assert!(width != (self.slice_info().width as usize));
        match *self {
            Node::Leaf(ref mut slice) => {
                let index = start_width_to_index(slice, width);
                Node::Leaf(slice.split_off(index))
            }
            Node::Branch(ref mut children) => {
                let (child_i, acc_info) = children.search_start_width(width);
                let (child_info, _) = children.info()[child_i];

                if width == acc_info.width as usize {
                    Node::Branch(children.split_off(child_i))
                } else if width == (acc_info.width as usize + child_info.width as usize) {
                    Node::Branch(children.split_off(child_i + 1))
                } else {
                    let mut r_children = children.split_off(child_i + 1);

                    // Recurse
                    let r_node = Arc::make_mut(&mut children.nodes_mut()[child_i])
                        .end_split(width - acc_info.width as usize);

                    r_children.insert(0, (r_node.slice_info(), Arc::new(r_node)));

                    children.update_child_info(child_i);
                    r_children.update_child_info(0);

                    Node::Branch(r_children)
                }
            }
        }
    }

    /// Returns the chunk that contains the given index, and the [SliceInfo]
    /// corresponding to the start of the chunk.
    pub fn get_chunk_at_index(&self, mut index: usize) -> (&[M], SliceInfo) {
        let mut node = self;
        let mut info = SliceInfo::new();

        loop {
            match *node {
                Node::Leaf(ref slice) => {
                    return (slice, info);
                }
                Node::Branch(ref children) => {
                    let (child_i, acc_info) = children.search_index(index);
                    info += acc_info;
                    node = &*children.nodes()[child_i];
                    index -= acc_info.len as usize;
                }
            }
        }
    }

    /// Returns the chunk that contains the given width, and the [SliceInfo]
    /// corresponding to the start of the chunk.
    pub fn get_first_chunk_at_width(&self, mut width: usize) -> (&[M], SliceInfo) {
        let mut node = self;
        let mut info = SliceInfo::new();

        loop {
            match *node {
                Node::Leaf(ref slice) => {
                    return (slice, info);
                }
                Node::Branch(ref children) => {
                    let (child_i, acc_info) = children.search_start_width(width);
                    info += acc_info;

                    node = &*children.nodes()[child_i];
                    width -= acc_info.width as usize;
                }
            }
        }
    }

    /// Returns the chunk that contains the given width, and the [SliceInfo]
    /// corresponding to the start of the chunk.
    pub fn get_last_chunk_at_width(&self, mut width: usize) -> (&[M], SliceInfo) {
        let mut node = self;
        let mut info = SliceInfo::new();

        loop {
            match *node {
                Node::Leaf(ref slice) => {
                    return (slice, info);
                }
                Node::Branch(ref children) => {
                    let (child_i, acc_info) = children.search_end_width(width);
                    info += acc_info;

                    node = &*children.nodes()[child_i];
                    width -= acc_info.width as usize;
                }
            }
        }
    }

    /// Returns the [SliceInfo] at the given starting width sum.
    #[inline(always)]
    pub fn start_width_to_slice_info(&self, width: usize) -> SliceInfo {
        let (chunk, info) = self.get_first_chunk_at_width(width);
        let bi = start_width_to_index(chunk, width - info.width as usize);
        SliceInfo {
            len: info.len + bi as Count,
            width: width as Count,
        }
    }

    /// Returns the [SliceInfo] at the given ending width sum.
    #[inline(always)]
    pub fn end_width_to_slice_info(&self, width: usize) -> SliceInfo {
        let (chunk, info) = self.get_last_chunk_at_width(width);
        let bi = end_width_to_index(chunk, width - info.width as usize);
        SliceInfo {
            len: info.len + bi as Count,
            width: width as Count,
        }
    }

    /// Returns the [SliceInfo] at the given index.
    #[inline(always)]
    pub fn index_to_slice_info(&self, index: usize) -> SliceInfo {
        let (chunk, info) = self.get_chunk_at_index(index);
        let width = index_to_width(chunk, index - info.len as usize);
        SliceInfo {
            len: index as Count,
            width: info.width + width as Count,
        }
    }

    pub fn slice_info(&self) -> SliceInfo {
        match *self {
            Node::Leaf(ref slice) => SliceInfo::from_slice(slice),
            Node::Branch(ref children) => children.combined_info(),
        }
    }

    //-----------------------------------------

    pub fn child_count(&self) -> usize {
        if let Node::Branch(ref children) = *self {
            children.len()
        } else {
            panic!()
        }
    }

    pub fn children(&self) -> &BranchChildren<M> {
        match *self {
            Node::Branch(ref children) => children,
            _ => panic!(),
        }
    }

    pub fn children_mut(&mut self) -> &mut BranchChildren<M> {
        match *self {
            Node::Branch(ref mut children) => children,
            _ => panic!(),
        }
    }

    pub fn leaf_slice(&self) -> &[M] {
        match *self {
            Node::Leaf(ref slice) => slice,
            _ => unreachable!(),
        }
    }

    pub fn leaf_slice_mut(&mut self) -> &mut LeafSlice<M> {
        match *self {
            Node::Leaf(ref mut slice) => slice,
            _ => panic!(),
        }
    }

    pub fn is_leaf(&self) -> bool {
        match *self {
            Node::Leaf(_) => true,
            Node::Branch(_) => false,
        }
    }

    pub fn is_undersized(&self) -> bool {
        match *self {
            Node::Leaf(ref slice) => slice.len() < MIN_LEN,
            Node::Branch(ref children) => children.len() < MIN_CHILDREN,
        }
    }

    /// How many nodes deep the tree is.
    ///
    /// This counts root and leafs. For example, a single leaf node
    /// has depth 1.
    pub fn depth(&self) -> usize {
        let mut node = self;
        let mut depth = 0;

        loop {
            match *node {
                Node::Leaf(_) => return depth,
                Node::Branch(ref children) => {
                    depth += 1;
                    node = &*children.nodes()[0];
                }
            }
        }
    }

    /// Debugging tool to make sure that all of the meta-data of the
    /// tree is consistent with the actual data.
    pub fn assert_integrity(&self) {
        match *self {
            Node::Leaf(_) => {}
            Node::Branch(ref children) => {
                for ((info, zero_width_end), node) in children.iter() {
                    assert_eq!(*info, node.slice_info());
                    assert_eq!(*zero_width_end, node.zero_width_end());
                    node.assert_integrity();
                }
            }
        }
    }

    /// Checks that the entire tree is the same height everywhere.
    pub fn assert_balance(&self) -> usize {
        // Depth, child count, and leaf node emptiness
        match *self {
            Node::Leaf(_) => 1,
            Node::Branch(ref children) => {
                let first_depth = children.nodes()[0].assert_balance();
                for node in &children.nodes()[1..] {
                    assert_eq!(node.assert_balance(), first_depth);
                }
                first_depth + 1
            }
        }
    }

    /// Checks that all internal nodes have the minimum number of
    /// children and all non-root leaf nodes are non-empty.
    pub fn assert_node_size(&self, is_root: bool) {
        match *self {
            Node::Leaf(ref slice) => {
                // Leaf size
                if !is_root {
                    assert!(slice.len() > 0);
                }
            }
            Node::Branch(ref children) => {
                // Child count
                if is_root {
                    assert!(children.len() > 1);
                } else {
                    assert!(children.len() >= MIN_CHILDREN);
                }

                for node in children.nodes() {
                    node.assert_node_size(false);
                }
            }
        }
    }

    /// Fixes dangling nodes down the left side of the tree.
    ///
    /// Returns whether it did anything or not that would affect the
    /// parent.
    pub fn zip_fix_left(&mut self) -> bool {
        if let Node::Branch(ref mut children) = *self {
            let mut did_stuff = false;
            loop {
                let do_merge = (children.len() > 1)
                    && match *children.nodes()[0] {
                        Node::Leaf(ref slice) => slice.len() < MIN_LEN,
                        Node::Branch(ref children2) => children2.len() < MIN_CHILDREN,
                    };

                if do_merge {
                    did_stuff |= children.merge_distribute(0, 1);
                }

                if !Arc::make_mut(&mut children.nodes_mut()[0]).zip_fix_left() {
                    break;
                }
            }
            did_stuff
        } else {
            false
        }
    }

    /// Fixes dangling nodes down the right side of the tree.
    ///
    /// Returns whether it did anything or not that would affect the
    /// parent. True: did stuff, false: didn't do stuff
    pub fn zip_fix_right(&mut self) -> bool {
        if let Node::Branch(ref mut children) = *self {
            let mut did_stuff = false;
            loop {
                let last_i = children.len() - 1;
                let do_merge = (children.len() > 1)
                    && match *children.nodes()[last_i] {
                        Node::Leaf(ref slice) => slice.len() < MIN_LEN,
                        Node::Branch(ref children2) => children2.len() < MIN_CHILDREN,
                    };

                if do_merge {
                    did_stuff |= children.merge_distribute(last_i - 1, last_i);
                }

                if !Arc::make_mut(children.nodes_mut().last_mut().unwrap()).zip_fix_right() {
                    break;
                }
            }
            did_stuff
        } else {
            false
        }
    }

    /// Fixes up the tree after [remove_range()][Node::remove_range] or
	/// [Rope::append()].
    /// Takes the width of the start of the removal range.
    ///
    /// Returns whether it did anything or not that would affect the
    /// parent. True: did stuff, false: didn't do stuff
    pub fn fix_tree_seam(&mut self, widt: usize) -> bool {
        if let Node::Branch(ref mut children) = *self {
            let mut did_stuff = false;
            loop {
                // Do merging
                if children.len() > 1 {
                    let (child_i, start_info) = children.search_start_width(widt);
                    let mut do_merge = match *children.nodes()[child_i] {
                        Node::Leaf(ref slice) => slice.len() < MIN_LEN,
                        Node::Branch(ref children2) => children2.len() < MIN_CHILDREN,
                    };

                    if child_i == 0 {
                        if do_merge {
                            did_stuff |= children.merge_distribute(0, 1);
                        }
                    } else {
                        do_merge |= {
                            start_info.width as usize == widt
                                && match *children.nodes()[child_i - 1] {
                                    Node::Leaf(ref slice) => slice.len() < MIN_LEN,
                                    Node::Branch(ref children2) => children2.len() < MIN_CHILDREN,
                                }
                        };
                        if do_merge {
                            let res = children.merge_distribute(child_i - 1, child_i);
                            did_stuff |= res
                        }
                    }
                }

                // Do recursion
                let (child_i, start_info) = children.search_start_width(widt);

                if start_info.width as usize == widt && child_i != 0 {
                    let tmp = children.info()[child_i - 1].0.width as usize;
                    let effect_1 =
                        Arc::make_mut(&mut children.nodes_mut()[child_i - 1]).fix_tree_seam(tmp);
                    let effect_2 =
                        Arc::make_mut(&mut children.nodes_mut()[child_i]).fix_tree_seam(0);
                    if (!effect_1) && (!effect_2) {
                        break;
                    }
                } else if !Arc::make_mut(&mut children.nodes_mut()[child_i])
                    .fix_tree_seam(widt - start_info.width as usize)
                {
                    break;
                }
            }
            debug_assert!(children.is_info_accurate());
            did_stuff
        } else {
            false
        }
    }

    pub fn zero_width_end(&self) -> bool {
        match self {
            Node::Leaf(ref leaf) => leaf.zero_width_end(),
            Node::Branch(ref branch) => branch.zero_width_end(),
        }
    }
}
