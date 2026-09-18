//! A tiny in-memory B+ tree.
//!
//! All values live in leaf nodes, which are linked for ordered scans.
//! Internal nodes hold separator keys plus child pointers. Nodes split on
//! overflow and borrow-or-merge with siblings on underflow.
//!
//! # Examples
//!
//! ```
//! use kvrs::BPlusTree;
//!
//! let mut tree = BPlusTree::new(4);
//! tree.insert(1, "one".to_string()).unwrap();
//! tree.insert(2, "two".to_string()).unwrap();
//! assert_eq!(tree.get(&1).unwrap(), Some(&"one".to_string()));
//! assert_eq!(tree.delete(1).unwrap(), true);
//! assert!(tree.get(&1).unwrap().is_none());
//! ```

pub mod bptree;

pub use bptree::{BPlusTree, BPlusTreeNode, PageId};
