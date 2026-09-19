//! A tiny SQLite-compatible storage engine.
//!
//! libnucleus provides an in-memory B+ tree implementation that is
//! designed to be compatible with SQLite's semantics.
//!
//! # Examples
//!
//! ```
//! use libnucleus::BPlusTree;
//!
//! let mut tree = BPlusTree::new(4);
//! tree.insert(1, "one".to_string()).unwrap();
//! tree.insert(2, "two".to_string()).unwrap();
//! assert_eq!(tree.get(&1).unwrap(), Some(&"one".to_string()));
//! assert_eq!(tree.delete(1).unwrap(), true);
//! assert!(tree.get(&1).unwrap().is_none());
//! ```

pub mod bptree;
pub mod pager;

pub use bptree::{BPlusTree, BPlusTreeNode, PageId};
