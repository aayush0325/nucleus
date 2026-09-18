// A tiny in-memory B+ tree.
//
// The idea:
// * All values live in leaf nodes. Leaves are linked with `next` so keys
//   can be scanned in order.
// * Internal nodes only hold separator keys plus child pointers.
// * A node may hold at most `max_keys` keys. Inserting one more splits
//   the node in half and pushes one key up to the parent.
// * Only splitting the root makes the tree taller.
// * On delete, a node that drops below `min_keys` first tries to borrow
//   from a sibling; if neither sibling can spare a key it merges with a
//   sibling and removes the separator from the parent, which may itself
//   underflow and propagate upward.

//! In-memory B+ tree with split on insert and borrow-or-merge on delete.
//!
//! Start with [`BPlusTree`]: [`BPlusTree::new`], [`BPlusTree::insert`],
//! [`BPlusTree::get`], [`BPlusTree::delete`].

pub type PageId = u64;
type Stack = Vec<PageId>;
pub mod pager;

use std::borrow::Borrow;
use std::ops::{Bound, RangeBounds};

use anyhow::{Result, bail};

use crate::bptree::BPlusTreeNode::{Internal, Leaf};

// One node in the tree. Nodes live in the pager and are addressed by
// PageId, which plays the role of a disk page number.
pub enum BPlusTreeNode<K, V> {
    Leaf {
        // Next leaf to the right, used for range scans.
        next: Option<PageId>,
        // Sorted keys, where keys[i] belongs to values[i].
        keys: Vec<K>,
        values: Vec<V>,
    },
    Internal {
        // Separator keys with one more child than keys.
        // See find_leaf for how a key picks a child.
        keys: Vec<K>,
        children: Vec<PageId>,
    },
}

pub struct BPlusTree<K: Ord + Clone, V> {
    root: Option<PageId>,
    max_keys: usize,
    pager: pager::PageManager<K, V>,
}

/// Ordered iterator over the entries of a [`BPlusTree`].
///
/// Walks the leaf `next` chain from left to right, so keys come out in
/// sorted order. The iterator borrows the tree, and yielded references
/// stay valid for that borrow. Construction is infallible: on corrupt
/// structure the iterator simply ends early.
///
/// Created by [`BPlusTree::iter`] and [`BPlusTree::range`].
///
/// # Examples
///
/// ```rust
/// use kvrs::BPlusTree;
///
/// let mut tree = BPlusTree::new(3);
/// for k in [3, 1, 2] {
///     tree.insert(k, k * 10).unwrap();
/// }
/// let all: Vec<_> = tree.iter().map(|(k, v)| (*k, *v)).collect();
/// assert_eq!(all, vec![(1, 10), (2, 20), (3, 30)]);
/// ```
pub struct Iter<'a, K: Ord + Clone, V> {
    tree: &'a BPlusTree<K, V>,
    leaf: Option<PageId>,
    pos: usize,
    end: Bound<K>,
}

impl<'a, K: Ord + Clone, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        // Copy the reference out so yielded items bind to the tree
        // lifetime `'a` rather than the `&mut self` borrow.
        let tree: &'a BPlusTree<K, V> = self.tree;
        loop {
            let id = self.leaf?;
            let (keys, values, next) = match tree.pager.get(id)? {
                Leaf { keys, values, next } => (keys, values, *next),
                _ => return None,
            };
            if self.pos < keys.len() {
                let key = &keys[self.pos];
                let past_end = match &self.end {
                    Bound::Unbounded => false,
                    Bound::Included(e) => key > e,
                    Bound::Excluded(e) => key >= e,
                };
                if past_end {
                    self.leaf = None;
                    return None;
                }
                let value = &values[self.pos];
                self.pos += 1;
                return Some((key, value));
            }
            // Leaf exhausted, follow the chain right.
            self.leaf = next;
            self.pos = 0;
        }
    }
}

impl<K: Ord + Clone, V> BPlusTree<K, V> {
    /// Create an empty tree that holds up to `max_keys` keys per node.
    ///
    /// # Panics
    ///
    /// Panics if `max_keys` is less than 2, since a smaller limit cannot
    /// describe a usable branching factor.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let tree: BPlusTree<i32, String> = BPlusTree::new(4);
    /// assert!(tree.is_empty());
    /// ```
    pub fn new(max_keys: usize) -> Self {
        assert!(max_keys >= 2, "max_keys must be >= 2");
        Self {
            root: None,
            max_keys,
            pager: pager::PageManager::new(),
        }
    }

    /// True when the tree holds no keys.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(3);
    /// assert!(tree.is_empty());
    /// tree.insert(1, 1).unwrap();
    /// assert!(!tree.is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// Minimum key count a non root node must keep after a delete.
    ///
    /// This is `floor(max_keys / 2)`, at least 1. The floor matters: with
    /// this bound a merge of two minimal nodes plus one pulled down
    /// separator always fits back into a single node.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let tree: BPlusTree<i32, i32> = BPlusTree::new(4);
    /// assert_eq!(tree.min_keys(), 2);
    /// let tree: BPlusTree<i32, i32> = BPlusTree::new(3);
    /// assert_eq!(tree.min_keys(), 1);
    /// ```
    pub fn min_keys(&self) -> usize {
        (self.max_keys / 2).max(1)
    }

    /// Find the leaf that should contain `key`.
    ///
    /// Returns the leaf id plus the ancestor stack from the root down to
    /// the leaf parent. Returns `None` for an empty tree.
    ///
    /// # Errors
    ///
    /// Returns an error if a page id cannot be resolved, which means the
    /// tree structure is corrupt.
    ///
    /// # Examples
    ///
    /// Descending to the right leaf is what makes point lookups work:
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(4);
    /// tree.insert(1, 10).unwrap();
    /// tree.insert(2, 20).unwrap();
    /// assert_eq!(tree.get(&1).unwrap(), Some(&10));
    /// assert_eq!(tree.get(&2).unwrap(), Some(&20));
    /// ```
    fn find_leaf<Q>(&self, key: &Q) -> Result<Option<(PageId, Stack)>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut pg = self.root;
        let mut stack: Stack = Vec::new();

        while let Some(pgid) = pg {
            match self.pager.get(pgid) {
                Some(Leaf { .. }) => return Ok(Some((pgid, stack))),
                Some(Internal { keys, children }) => {
                    let idx = keys.partition_point(|x| x.borrow() <= key);
                    match children.get(idx) {
                        Some(&child) => {
                            stack.push(pgid);
                            pg = Some(child);
                        }
                        None => bail!("corrupt internal node {pgid}"),
                    }
                }
                None => bail!("Invalid Page Id dereference"),
            }
        }

        // Empty tree.
        Ok(None)
    }

    /// Look up a key and borrow its value.
    ///
    /// Returns `None` when the tree is empty or the key is absent.
    ///
    /// # Errors
    ///
    /// Returns an error if the tree structure is corrupt.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(4);
    /// tree.insert(7, 70).unwrap();
    /// assert_eq!(tree.get(&7).unwrap(), Some(&70));
    /// assert_eq!(tree.get(&99).unwrap(), None);
    /// ```
    pub fn get<Q>(&self, key: &Q) -> Result<Option<&V>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let Some((leaf_id, _)) = self.find_leaf(key)? else {
            return Ok(None);
        };
        match self.pager.get(leaf_id) {
            Some(Leaf { keys, values, .. }) => match keys.binary_search_by(|k| k.borrow().cmp(key)) {
                Ok(idx) => Ok(Some(&values[idx])),
                Err(_) => Ok(None),
            },
            _ => bail!("find_leaf returned a non-leaf node"),
        }
    }

    /// Iterate over all entries in sorted key order.
    ///
    /// Starts at the leftmost leaf and follows the leaf `next` chain.
    /// An empty tree yields nothing.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree = BPlusTree::new(3);
    /// for k in [3, 1, 2] {
    ///     tree.insert(k, k).unwrap();
    /// }
    /// let keys: Vec<_> = tree.iter().map(|(k, _)| *k).collect();
    /// assert_eq!(keys, vec![1, 2, 3]);
    /// ```
    pub fn iter(&self) -> Iter<'_, K, V> {
        // Descend leftmost to the first leaf.
        let mut leaf = self.root;
        while let Some(id) = leaf {
            match self.pager.get(id) {
                Some(Leaf { .. }) => break,
                Some(Internal { children, .. }) => leaf = children.first().copied(),
                None => {
                    leaf = None;
                    break;
                }
            }
        }
        Iter {
            tree: self,
            leaf,
            pos: 0,
            end: Bound::Unbounded,
        }
    }

    /// Iterate over entries in `bounds`, in sorted key order.
    ///
    /// Accepts the same syntax as `BTreeMap::range`: `2..5`, `2..=5`,
    /// `2..`, `..5`, `..`. Seeks to the first key covered by the lower
    /// bound, then walks the leaf chain until the upper bound is passed.
    /// An empty tree, or a range matching nothing, yields nothing.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree = BPlusTree::new(3);
    /// for k in 1..=6 {
    ///     tree.insert(k, k * 10).unwrap();
    /// }
    /// let got: Vec<_> = tree.range(2..=4).map(|(k, v)| (*k, *v)).collect();
    /// assert_eq!(got, vec![(2, 20), (3, 30), (4, 40)]);
    /// let all: Vec<_> = tree.range(..).map(|(k, _)| *k).collect();
    /// assert_eq!(all, vec![1, 2, 3, 4, 5, 6]);
    /// ```
    pub fn range<R>(&self, bounds: R) -> Iter<'_, K, V>
    where
        R: RangeBounds<K>,
    {
        let end: Bound<K> = match bounds.end_bound() {
            Bound::Included(k) => Bound::Included(k.clone()),
            Bound::Excluded(k) => Bound::Excluded(k.clone()),
            Bound::Unbounded => Bound::Unbounded,
        };
        let (leaf, pos) = match bounds.start_bound() {
            Bound::Unbounded => {
                // Same leftmost descent as `iter`.
                let mut leaf = self.root;
                while let Some(id) = leaf {
                    match self.pager.get(id) {
                        Some(Leaf { .. }) => break,
                        Some(Internal { children, .. }) => leaf = children.first().copied(),
                        None => {
                            leaf = None;
                            break;
                        }
                    }
                }
                (leaf, 0)
            }
            Bound::Included(k) | Bound::Excluded(k) => {
                let inclusive = matches!(bounds.start_bound(), Bound::Included(_));
                match self.find_leaf(k) {
                    Ok(Some((id, _))) => match self.pager.get(id) {
                        Some(Leaf { keys, .. }) => match keys.binary_search(k) {
                            Ok(i) => (Some(id), if inclusive { i } else { i + 1 }),
                            Err(i) => (Some(id), i),
                        },
                        _ => (None, 0),
                    },
                    _ => (None, 0),
                }
            }
        };
        Iter {
            tree: self,
            leaf,
            pos,
            end,
        }
    }

    /// Insert a key value pair, overwriting the old value on duplicates.
    ///
    /// A full leaf splits in half and the first key of the new right half
    /// becomes the parent separator, growing the tree when the root splits.
    ///
    /// # Errors
    ///
    /// Returns an error if the tree structure is corrupt.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(3);
    /// tree.insert(1, 10).unwrap();
    /// tree.insert(1, 11).unwrap(); // overwrite
    /// assert_eq!(tree.get(&1).unwrap(), Some(&11));
    /// ```
    pub fn insert(&mut self, key: K, value: V) -> Result<()> {
        let Some((leaf_node_id, stack)) = self.find_leaf(&key)? else {
            // Empty tree, so the first insert creates a lone leaf root.
            let id = self.pager.new_page(Leaf {
                next: None,
                keys: vec![key],
                values: vec![value],
            });
            self.root = Some(id);
            return Ok(());
        };

        let Some(node) = self.pager.get_mut(leaf_node_id) else {
            bail!("the given node is corrupted")
        };

        let overflowed = match node {
            Leaf { keys, values, .. } => match keys.binary_search(&key) {
                Ok(idx) => {
                    values[idx] = value;
                    false
                }
                Err(pos) => {
                    keys.insert(pos, key);
                    values.insert(pos, value);
                    keys.len() > self.max_keys
                }
            },
            Internal { .. } => bail!("Expected a leaf node ONLY"),
        };

        if !overflowed {
            return Ok(());
        }

        self.split_leaf(leaf_node_id, &stack)
    }

    /// Split a full leaf into two halves linked in key order.
    ///
    /// The left half keeps the original page id, the right half gets a new
    /// page, and the first key of the right half is pushed to the parent.
    ///
    /// # Errors
    ///
    /// Returns an error if the page id is unknown or not a leaf.
    ///
    /// # Examples
    ///
    /// With `max_keys = 2`, leaf `[1, 2, 3]` becomes `[1]` linked to
    /// `[2, 3]` with separator `2` inserted into the parent:
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(2);
    /// for k in 1..=3 {
    ///     tree.insert(k, k).unwrap();
    /// }
    /// assert_eq!(tree.get(&3).unwrap(), Some(&3));
    /// ```
    fn split_leaf(&mut self, page: PageId, stack: &[PageId]) -> Result<()> {
        let (right_keys, right_values, old_next) = match self.pager.get_mut(page) {
            Some(Leaf { next, keys, values }) => {
                let mid = keys.len() / 2;
                (keys.split_off(mid), values.split_off(mid), next.take())
            }
            Some(_) => bail!("Expected a leaf node"),
            None => bail!("Given page id not found"),
        };

        let separator = right_keys.first().cloned().expect("This should never be empty");
        let new_right = self.pager.new_page(Leaf {
            next: old_next,
            keys: right_keys,
            values: right_values,
        });

        match self.pager.get_mut(page) {
            Some(Leaf { next, .. }) => {
                *next = Some(new_right);
            }
            _ => unreachable!("this sholuld definitely be a leaf node"),
        }

        self.insert_in_parent(page, new_right, separator, stack)
    }

    /// Insert a separator and right child pointer into the parent.
    ///
    /// An empty stack means the old node was the root, so a new root with
    /// the two children is created and the tree grows one level.
    ///
    /// # Errors
    ///
    /// Returns an error if the parent page is missing.
    ///
    /// # Examples
    ///
    /// Inserting separator `5` between leaf pages `L` and `R` turns parent
    /// `[3]` into `[3, 5]`, or creates root `[5]` with children `[L, R]`
    /// when the split leaf was the root:
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(2);
    /// for k in 1..=5 {
    ///     tree.insert(k, k).unwrap();
    /// }
    /// assert_eq!(tree.get(&5).unwrap(), Some(&5));
    /// ```
    fn insert_in_parent(
        &mut self,
        left_id: PageId,
        right_id: PageId,
        separator: K,
        stack: &[PageId],
    ) -> Result<()> {
        if stack.is_empty() {
            let new_root = self.pager.new_page(Internal {
                keys: vec![separator],
                children: vec![left_id, right_id],
            });
            self.root = Some(new_root);
            return Ok(());
        }

        let parent_id = stack[stack.len() - 1];
        let parent_stack = &stack[..stack.len() - 1];

        let overflow = match self.pager.get_mut(parent_id) {
            Some(Internal { keys, children }) => {
                let pos = keys.partition_point(|k| *k <= separator);
                keys.insert(pos, separator);
                children.insert(pos + 1, right_id);
                keys.len() > self.max_keys
            }
            _ => unreachable!("A leaf node can never be a parent"),
        };

        if overflow {
            return self.split_internal(parent_id, parent_stack);
        }

        Ok(())
    }

    /// Split a full internal node and promote the middle separator.
    ///
    /// The middle key moves up to the parent while the keys and children
    /// around it form the new right sibling.
    ///
    /// # Errors
    ///
    /// Returns an error if the page id is unknown or not internal.
    ///
    /// # Examples
    ///
    /// Internal `[1, 2, 3]` with `max_keys = 2` promotes `2`, leaving `[1]`
    /// on the left page and `[3]` on the new right page:
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(2);
    /// for k in 1..=8 {
    ///     tree.insert(k, k).unwrap();
    /// }
    /// assert_eq!(tree.get(&8).unwrap(), Some(&8));
    /// ```
    fn split_internal(&mut self, page_id: PageId, stack: &[PageId]) -> Result<()> {
        let (right_keys, right_children, promoted) = match self.pager.get_mut(page_id) {
            Some(Internal { keys, children }) => {
                let mid = keys.len() / 2;
                let mut rk = keys.split_off(mid);
                let promoted = rk.remove(0);
                let rc = children.split_off(mid + 1);
                (rk, rc, promoted)
            }
            _ => bail!("split_internal called on leaf {page_id}"),
        };

        let right_id = self.pager.new_page(Internal {
            keys: right_keys,
            children: right_children,
        });

        self.insert_in_parent(page_id, right_id, promoted, stack)
    }

    /// Delete `key` using borrow or merge rebalancing.
    ///
    /// Returns `true` when the key was present and `false` for a miss.
    /// Accepts both owned and borrowed keys, so `delete(k)` and
    /// `delete(&k)` both work through the `Borrow` bound.
    ///
    /// The steps are: descend to the leaf, remove the key, repair the
    /// parent separator when the first leaf key changed, return early when
    /// the leaf still holds enough keys, and otherwise call `fix_underflow`
    /// to borrow from a sibling or merge, possibly shrinking the tree.
    ///
    /// # Errors
    ///
    /// Returns an error if the tree structure is corrupt.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(3);
    /// tree.insert(1, 10).unwrap();
    /// tree.insert(2, 20).unwrap();
    /// assert_eq!(tree.delete(1).unwrap(), true);
    /// assert_eq!(tree.get(&1).unwrap(), None);
    /// assert_eq!(tree.delete(99).unwrap(), false);
    /// ```
    pub fn delete<Q>(&mut self, key: Q) -> Result<bool>
    where
        Q: Borrow<K>,
    {
        let key: &K = key.borrow();
        let Some((leaf_node_id, stack)) = self.find_leaf(key)? else {
            // Empty tree, so there is nothing to remove.
            return Ok(false);
        };

        // Remove the key while capturing the leaf length and new first key.
        // The pager borrow ends here so later steps can mutate other pages.
        let (removed_pos, remaining, new_first) = match self.pager.get_mut(leaf_node_id) {
            Some(Leaf { keys, values, .. }) => match keys.binary_search(key) {
                Ok(pos) => {
                    keys.remove(pos);
                    values.remove(pos);
                    (pos, keys.len(), keys.first().cloned())
                }
                Err(_) => return Ok(false),
            },
            Some(Internal { .. }) => bail!("find_leaf returned a non-leaf node"),
            None => bail!("Invalid Page Id dereference"),
        };

        // A lone root leaf needs no rebalancing. Clearing it when empty
        // lets the tree be reused as if freshly created.
        if stack.is_empty() {
            if remaining == 0 {
                self.root = None;
                self.pager.free(leaf_node_id);
            }
            return Ok(true);
        }

        // A parent separator always mirrors the first key of its right
        // child, so deleting position 0 stale dates keys[idx - 1].
        if removed_pos == 0 {
            if let Some(first) = new_first {
                let parent_id = *stack.last().expect("checked non-empty above");
                let child_idx = self.child_index(parent_id, leaf_node_id)?;
                if child_idx > 0 {
                    match self.pager.get_mut(parent_id) {
                        Some(Internal { keys, .. }) => {
                            keys[child_idx - 1] = first;
                        }
                        _ => bail!("parent {parent_id} is not internal"),
                    }
                }
            }
            // An emptied leaf has no new first key. The merge path below
            // removes its stale separator anyway.
        }

        // Enough keys remain, so the tree is still valid.
        if remaining >= self.min_keys() {
            return Ok(true);
        }

        // Too few keys remain, so borrow from a sibling or merge.
        self.fix_underflow(leaf_node_id, stack)?;

        Ok(true)
    }

    /// Position of a child inside its parent children vector.
    ///
    /// The index also locates the separators around the child: child `i`
    /// sits between `keys[i - 1]` on the left and `keys[i]` on the right.
    ///
    /// # Errors
    ///
    /// Returns an error if `parent_id` is not internal or `child_id` is
    /// not one of its children.
    ///
    /// # Examples
    ///
    /// A parent with children `[A, B, C]` reports index `1` for child `B`.
    /// Splitting first exercises the lookup, which every delete relies on:
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(3);
    /// for k in 1..=4 {
    ///     tree.insert(k, k).unwrap();
    /// }
    /// assert_eq!(tree.get(&4).unwrap(), Some(&4));
    /// assert!(tree.delete(4).unwrap());
    /// assert_eq!(tree.get(&4).unwrap(), None);
    /// ```
    fn child_index(&self, parent_id: PageId, child_id: PageId) -> Result<usize> {
        match self.pager.get(parent_id) {
            Some(Internal { children, .. }) => children
                .iter()
                .position(|&c| c == child_id)
                .ok_or_else(|| anyhow::anyhow!("node {child_id} not a child of {parent_id}")),
            _ => bail!("node {parent_id} is not internal"),
        }
    }

    /// Rebalance an underflowing node by borrowing or merging.
    ///
    /// The stack runs from the root down to the direct parent. An empty
    /// stack means the page is the root, which only ever shrinks. Otherwise
    /// the order is: borrow from the left sibling, borrow from the right
    /// sibling, merge into the left sibling, or merge the right sibling
    /// into this page. Merges delegate to `fix_parent_after_merge`.
    ///
    /// # Errors
    ///
    /// Returns an error if the tree structure is corrupt.
    ///
    /// # Examples
    ///
    /// Deleting down to leaf `[4]` with left sibling `[1, 2, 3]` borrows
    /// and ends as `[3, 4]`; with left sibling `[1]` instead, the two merge
    /// into `[1, 4]` and the parent loses one separator:
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(3);
    /// for k in 1..=12 {
    ///     tree.insert(k, k).unwrap();
    /// }
    /// for k in 1..=6 {
    ///     tree.delete(k).unwrap();
    /// }
    /// assert_eq!(tree.get(&7).unwrap(), Some(&7));
    /// ```
    fn fix_underflow(&mut self, page_id: PageId, mut stack: Stack) -> Result<()> {
        // A root never merges. An internal root with one child is replaced
        // by that child, which lowers the tree height by one.
        if stack.is_empty() {
            let only_child = match self.pager.get(page_id) {
                Some(Internal { keys, children }) if keys.is_empty() && children.len() == 1 => {
                    Some(children[0])
                }
                _ => None,
            };
            if let Some(child) = only_child {
                self.root = Some(child);
                self.pager.free(page_id);
            }
            return Ok(());
        }

        let min = self.min_keys();
        let parent_id = stack.pop().expect("checked non-empty above");
        let idx = self.child_index(parent_id, page_id)?;
        let is_leaf = matches!(self.pager.get(page_id), Some(Leaf { .. }));

        if is_leaf {
            if idx > 0 {
                let left_id = match self.pager.get(parent_id) {
                    Some(Internal { children, .. }) => children[idx - 1],
                    _ => bail!("parent {parent_id} is not internal"),
                };
                let left_len = match self.pager.get(left_id) {
                    Some(Leaf { keys, .. }) => keys.len(),
                    _ => bail!("sibling {left_id} is not a leaf"),
                };
                if left_len > min {
                    self.borrow_leaf_from_left(left_id, page_id, parent_id, idx)?;
                    return Ok(());
                }
            }

            // The left sibling could not help, so try the right one.
            let child_count = match self.pager.get(parent_id) {
                Some(Internal { children, .. }) => children.len(),
                _ => bail!("parent {parent_id} is not internal"),
            };
            if idx + 1 < child_count {
                let right_id = match self.pager.get(parent_id) {
                    Some(Internal { children, .. }) => children[idx + 1],
                    _ => bail!("parent {parent_id} is not internal"),
                };
                let right_len = match self.pager.get(right_id) {
                    Some(Leaf { keys, .. }) => keys.len(),
                    _ => bail!("sibling {right_id} is not a leaf"),
                };
                if right_len > min {
                    self.borrow_leaf_from_right(page_id, right_id, parent_id, idx)?;
                    return Ok(());
                }
            }

            // Neither sibling can spare a key, so combine two leaves.
            // Merging into the left page keeps the surviving page at the
            // left slot; without a left sibling the right page folds into
            // this one instead.
            if idx > 0 {
                let left_id = match self.pager.get(parent_id) {
                    Some(Internal { children, .. }) => children[idx - 1],
                    _ => bail!("parent {parent_id} is not internal"),
                };
                self.merge_leaf_into_left(left_id, page_id, parent_id, idx)?;
            } else {
                let right_id = match self.pager.get(parent_id) {
                    Some(Internal { children, .. }) => children[idx + 1],
                    _ => bail!("parent {parent_id} is not internal"),
                };
                self.merge_right_leaf_into_page(page_id, right_id, parent_id, idx)?;
            }

            self.fix_parent_after_merge(parent_id, stack)?;
            return Ok(());
        }

        if idx > 0 {
            let left_id = match self.pager.get(parent_id) {
                Some(Internal { children, .. }) => children[idx - 1],
                _ => bail!("parent {parent_id} is not internal"),
            };
            let left_len = match self.pager.get(left_id) {
                Some(Internal { keys, .. }) => keys.len(),
                _ => bail!("sibling {left_id} is not internal"),
            };
            if left_len > min {
                self.borrow_internal_from_left(left_id, page_id, parent_id, idx)?;
                return Ok(());
            }
        }

        let child_count = match self.pager.get(parent_id) {
            Some(Internal { children, .. }) => children.len(),
            _ => bail!("parent {parent_id} is not internal"),
        };
        if idx + 1 < child_count {
            let right_id = match self.pager.get(parent_id) {
                Some(Internal { children, .. }) => children[idx + 1],
                _ => bail!("parent {parent_id} is not internal"),
            };
            let right_len = match self.pager.get(right_id) {
                Some(Internal { keys, .. }) => keys.len(),
                _ => bail!("sibling {right_id} is not internal"),
            };
            if right_len > min {
                self.borrow_internal_from_right(page_id, right_id, parent_id, idx)?;
                return Ok(());
            }
        }

        if idx > 0 {
            let left_id = match self.pager.get(parent_id) {
                Some(Internal { children, .. }) => children[idx - 1],
                _ => bail!("parent {parent_id} is not internal"),
            };
            self.merge_internal_into_left(left_id, page_id, parent_id, idx)?;
        } else {
            let right_id = match self.pager.get(parent_id) {
                Some(Internal { children, .. }) => children[idx + 1],
                _ => bail!("parent {parent_id} is not internal"),
            };
            self.merge_right_internal_into_page(page_id, right_id, parent_id, idx)?;
        }

        self.fix_parent_after_merge(parent_id, stack)?;
        Ok(())
    }

    /// Recurse upward when a merge leaves the parent short on keys.
    ///
    /// A non root parent below `min_keys` is rebalanced with
    /// `fix_underflow`. A root parent only acts when it is empty, in which
    /// case its single child becomes the new root.
    ///
    /// # Errors
    ///
    /// Returns an error if the tree structure is corrupt.
    ///
    /// # Examples
    ///
    /// Merging two leaves removes one parent separator. If the parent was
    /// already minimal, say `[5]` becoming `[]`, this helper shrinks the
    /// tree or continues the fix one level higher:
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(3);
    /// for k in 1..=8 {
    ///     tree.insert(k, k).unwrap();
    /// }
    /// for k in 1..=8 {
    ///     tree.delete(k).unwrap();
    /// }
    /// assert!(tree.is_empty());
    /// ```
    fn fix_parent_after_merge(&mut self, parent_id: PageId, stack: Stack) -> Result<()> {
        let parent_len = match self.pager.get(parent_id) {
            Some(Internal { keys, .. }) => keys.len(),
            _ => bail!("node {parent_id} is not internal"),
        };
        if stack.is_empty() {
            // The parent is the root, so only an emptied root shrinks.
            if parent_len == 0 {
                self.fix_underflow(parent_id, stack)?;
            }
            return Ok(());
        }
        if parent_len < self.min_keys() {
            self.fix_underflow(parent_id, stack)?;
        }
        Ok(())
    }

    /// Move the last entry of the left leaf sibling into this page.
    ///
    /// The borrowed key becomes the new first key of the page and the
    /// parent separator between the siblings is updated to match it.
    ///
    /// # Errors
    ///
    /// Returns an error if any page id is unknown or mistyped.
    ///
    /// # Examples
    ///
    /// Left `[1, 2, 3]`, page `[4]`, separator `4`. After the move left is
    /// `[1, 2]`, page is `[3, 4]`, and the separator is `3`:
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(3);
    /// for k in 1..=6 {
    ///     tree.insert(k, k).unwrap();
    /// }
    /// tree.delete(2).unwrap();
    /// assert_eq!(tree.get(&3).unwrap(), Some(&3));
    /// ```
    fn borrow_leaf_from_left(
        &mut self,
        left_id: PageId,
        page_id: PageId,
        parent_id: PageId,
        idx: usize,
    ) -> Result<()> {
        debug_assert!(idx > 0);
        let (k, v) = match self.pager.get_mut(left_id) {
            Some(Leaf { keys, values, .. }) => {
                let k = keys.pop().expect("caller checked left_len > min");
                let v = values.pop().expect("keys/values stay in sync");
                (k, v)
            }
            _ => bail!("sibling {left_id} is not a leaf"),
        };
        match self.pager.get_mut(page_id) {
            Some(Leaf { keys, values, .. }) => {
                keys.insert(0, k.clone());
                values.insert(0, v);
            }
            _ => bail!("node {page_id} is not a leaf"),
        }
        match self.pager.get_mut(parent_id) {
            Some(Internal { keys, .. }) => {
                keys[idx - 1] = k;
            }
            _ => bail!("parent {parent_id} is not internal"),
        }
        Ok(())
    }

    /// Move the first entry of the right leaf sibling into this page.
    ///
    /// The borrowed key is appended at the back of the page and the parent
    /// separator is updated to the right sibling new first key.
    ///
    /// # Errors
    ///
    /// Returns an error if any page id is unknown or mistyped.
    ///
    /// # Examples
    ///
    /// Page `[1]`, right `[2, 3, 4]`, separator `2`. After the move page is
    /// `[1, 2]`, right is `[3, 4]`, and the separator is `3`:
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(3);
    /// for k in 1..=6 {
    ///     tree.insert(k, k).unwrap();
    /// }
    /// tree.delete(1).unwrap();
    /// assert_eq!(tree.get(&2).unwrap(), Some(&2));
    /// ```
    fn borrow_leaf_from_right(
        &mut self,
        page_id: PageId,
        right_id: PageId,
        parent_id: PageId,
        idx: usize,
    ) -> Result<()> {
        let (k, v) = match self.pager.get_mut(right_id) {
            Some(Leaf { keys, values, .. }) => {
                if keys.is_empty() {
                    bail!("sibling {right_id} is empty");
                }
                (keys.remove(0), values.remove(0))
            }
            _ => bail!("sibling {right_id} is not a leaf"),
        };
        match self.pager.get_mut(page_id) {
            Some(Leaf { keys, values, .. }) => {
                keys.push(k);
                values.push(v);
            }
            _ => bail!("node {page_id} is not a leaf"),
        };
        // The separator tracks the right sibling first key by definition.
        let new_sep = match self.pager.get(right_id) {
            Some(Leaf { keys, .. }) => keys.first().cloned().expect("right held > min keys"),
            _ => bail!("sibling {right_id} is not a leaf"),
        };
        match self.pager.get_mut(parent_id) {
            Some(Internal { keys, .. }) => {
                keys[idx] = new_sep;
            }
            _ => bail!("parent {parent_id} is not internal"),
        }
        Ok(())
    }

    /// Fold an underflowing leaf into its left sibling.
    ///
    /// All entries move left, the leaf `next` chain is stitched around the
    /// freed page, and the parent separator plus child pointer are removed.
    ///
    /// # Errors
    ///
    /// Returns an error if any page id is unknown or mistyped.
    ///
    /// # Examples
    ///
    /// Left `[1]`, page `[2]`, separator `2`. After the merge left is
    /// `[1, 2]`, the parent separator is gone, and the page is freed:
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(3);
    /// for k in 1..=4 {
    ///     tree.insert(k, k).unwrap();
    /// }
    /// for k in 1..=3 {
    ///     tree.delete(k).unwrap();
    /// }
    /// assert_eq!(tree.get(&4).unwrap(), Some(&4));
    /// ```
    fn merge_leaf_into_left(
        &mut self,
        left_id: PageId,
        page_id: PageId,
        parent_id: PageId,
        idx: usize,
    ) -> Result<()> {
        debug_assert!(idx > 0);
        let (mk, mv, next) = match self.pager.get_mut(page_id) {
            Some(Leaf { keys, values, next }) => {
                (std::mem::take(keys), std::mem::take(values), *next)
            }
            _ => bail!("node {page_id} is not a leaf"),
        };
        match self.pager.get_mut(left_id) {
            Some(Leaf {
                keys,
                values,
                next: left_next,
            }) => {
                keys.extend(mk);
                values.extend(mv);
                *left_next = next;
            }
            _ => bail!("sibling {left_id} is not a leaf"),
        }
        match self.pager.get_mut(parent_id) {
            Some(Internal { keys, children }) => {
                keys.remove(idx - 1);
                let removed = children.remove(idx);
                debug_assert_eq!(removed, page_id);
            }
            _ => bail!("parent {parent_id} is not internal"),
        }
        self.pager.free(page_id);
        Ok(())
    }

    /// Fold the right leaf sibling into an underflowing page.
    ///
    /// Used when there is no left sibling. Entries move into the page, the
    /// `next` chain skips the freed sibling, and the parent separator at
    /// the page slot is removed.
    ///
    /// # Errors
    ///
    /// Returns an error if any page id is unknown or mistyped.
    ///
    /// # Examples
    ///
    /// Page `[1]`, right `[2]`, separator `2`. After the merge the page is
    /// `[1, 2]`, the parent separator is gone, and the right page is freed:
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(3);
    /// for k in 1..=4 {
    ///     tree.insert(k, k).unwrap();
    /// }
    /// tree.delete(4).unwrap();
    /// tree.delete(3).unwrap();
    /// tree.delete(2).unwrap();
    /// assert_eq!(tree.get(&1).unwrap(), Some(&1));
    /// ```
    fn merge_right_leaf_into_page(
        &mut self,
        page_id: PageId,
        right_id: PageId,
        parent_id: PageId,
        idx: usize,
    ) -> Result<()> {
        let (rk, rv, next) = match self.pager.get_mut(right_id) {
            Some(Leaf { keys, values, next }) => {
                (std::mem::take(keys), std::mem::take(values), *next)
            }
            _ => bail!("sibling {right_id} is not a leaf"),
        };
        match self.pager.get_mut(page_id) {
            Some(Leaf {
                keys,
                values,
                next: page_next,
            }) => {
                keys.extend(rk);
                values.extend(rv);
                *page_next = next;
            }
            _ => bail!("node {page_id} is not a leaf"),
        }
        match self.pager.get_mut(parent_id) {
            Some(Internal { keys, children }) => {
                keys.remove(idx);
                let removed = children.remove(idx + 1);
                debug_assert_eq!(removed, right_id);
            }
            _ => bail!("parent {parent_id} is not internal"),
        }
        self.pager.free(right_id);
        Ok(())
    }

    /// Rotate one entry from the left internal sibling through the parent.
    ///
    /// The parent separator moves down to the front of the page and the
    /// sibling last key moves up to take its place, with the matching
    /// child pointer moving alongside.
    ///
    /// # Errors
    ///
    /// Returns an error if any page id is unknown or mistyped.
    ///
    /// # Examples
    ///
    /// Separator `5`, left keys `[1, 2]`, page keys `[7]`. After the rotation
    /// the separator is `2`, left is `[1]`, and page is `[5, 7]` with the
    /// sibling last child prepended:
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(3);
    /// for k in 1..=12 {
    ///     tree.insert(k, k).unwrap();
    /// }
    /// for k in 1..=8 {
    ///     tree.delete(k).unwrap();
    /// }
    /// assert_eq!(tree.get(&9).unwrap(), Some(&9));
    /// ```
    fn borrow_internal_from_left(
        &mut self,
        left_id: PageId,
        page_id: PageId,
        parent_id: PageId,
        idx: usize,
    ) -> Result<()> {
        debug_assert!(idx > 0);
        // Take the sibling last key plus its rightmost child.
        let (up_key, down_child) = match self.pager.get_mut(left_id) {
            Some(Internal { keys, children }) => {
                let k = keys.pop().expect("caller checked left_len > min");
                let c = children.pop().expect("children = keys + 1");
                (k, c)
            }
            _ => bail!("sibling {left_id} is not internal"),
        };
        // Swap the parent separator down into the page front.
        match self.pager.get_mut(parent_id) {
            Some(Internal { keys, .. }) => {
                let sep = std::mem::replace(&mut keys[idx - 1], up_key);
                match self.pager.get_mut(page_id) {
                    Some(Internal { keys, children }) => {
                        keys.insert(0, sep);
                        children.insert(0, down_child);
                    }
                    _ => bail!("node {page_id} is not internal"),
                }
            }
            _ => bail!("parent {parent_id} is not internal"),
        }
        Ok(())
    }

    /// Rotate one entry from the right internal sibling through the parent.
    ///
    /// The parent separator moves down to the back of the page and the
    /// sibling first key moves up to take its place, with the matching
    /// child pointer moving alongside.
    ///
    /// # Errors
    ///
    /// Returns an error if any page id is unknown or mistyped.
    ///
    /// # Examples
    ///
    /// Separator `5`, page keys `[3]`, right keys `[7, 9]`. After the rotation
    /// the separator is `7`, page is `[3, 5]`, and right is `[9]` with its
    /// first child appended to the page:
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(3);
    /// for k in 1..=12 {
    ///     tree.insert(k, k).unwrap();
    /// }
    /// for k in 10..=12 {
    ///     tree.delete(k).unwrap();
    /// }
    /// assert_eq!(tree.get(&1).unwrap(), Some(&1));
    /// ```
    fn borrow_internal_from_right(
        &mut self,
        page_id: PageId,
        right_id: PageId,
        parent_id: PageId,
        idx: usize,
    ) -> Result<()> {
        // Take the sibling first key plus its leftmost child.
        let (up_key, down_child) = match self.pager.get_mut(right_id) {
            Some(Internal { keys, children }) => {
                if keys.is_empty() {
                    bail!("sibling {right_id} is empty");
                }
                (keys.remove(0), children.remove(0))
            }
            _ => bail!("sibling {right_id} is not internal"),
        };
        match self.pager.get_mut(parent_id) {
            Some(Internal { keys, .. }) => {
                let sep = std::mem::replace(&mut keys[idx], up_key);
                match self.pager.get_mut(page_id) {
                    Some(Internal { keys, children }) => {
                        keys.push(sep);
                        children.push(down_child);
                    }
                    _ => bail!("node {page_id} is not internal"),
                }
            }
            _ => bail!("parent {parent_id} is not internal"),
        }
        Ok(())
    }

    /// Fold an underflowing internal node into its left sibling.
    ///
    /// The parent separator between them is pulled down between the two
    /// key ranges, then the page is freed and the parent loses one key
    /// and one child pointer.
    ///
    /// # Errors
    ///
    /// Returns an error if any page id is unknown or mistyped.
    ///
    /// # Examples
    ///
    /// Left `[1]`, separator `5`, page `[7]`. After the merge left is
    /// `[1, 5, 7]` with both child lists joined and the parent separator
    /// `5` removed:
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(3);
    /// for k in 1..=12 {
    ///     tree.insert(k, k).unwrap();
    /// }
    /// for k in 1..=10 {
    ///     tree.delete(k).unwrap();
    /// }
    /// assert_eq!(tree.get(&11).unwrap(), Some(&11));
    /// ```
    fn merge_internal_into_left(
        &mut self,
        left_id: PageId,
        page_id: PageId,
        parent_id: PageId,
        idx: usize,
    ) -> Result<()> {
        debug_assert!(idx > 0);
        let separator = match self.pager.get_mut(parent_id) {
            Some(Internal { keys, children }) => {
                let sep = keys.remove(idx - 1);
                let removed = children.remove(idx);
                debug_assert_eq!(removed, page_id);
                sep
            }
            _ => bail!("parent {parent_id} is not internal"),
        };
        let (mk, mc) = match self.pager.get_mut(page_id) {
            Some(Internal { keys, children }) => {
                (std::mem::take(keys), std::mem::take(children))
            }
            _ => bail!("node {page_id} is not internal"),
        };
        match self.pager.get_mut(left_id) {
            Some(Internal { keys, children }) => {
                keys.push(separator);
                keys.extend(mk);
                children.extend(mc);
            }
            _ => bail!("sibling {left_id} is not internal"),
        }
        self.pager.free(page_id);
        Ok(())
    }

    /// Fold the right internal sibling into an underflowing page.
    ///
    /// Used when there is no left sibling. The parent separator is pulled
    /// down between the two key ranges, then the sibling is freed and the
    /// parent loses one key and one child pointer.
    ///
    /// # Errors
    ///
    /// Returns an error if any page id is unknown or mistyped.
    ///
    /// # Examples
    ///
    /// Page `[1]`, separator `5`, right `[7]`. After the merge the page is
    /// `[1, 5, 7]` with both child lists joined and the parent separator
    /// `5` removed:
    ///
    /// ```rust
    /// use kvrs::BPlusTree;
    ///
    /// let mut tree: BPlusTree<i32, i32> = BPlusTree::new(3);
    /// for k in 1..=12 {
    ///     tree.insert(k, k).unwrap();
    /// }
    /// for k in 5..=12 {
    ///     tree.delete(k).unwrap();
    /// }
    /// assert_eq!(tree.get(&1).unwrap(), Some(&1));
    /// ```
    fn merge_right_internal_into_page(
        &mut self,
        page_id: PageId,
        right_id: PageId,
        parent_id: PageId,
        idx: usize,
    ) -> Result<()> {
        let separator = match self.pager.get_mut(parent_id) {
            Some(Internal { keys, children }) => {
                let sep = keys.remove(idx);
                let removed = children.remove(idx + 1);
                debug_assert_eq!(removed, right_id);
                sep
            }
            _ => bail!("parent {parent_id} is not internal"),
        };
        let (rk, rc) = match self.pager.get_mut(right_id) {
            Some(Internal { keys, children }) => {
                (std::mem::take(keys), std::mem::take(children))
            }
            _ => bail!("sibling {right_id} is not internal"),
        };
        match self.pager.get_mut(page_id) {
            Some(Internal { keys, children }) => {
                keys.push(separator);
                keys.extend(rk);
                children.extend(rc);
            }
            _ => bail!("node {page_id} is not internal"),
        }
        self.pager.free(right_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_all<K: Ord + Clone, V: Clone>(t: &BPlusTree<K, V>) -> Vec<K> {
        // Walk leaves through leftmost descent, then follow the next chain.
        let mut root = match t.root {
            Some(r) => r,
            None => return vec![],
        };
        loop {
            match t.pager.get(root).unwrap() {
                Leaf { .. } => break,
                Internal { children, .. } => root = children[0],
            }
        }
        let mut out = vec![];
        let mut cur = Some(root);
        while let Some(id) = cur {
            match t.pager.get(id).unwrap() {
                Leaf { keys, next, .. } => {
                    out.extend(keys.iter().cloned());
                    cur = *next;
                }
                Internal { .. } => panic!("leaf chain hit internal"),
            }
        }
        out
    }

    fn assert_valid<K: Ord + Clone + std::fmt::Debug, V: Clone>(t: &BPlusTree<K, V>) {
        if t.root.is_none() {
            return;
        }
        // The leaf chain must stay strictly sorted.
        let all = collect_all(t);
        for w in all.windows(2) {
            assert!(w[0] < w[1], "leaf chain not strictly sorted: {all:?}");
        }
    }

    #[test]
    fn delete_missing_and_empty() -> Result<()> {
        let mut t: BPlusTree<i32, i32> = BPlusTree::new(3);
        assert!(!t.delete(1)?);
        t.insert(1, 10)?;
        assert!(!t.delete(2)?);
        assert!(t.delete(1)?);
        assert!(t.get(&1)?.is_none());
        Ok(())
    }

    #[test]
    fn delete_borrow_and_merge() -> Result<()> {
        let mut t = BPlusTree::new(3);
        for k in 1..=12 {
            t.insert(k, k * 10)?;
        }
        assert_valid(&t);
        // Deleting the first half forces borrows, merges, and separator fixes.
        for k in 1..=6 {
            assert!(t.delete(k)?, "missing {k}");
            assert!(t.get(&k)?.is_none());
            assert_valid(&t);
        }
        assert_eq!(collect_all(&t), vec![7, 8, 9, 10, 11, 12]);
        // Deleting the rest empties the tree and clears the root.
        for k in 7..=12 {
            assert!(t.delete(k)?);
            assert_valid(&t);
        }
        assert!(t.root.is_none());
        // The emptied tree accepts inserts again.
        t.insert(42, 1)?;
        assert_eq!(t.get(&42)?, Some(&1));
        Ok(())
    }

    #[test]
    fn delete_shrink_height() -> Result<()> {
        let mut t = BPlusTree::new(4);
        for k in 0..30 {
            t.insert(k, k)?;
        }
        for k in 0..30 {
            t.delete(k)?;
            assert_valid(&t);
            for r in (k + 1)..30 {
                assert_eq!(t.get(&r)?, Some(&r), "lost {r} after deleting {k}");
            }
        }
        assert!(t.root.is_none());
        Ok(())
    }

    #[test]
    fn iter_yields_sorted_order_across_leaves() -> Result<()> {
        let mut t = BPlusTree::new(3);
        // Shuffled insert forces several splits and a multi leaf chain.
        for k in [9, 1, 5, 3, 7, 11, 2, 8, 4, 6, 10, 0] {
            t.insert(k, k * 10)?;
        }
        let got: Vec<_> = t.iter().map(|(k, v)| (*k, *v)).collect();
        let want: Vec<_> = (0..12).map(|k| (k, k * 10)).collect();
        assert_eq!(got, want);
        Ok(())
    }

    #[test]
    fn iter_empty_and_after_delete() -> Result<()> {
        let mut t: BPlusTree<i32, i32> = BPlusTree::new(3);
        assert_eq!(t.iter().count(), 0);
        for k in 1..=8 {
            t.insert(k, k)?;
        }
        // Merges stitch the `next` chain; iteration must skip freed pages.
        for k in [2, 4, 6, 8] {
            t.delete(k)?;
        }
        let keys: Vec<_> = t.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec![1, 3, 5, 7]);
        Ok(())
    }

    #[test]
    fn range_bounds() -> Result<()> {
        let mut t = BPlusTree::new(3);
        for k in 1..=6 {
            t.insert(k, k * 10)?;
        }
        let keys = |it: super::Iter<'_, i32, i32>| it.map(|(k, _)| *k).collect::<Vec<_>>();
        assert_eq!(keys(t.range(2..=4)), vec![2, 3, 4]);
        assert_eq!(keys(t.range(2..4)), vec![2, 3]);
        assert_eq!(keys(t.range(4..)), vec![4, 5, 6]);
        assert_eq!(keys(t.range(..3)), vec![1, 2]);
        assert_eq!(keys(t.range(..)), vec![1, 2, 3, 4, 5, 6]);
        assert!(keys(t.range(10..20)).is_empty());
        assert!(keys(t.range(4..2)).is_empty());
        Ok(())
    }

    #[test]
    fn delete_both_owned_and_ref_forms() -> Result<()> {
        let mut t = BPlusTree::new(3);
        t.insert(5, 50)?;
        assert!(t.delete(&5)?);
        t.insert(6, 60)?;
        assert!(t.delete(6)?);
        Ok(())
    }
}
