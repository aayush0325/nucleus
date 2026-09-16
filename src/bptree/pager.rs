use std::collections::HashMap;

use super::{PageId, BPlusTreeNode};

pub struct PageManager<K: Ord, V> {
    pages: HashMap<PageId, BPlusTreeNode<K, V>>,
    next_page_id: PageId
}

impl <K: Ord, V> PageManager<K, V> {
    pub fn new() -> Self {
        Self {
            pages: HashMap::new(),
            next_page_id: 0
        }
    }

    pub fn new_page(&mut self, node: BPlusTreeNode<K, V>) -> PageId {
        let page_id = self.next_page_id;
        self.next_page_id += 1;

        self.pages.insert(page_id, node);

        page_id
    }

    pub fn get(&self, page_id: PageId) -> Option<&BPlusTreeNode<K, V>> {
        self.pages.get(&page_id)
    }

    pub fn get_mut(&mut self, page_id: PageId) -> Option<&mut BPlusTreeNode<K, V>> {
        self.pages.get_mut(&page_id)
    }
}