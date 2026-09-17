// A tiny in-memory B+ tree.
//
// The idea:
// - All values live in leaf nodes. Leaves are linked (next) so you
//   can scan keys in order.
// - Internal nodes only hold separator keys + child pointers.
// - A node may hold at most `max_keys` keys. If it gets one more,
//   we split it in half and push one key up to the parent.
// - Only splitting the root makes the tree taller.

pub type PageId = u64;
type Stack = Vec<PageId>;
mod pager;

use anyhow::{Result, bail};

use crate::bptree::BPlusTreeNode::{Internal, Leaf};

// One node in the tree. We store nodes in the pager and
// refer to them by PageId (like a disk page number).
pub enum BPlusTreeNode<K, V> {
    Leaf {
        // Next leaf to the right (for range scans).
        next: Option<PageId>,
        // keys[i] belongs to values[i]. Always kept sorted.
        keys: Vec<K>,
        values: Vec<V>,
    },
    Internal {
        // Separator keys. children.len() is always keys.len() + 1.
        // Child i holds keys < keys[i] (roughly); see find_leaf.
        keys: Vec<K>,
        children: Vec<PageId>,
    },
}

pub struct BPlusTree<K: Ord + Clone, V> {
    root: Option<PageId>,
    max_keys: usize,
    pager: pager::PageManager<K, V>,
}

impl <K: Ord + Clone, V> BPlusTree<K, V> {
    fn find_leaf(&self, key: &K) -> Result<Option<(PageId, Stack)>> {
        let mut pg = self.root;
        let mut stack: Stack = Vec::new();

        while let Some(pgid) = pg {
            match self.pager.get(pgid) {
                Some(Leaf {..}) => return Ok( Some((pgid, stack)) ),
                Some(Internal {keys, children}) => {
                    let idx = keys.partition_point(|x| x <= key );
                    match children.get(idx) {
                        Some(&child) => {
                            stack.push(pgid);
                            pg = Some(child);
                        }
                        None => bail!("corrupt internal node {pgid}"),
                    }
                },
                None => bail!("Invalid Page Id dereference")
            }
        }

        Ok(None) // Empty tree
    }

    fn insert(&mut self, key: K, value: V) -> Result<()> {
        let Some((leaf_node_id, stack)) = self.find_leaf(&key)? else {
            // Tree is currently empty
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
            Leaf { keys, values, .. } => {
                match keys.binary_search(&key) {
                    Ok(idx) => {
                        values[idx] = value;
                        false
                    },
                    Err(pos) => {
                        keys.insert(pos, key);
                        values.insert(pos, value);
                        keys.len() > self.max_keys
                    }
                }
            },
            Internal { .. } => bail!("Expected a leaf node ONLY")
        };

        if !overflowed {
            return Ok(())
        }

        Ok(())
    }

    fn split(&mut self, page: PageId, stack: &[PageId]) -> Result<()> {
        match self.pager.get(page) {
            Some(Leaf { .. }) => todo!(),
            Some(Internal { .. }) => todo!(),
            None => bail!("Expected a non null page")
        }
    }

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
        let new_right = self.pager.new_page(Leaf { next: old_next, keys: right_keys, values: right_values });

        match self.pager.get_mut(page) {
            Some(Leaf { next, .. }) => {
                *next = Some(new_right);
            },
            _ => unreachable!("this sholuld definitely be a leaf node")
        }

        self.insert_in_parent(page, new_right, separator, stack)
    }

    fn insert_in_parent(&mut self, left_id: PageId, right_id: PageId, separator: K, stack: &[PageId]) -> Result<()> {
        if stack.is_empty() {
            let new_root = self.pager.new_page(Internal { keys: vec![separator], children: vec![left_id, right_id] });
            self.root = Some(new_root);
            return Ok(())
        }

        let parent_id = stack[stack.len() - 1];
        let parent_stack = &stack[..stack.len()-1];

        let overflow = match self.pager.get_mut(parent_id) {
            Some(Internal { keys, children }) => {
                let pos = keys.partition_point(|k| *k <= separator);
                keys.insert(pos, separator);
                children.insert(pos + 1, right_id);
                keys.len() > self.max_keys  
            },
            _ => unreachable!("A leaf node can never be a parent")
        };

        if overflow {
            return self.split_internal(parent_id, parent_stack)
        }

        Ok(())
    }

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
    
}