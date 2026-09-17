// A fake "disk": just a HashMap from page number to node.
//
// Real databases read/write fixed-size pages from a file.
// Here each new_page() hands out the next free PageId,
// so the tree code looks like it uses disk pages
// while everything stays in memory.

use std::collections::HashMap;

use super::{BPlusTreeNode, PageId};

pub struct PageManager<K, V> {
    pages: HashMap<PageId, BPlusTreeNode<K, V>>,
    next_page_id: PageId,
}

impl<K: Ord + Clone, V> PageManager<K, V> {
    pub fn new() -> Self {
        Self {
            pages: HashMap::new(),
            next_page_id: 0,
        }
    }

    // Store a node and get back its page number.
    pub fn new_page(&mut self, node: BPlusTreeNode<K, V>) -> PageId {
        let page_id = self.next_page_id;
        self.next_page_id += 1;
        self.pages.insert(page_id, node);
        page_id
    }

    // Read-only lookup.
    pub fn get(&self, page_id: PageId) -> Option<&BPlusTreeNode<K, V>> {
        self.pages.get(&page_id)
    }

    // Mutable lookup (for inserting keys or fixing pointers).
    pub fn get_mut(&mut self, page_id: PageId) -> Option<&mut BPlusTreeNode<K, V>> {
        self.pages.get_mut(&page_id)
    }
}
