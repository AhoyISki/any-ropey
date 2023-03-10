use std::sync::Arc;

use smallvec::SmallVec;

use crate::rope::{Measurable, Rope};
use crate::tree::{BranchChildren, LeafSlice, Node, MAX_LEN, MAX_CHILDREN, MIN_LEN};

/// An efficient incremental [Rope<M>] builder.
///
/// This is used to efficiently build ropes from sequences of [M][Measurable]
/// chunks.
///
/// Unlike repeatedly calling [Rope::insert()] on the end of a rope,
/// this API runs in time linear to the amount of data fed to it, and
/// is overall much faster.
///
/// # Example
/// ```
/// # use any_rope::RopeBuilder;
/// # use any_rope::Lipsum::*;
/// #
/// let mut builder = RopeBuilder::new();
///
/// builder.append(Lorem);
/// builder.append(Ipsum);
/// builder.append(Dolor(70));
/// builder.append(Sit);
/// builder.append(Amet);
///
/// let rope = builder.finish();
///
/// assert_eq!(rope, [Lorem, Ipsum, Dolor(70), Sit, Amet].as_slice());
/// ```
#[derive(Debug, Clone)]
pub struct RopeBuilder<M>
where
    M: Measurable,
{
    stack: SmallVec<[Arc<Node<M>>; 4]>,
    buffer: Vec<M>,
    last_chunk_len: usize,
}

impl<M> RopeBuilder<M>
where
    M: Measurable,
{
    /// Creates a new RopeBuilder, ready for input.
    pub fn new() -> Self {
        RopeBuilder {
            stack: {
                let mut stack = SmallVec::new();
                stack.push(Arc::new(Node::new()));
                stack
            },
            buffer: Vec::new(),
            last_chunk_len: 0,
        }
    }

    /// Appends `chunk` to the end of the in-progress [Rope<M>].
    ///
    /// Call this method repeatedly to incrementally build up a
    /// [Rope<M>]. The passed slice chunk can be as large or small as
    /// desired, but larger chunks are more efficient.
    pub fn append_slice(&mut self, chunk: &[M]) {
        self.append_internal(chunk, false);
    }

    /// Appends a single [M][Measurable] to the end of the in-progress [Rope<M>]
    ///
    /// Call this method repeatedly to incrementally build up a [Rope<M>].
    pub fn append(&mut self, element: M) {
        self.append_internal(&[element], false);
    }

    /// Finishes the build, and returns the [Rope<M>].
    ///
    /// Note: this method consumes the builder. If you want to continue
    /// building other ropes with the same prefix, you can clone the builder
    /// before calling this function.
    pub fn finish(mut self) -> Rope<M> {
        // Append the last leaf
        self.append_internal(&[], true);
        self.finish_internal(true)
    }

    /// Builds a rope all at once from a single slice.
    ///
    /// This avoids the creation and use of the internal buffer. This is
    /// for internal use only, because the public-facing API has
    /// [Rope::from_slice()], which actually uses this for its implementation.
    pub(crate) fn build_at_once(mut self, chunk: &[M]) -> Rope<M> {
        self.append_internal(chunk, true);
        self.finish_internal(true)
    }

    /// NOT PART OF THE PUBLIC API (hidden from docs for a reason!).
    ///
    /// Appends `contents` to the in-progress rope as a single leaf
    /// node (chunk). This is useful for building ropes with specific
    /// chunk configurations for testing purposes. It will happily append
    /// both empty and more-than-max-size chunks.
    ///
    /// This makes no attempt to be consistent with the standard [RopeBuilder::append()]
    /// method, and should not be used in conjunction with it.
    #[doc(hidden)]
    pub fn _append_chunk(&mut self, contents: &[M]) {
        self.append_leaf_node(Arc::new(Node::Leaf(LeafSlice::from_slice(contents))));
    }

    /// NOT PART OF THE PUBLIC API (hidden from docs for a reason!).
    ///
    /// Finishes the build without doing any tree fixing to adhere
    /// to the btree invariants. To be used with [RopeBuilder::append_chunk()] to
    /// construct ropes with specific chunk boundaries for testing.
    #[doc(hidden)]
    pub fn _finish_no_fix(self) -> Rope<M> {
        self.finish_internal(false)
    }

    //-----------------------------------------------------------------

    // Internal workings of `append()`.
    fn append_internal(&mut self, chunk: &[M], is_last_chunk: bool) {
        let mut chunk = chunk;

        // Repeatedly chop slices off the end of the input, creating
        // leaf nodes out of them and appending them to the tree.
        while !chunk.is_empty() || (!self.buffer.is_empty() && is_last_chunk) {
            // Get the slice for the next leaf
            let (leaf_slice, remainder) = self.get_next_leaf_slice(chunk, is_last_chunk);
            chunk = remainder;

            self.last_chunk_len = chunk.len();

            // Append the leaf to the rope
            match leaf_slice {
                NextSlice::None => break,
                NextSlice::UseBuffer => {
                    let leaf_slice = LeafSlice::from_slice(self.buffer.as_slice());
                    self.append_leaf_node(Arc::new(Node::Leaf(leaf_slice)));
                    self.buffer.clear();
                }
                NextSlice::Slice(s) => {
                    self.append_leaf_node(Arc::new(Node::Leaf(LeafSlice::from_slice(s))));
                }
            }
        }
    }

    // Internal workings of `finish()`.
    //
    // When `fix_tree` is false, the resulting node tree is NOT fixed up
    // to adhere to the btree invariants. This is useful for some testing
    // code. But generally, `fix_tree` should be set to true.
    fn finish_internal(mut self, fix_tree: bool) -> Rope<M> {
        // Zip up all the remaining nodes on the stack
        let mut stack_index = self.stack.len() - 1;
        while stack_index >= 1 {
            let node = self.stack.pop().unwrap();
            if let Node::Branch(ref mut children) = *Arc::make_mut(&mut self.stack[stack_index - 1])
            {
                children.push((node.slice_info(), node));
            } else {
                unreachable!();
            }
            stack_index -= 1;
        }

        // Create the rope.
        let mut rope = Rope {
            root: self.stack.pop().unwrap(),
        };

        // Fix up the tree to be well-formed.
        if fix_tree {
            Arc::make_mut(&mut rope.root).zip_fix_right();
            if self.last_chunk_len < MIN_LEN && self.last_chunk_len != rope.len() {
                // Merge the last chunk if it was too small.
                let index = rope.width() - rope.index_to_width(rope.len() - self.last_chunk_len);
                Arc::make_mut(&mut rope.root).fix_tree_seam(index);
            }
            rope.pull_up_singular_nodes();
        }

        return rope;
    }

    // Returns (next_leaf_slice, remaining_slice)
    #[inline(always)]
    fn get_next_leaf_slice<'a>(
        &mut self,
        slice: &'a [M],
        is_last_chunk: bool,
    ) -> (NextSlice<'a, M>, &'a [M]) {
        assert!(
            self.buffer.len() < MAX_LEN,
            "RopeBuilder: buffer is already full when receiving a chunk! \
             This should never happen!",
        );

        // Simplest case: empty buffer and enough in `slice` for a full
        // chunk, so just chop a chunk off from `slice` and use that.
        if self.buffer.is_empty() && slice.len() >= MAX_LEN {
            let split_index = MAX_LEN.min(slice.len() - 1);
            return (
                NextSlice::Slice(&slice[..split_index]),
                &slice[split_index..],
            );
        }
        // If the buffer + `slice` is enough for a full chunk, push enough
        // of `slice` onto the buffer to fill it and use that.
        else if (slice.len() + self.buffer.len()) >= MAX_LEN {
            let split_index = MAX_LEN - self.buffer.len();
            self.buffer.extend_from_slice(&slice[..split_index]);
            return (NextSlice::UseBuffer, &slice[split_index..]);
        }
        // If we don't have enough slice for a full chunk.
        else {
            // If it's our last chunk, wrap it all up!
            if is_last_chunk {
                if self.buffer.is_empty() {
                    return if slice.is_empty() {
                        (NextSlice::None, &[])
                    } else {
                        (NextSlice::Slice(slice), &[])
                    };
                } else {
                    self.buffer.extend_from_slice(slice);
                    return (NextSlice::UseBuffer, &[]);
                }
            }
            // Otherwise, just push to the buffer.
            else {
                self.buffer.extend_from_slice(slice);
                return (NextSlice::None, &[]);
            }
        }
    }

    fn append_leaf_node(&mut self, leaf: Arc<Node<M>>) {
        let last = self.stack.pop().unwrap();
        match *last {
            Node::Leaf(_) => {
                if last.leaf_slice().is_empty() {
                    self.stack.push(leaf);
                } else {
                    let mut children = BranchChildren::new();
                    children.push((last.slice_info(), last));
                    children.push((leaf.slice_info(), leaf));
                    self.stack.push(Arc::new(Node::Branch(children)));
                }
            }

            Node::Branch(_) => {
                self.stack.push(last);
                let mut left = leaf;
                let mut stack_index = (self.stack.len() - 1) as isize;
                loop {
                    if stack_index < 0 {
                        // We're above the root, so do a root split.
                        let mut children = BranchChildren::new();
                        children.push((left.slice_info(), left));
                        self.stack.insert(0, Arc::new(Node::Branch(children)));
                        break;
                    } else if self.stack[stack_index as usize].child_count() < (MAX_CHILDREN - 1) {
                        // There's room to add a child, so do that.
                        Arc::make_mut(&mut self.stack[stack_index as usize])
                            .children_mut()
                            .push((left.slice_info(), left));
                        break;
                    } else {
                        // Not enough room to fit a child, so split.
                        left = Arc::new(Node::Branch(
                            Arc::make_mut(&mut self.stack[stack_index as usize])
                                .children_mut()
                                .push_split((left.slice_info(), left)),
                        ));
                        std::mem::swap(&mut left, &mut self.stack[stack_index as usize]);
                        stack_index -= 1;
                    }
                }
            }
        }
    }
}

impl<M> Default for RopeBuilder<M>
where
    M: Measurable,
{
    fn default() -> Self {
        Self::new()
    }
}

enum NextSlice<'a, M>
where
    M: Measurable,
{
    None,
    UseBuffer,
    Slice(&'a [M]),
}

//===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Lipsum::{self, *};

    /// 70 elements, total width of 135.
    fn lorem_ipsum() -> Vec<Lipsum> {
        (0..70)
            .into_iter()
            .map(|num| match num % 14 {
                0 | 7 => Lorem,
                1 | 8 => Ipsum,
                2 => Dolor(4),
                3 | 10 => Sit,
                4 | 11 => Amet,
                5 => Consectur("hello"),
                6 => Adipiscing(true),
                9 => Dolor(8),
                12 => Consectur("bye"),
                13 => Adipiscing(false),
                _ => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn rope_builder_01() {
        let mut builder = RopeBuilder::new();

        for _ in 0..5 {
            builder.append_slice(&[Lorem, Ipsum, Dolor(4), Sit, Amet]);
            builder.append_slice(&[Consectur("hello"), Adipiscing(true)]);
            builder.append_slice(&[Lorem, Ipsum, Dolor(8), Sit, Amet]);
            builder.append_slice(&[Consectur("bye"), Adipiscing(false)]);
        }

        let rope = builder.finish();

        assert_eq!(rope, lorem_ipsum());

        rope.assert_integrity();
        rope.assert_invariants();
    }

    #[test]
    fn rope_builder_02() {
        let mut builder = RopeBuilder::new();

        for _ in 0..5 {
            builder.append(Lorem);
            builder.append(Ipsum);
            builder.append(Dolor(4));
            builder.append(Sit);
            builder.append(Amet);
            builder.append_slice(&[Consectur("hello"), Adipiscing(true)]);
            builder.append(Lorem);
            builder.append(Ipsum);
            builder.append(Dolor(8));
            builder.append(Sit);
            builder.append(Amet);
            builder.append_slice(&[Consectur("bye"), Adipiscing(false)]);
        }

        let rope = builder.finish();

        assert_eq!(rope, lorem_ipsum());

        rope.assert_integrity();
        rope.assert_invariants();
    }

    #[test]
    fn rope_builder_default_01() {
        let mut builder = RopeBuilder::default();

        for _ in 0..5 {
            builder.append_slice(&[Lorem, Ipsum, Dolor(4), Sit, Amet]);
            builder.append_slice(&[Consectur("hello"), Adipiscing(true)]);
            builder.append_slice(&[Lorem, Ipsum, Dolor(8), Sit, Amet]);
            builder.append_slice(&[Consectur("bye"), Adipiscing(false)]);
        }

        let rope = builder.finish();

        assert_eq!(rope, lorem_ipsum());

        rope.assert_integrity();
        rope.assert_invariants();
    }
}
