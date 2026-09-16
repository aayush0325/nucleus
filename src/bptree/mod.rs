pub type PageId = u64;
mod pager;

use anyhow::{Result, bail};

use crate::bptree::BPlusTreeNode::{Internal, Leaf};

pub enum BPlusTreeNode<K: Ord, V> {
    Leaf {
        next: Option<PageId>,
        keys: Vec<K>, // keys.len() == values.len()
        values: Vec<V>
    },

    Internal {
        keys: Vec<K>, // keys.len() + 1 = children.len()
        children: Vec<PageId>
    }
}

/// Split when keys.len() > max_keys, underflow when keys.len() < ceil (max_keys/2)
/// only the root node split/merge increases/decreases the height of the tree
pub struct BPlusTree<K: Ord, V> {
    root: Option<PageId>,
    max_keys: usize,
    pager: pager::PageManager<K, V>
}

impl<K: Ord, V> BPlusTree <K, V> {
    fn new(max_keys: usize) -> Self {
        Self {
            root: None,
            max_keys,
            pager: pager::PageManager::new()
        }
    }

    fn insert(&mut self, key: K, value: V) -> Result<()> {
        let Some(leaf_id) = self.find_leaf(&key)? else {
            // empty tree -> new root leaf
            let id = self.pager.new_page(Leaf {
                next: None,
                keys: vec![key],
                values: vec![value],
            });
            self.root = Some(id);
            return Ok(());
        };

        let Some(node) = self.pager.get_mut(leaf_id) else {
            bail!("missing leaf page {leaf_id}");
        };
        match node {
            Leaf { keys, values, .. } => {
                let mut tosplit: bool = false;
                match keys.binary_search(&key) {
                    Ok(idx) => {
                        values[idx] = value;
                    },
                    Err(pos) => {
                        keys.insert(pos, key);
                        values.insert(pos, value);
                        if keys.len() > self.max_keys {
                            tosplit = true;
                        }
                    }
                }

                if tosplit {
                    self.split(leaf_id);
                }

                Ok(())
            }
            Internal { .. } => bail!("find_leaf returned internal node {leaf_id}"),
        }
    }

    fn split(&mut self, page_id: PageId) {
        
    }

    fn find_leaf(&self, key: &K) -> Result<Option<PageId>> {
        let mut pg = self.root;

        while let Some(pgid) = pg {
            let Some(node) = self.pager.get(pgid) else {
                bail!("missing page {pgid}");
            };
            match node {
                Leaf { .. } => return Ok(Some(pgid)),
                Internal { keys, children } => {
                    let idx = keys.partition_point(|k| k <= key);
                    let Some(&child) = children.get(idx) else {
                        bail!("corrupt internal node {pgid}: idx {idx} out of bounds");
                    };
                    pg = Some(child);
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {

}