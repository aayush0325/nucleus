//! Page storage for the B+ tree.
//!
//! A fake disk: a map from page number to node. Real databases read and
//! write fixed size pages from a file; here [`PageManager::new_page`]
//! hands out the next free [`PageId`](super::PageId), so the tree code
//! looks like it uses disk pages while everything stays in memory.

use std::collections::HashMap;

use super::{BPlusTreeNode, PageId};

/// Owns every tree node, keyed by page id.
pub struct PageManager<K, V> {
    pages: HashMap<PageId, BPlusTreeNode<K, V>>,
    next_page_id: PageId,
}

impl<K: Ord + Clone, V> PageManager<K, V> {
    /// Create an empty page store.
    ///
    /// # Examples
    ///
    /// ```
    /// use libnucleus::pager::PageManager;
    ///
    /// let store: PageManager<i32, i32> = PageManager::new();
    /// assert!(store.get(0).is_none());
    /// ```
    pub fn new() -> Self {
        Self {
            pages: HashMap::new(),
            next_page_id: 0,
        }
    }

    /// Store a node and get back its page number.
    ///
    /// Page numbers increase monotonically from zero.
    ///
    /// # Examples
    ///
    /// ```
    /// use libnucleus::pager::PageManager;
    /// use libnucleus::BPlusTreeNode;
    ///
    /// let mut store = PageManager::new();
    /// let id = store.new_page(BPlusTreeNode::Leaf {
    ///     next: None,
    ///     keys: vec![1],
    ///     values: vec![10],
    /// });
    /// assert_eq!(id, 0);
    /// assert!(store.get(0).is_some());
    /// ```
    pub fn new_page(&mut self, node: BPlusTreeNode<K, V>) -> PageId {
        let page_id = self.next_page_id;
        self.next_page_id += 1;
        self.pages.insert(page_id, node);
        page_id
    }

    /// Read-only lookup by page id.
    ///
    /// # Examples
    ///
    /// ```
    /// use libnucleus::pager::PageManager;
    /// use libnucleus::BPlusTreeNode;
    ///
    /// let mut store = PageManager::new();
    /// let id = store.new_page(BPlusTreeNode::Leaf {
    ///     next: None,
    ///     keys: vec![1],
    ///     values: vec![10],
    /// });
    /// assert!(matches!(store.get(id), Some(BPlusTreeNode::Leaf { .. })));
    /// ```
    pub fn get(&self, page_id: PageId) -> Option<&BPlusTreeNode<K, V>> {
        self.pages.get(&page_id)
    }

    /// Mutable lookup, used to insert keys or fix pointers.
    ///
    /// # Examples
    ///
    /// ```
    /// use libnucleus::pager::PageManager;
    /// use libnucleus::BPlusTreeNode;
    ///
    /// let mut store = PageManager::new();
    /// let id = store.new_page(BPlusTreeNode::Leaf {
    ///     next: None,
    ///     keys: vec![1],
    ///     values: vec![10],
    /// });
    /// if let Some(BPlusTreeNode::Leaf { keys, .. }) = store.get_mut(id) {
    ///     keys.push(2);
    /// }
    /// assert!(store.get(id).is_some());
    /// ```
    pub fn get_mut(&mut self, page_id: PageId) -> Option<&mut BPlusTreeNode<K, V>> {
        self.pages.get_mut(&page_id)
    }

    /// Release a page freed by a merge or by clearing the root.
    ///
    /// # Examples
    ///
    /// ```
    /// use libnucleus::pager::PageManager;
    /// use libnucleus::BPlusTreeNode;
    ///
    /// let mut store = PageManager::new();
    /// let id = store.new_page(BPlusTreeNode::Leaf {
    ///     next: None,
    ///     keys: vec![1],
    ///     values: vec![10],
    /// });
    /// store.free(id);
    /// assert!(store.get(id).is_none());
    /// ```
    pub fn free(&mut self, page_id: PageId) {
        self.pages.remove(&page_id);
    }
}
