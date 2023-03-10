mod node;
mod branch_children;
mod leaf_slice;
mod slice_info;

pub(crate) use self::node::Node;
pub(crate) use self::branch_children::BranchChildren;
pub(crate) use self::leaf_slice::LeafSlice;
pub(crate) use self::slice_info::SliceInfo;

// Type used for storing tree metadata, such as indices and widths.
pub(crate) type Count = u64;

// Real constants used in release builds.
#[cfg(not(any(test, feature = "small_chunks")))]
mod constants {
    use super::SliceInfo;
    use smallvec::SmallVec;
    use std::{
        mem::{align_of, size_of},
        sync::Arc,
    };

    // Because stdlib's max is not const for some reason.
    // TODO: replace with stdlib max once it's const.
    const fn cmax(a: usize, b: usize) -> usize {
        if a > b {
            a
        } else {
            b
        }
    }

    // Aim for Node + Arc counters to be 1024 bytes.  Keeping the nodes
    // multiples of large powers of two makes it easier for the memory
    // allocator to avoid fragmentation.
    const TARGET_TOTAL_SIZE: usize = 1024;

    // Space that the strong and weak Arc counters take up in `ArcInner`.
    const ARC_COUNTERS_SIZE: usize = size_of::<std::sync::atomic::AtomicUsize>() * 2;

    // Misc useful info that we need below.
    const NODE_CHILDREN_ALIGN: usize = cmax(align_of::<Arc<u8>>(), align_of::<SliceInfo>());
    const NODE_TEXT_ALIGN: usize = align_of::<SmallVec<[u8; 16]>>();
    const START_OFFSET: usize = {
        const NODE_INNER_ALIGN: usize = cmax(NODE_CHILDREN_ALIGN, NODE_TEXT_ALIGN);
        // The +NODE_INNER_ALIGN is because of Node's enum discriminant.
        ARC_COUNTERS_SIZE + NODE_INNER_ALIGN
    };

    // Node maximums.
    pub(crate) const MAX_CHILDREN: usize = {
        let node_list_align = align_of::<Arc<u8>>();
        let info_list_align = align_of::<SliceInfo>();
        let field_gap = if node_list_align >= info_list_align {
            0
        } else {
            // This is over-conservative, because in reality it depends
            // on the number of elements.  But handling that is probably
            // more complexity than it's worth.
            info_list_align - node_list_align
        };

        // The -NODE_CHILDREN_ALIGN is for the `len` field in `NodeChildrenInternal`.
        let target_size = TARGET_TOTAL_SIZE - START_OFFSET - NODE_CHILDREN_ALIGN - field_gap;

        target_size / (size_of::<Arc<u8>>() + size_of::<SliceInfo>())
    };
    pub(crate) const MAX_LEN: usize = {
        let smallvec_overhead = size_of::<SmallVec<[u8; 16]>>() - 16;
        TARGET_TOTAL_SIZE - START_OFFSET - smallvec_overhead
    };

    // Node minimums.
    // Note: MIN_LEN is intentionally a little smaller than half
    // MAX_LEN, to give a little wiggle room when on the edge of
    // merging/splitting.
    pub(crate) const MIN_CHILDREN: usize = MAX_CHILDREN / 2;
    pub(crate) const MIN_LEN: usize = (MAX_LEN / 2) - (MAX_LEN / 32);
}

// Smaller constants used in debug builds. These are different from release
// in order to trigger deeper trees without having to use huge slice data in
// the tests.
#[cfg(any(test, feature = "small_chunks"))]
mod test_constants {
    pub(crate) const MAX_CHILDREN: usize = 5;
    pub(crate) const MIN_CHILDREN: usize = MAX_CHILDREN / 2;

    pub(crate) const MAX_LEN: usize = 9;
    pub(crate) const MIN_LEN: usize = (MAX_LEN / 2) - (MAX_LEN / 32);
}

#[cfg(not(test))]
pub(crate) use self::constants::{MAX_CHILDREN, MAX_LEN, MIN_CHILDREN, MIN_LEN};

#[cfg(test)]
pub(crate) use self::test_constants::{MAX_CHILDREN, MAX_LEN, MIN_CHILDREN, MIN_LEN};
