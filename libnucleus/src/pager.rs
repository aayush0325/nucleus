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
/// Parsing the 100-byte SQLite database header (page 1).
///
/// Every SQLite database file begins with a fixed 100-byte header stored
/// in the first page of the file. This module reads that header into a
/// typed [`DbHeader`] so that the rest of the engine (the pager, the B+
/// tree) knows the page size, text encoding, and file version before it
/// touches any other page.
///
/// The file format uses **big-endian** integers everywhere, and the
/// page-size field has one quirk worth memorizing: a stored value of `1`
/// means 65536 bytes.

use anyhow::{Context, Result, bail};

/// The fixed size, in bytes, of a SQLite database header.
pub const HEADER_SIZE: usize = 100;

/// Magic string at the start of every SQLite file: `"SQLite format 3\0"`.
///
/// Including the trailing NUL byte this is exactly 16 bytes.
pub const MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// A parsed view of the SQLite database header.
///
/// Fields are only the ones needed to bootstrap the rest of the engine;
/// the full 100-byte layout is documented at
/// <https://www.sqlite.org/fileformat.html>.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DbHeader {
    /// Page size in bytes. A stored value of 1 means 65536.
    pub page_size: u32,
    /// File format write version (1 = rollback journal, 2 = WAL).
    pub write_version: u8,
    /// File format read version (1 = legacy, 2 = WAL).
    pub read_version: u8,
    /// Bytes of "reserved" space at the end of every page (usually 0).
    pub reserved_space: u8,
    /// Number of freelist pages in the file.
    pub freelist_pages: u32,
    /// Schema cookie: incremented whenever the schema changes.
    pub schema_cookie: u32,
    /// Schema format number: 1, 2, or 4.
    pub schema_format: u32,
    /// Default page cache size suggested by the file.
    pub page_cache_size: u32,
    /// Text encoding: 1 = UTF-8, 2 = UTF-16LE, 3 = UTF-16BE.
    pub text_encoding: u32,
    /// Value set by `PRAGMA user_version`.
    pub user_version: u32,
    /// Value set by `PRAGMA application_id`.
    pub application_id: u32,
    /// SQLite library version that wrote the file (e.g. 3034002 = 3.34.2).
    pub version_number: u32,
}

/// Read a big-endian `u16` from `buf` at `offset`.
///
/// The SQLite file format stores all integers most-significant byte
/// first, so we never use the host-native `u16::from_ne_bytes`.
fn read_u16(buf: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([buf[offset], buf[offset + 1]])
}

/// Read a big-endian `u32` from `buf` at `offset`.
fn read_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

/// Parse a SQLite header from the first bytes of a database file.
///
/// `buf` must be at least [`HEADER_SIZE`] bytes (the whole of page 1).
/// The magic string is validated first; a mismatch means the file is not
/// a SQLite database at all.
///
/// # Examples
///
/// ```
/// use libnucleus::pager::{self, MAGIC, HEADER_SIZE};
///
/// // Build a minimal valid header: magic string + page size 4096.
/// let mut buf = [0u8; HEADER_SIZE];
/// buf[0..16].copy_from_slice(MAGIC);
/// buf[16..18].copy_from_slice(&4096u16.to_be_bytes());
///
/// let header = pager::parse(&buf).unwrap();
/// assert_eq!(header.page_size, 4096);
/// ```
///
/// A stored page size of `1` is decoded as 65536:
///
/// ```
/// use libnucleus::pager::{self, MAGIC, HEADER_SIZE};
///
/// let mut buf = [0u8; HEADER_SIZE];
/// buf[0..16].copy_from_slice(MAGIC);
/// buf[16..18].copy_from_slice(&1u16.to_be_bytes());
///
/// assert_eq!(pager::parse(&buf).unwrap().page_size, 65536);
/// ```
pub fn parse(buf: &[u8]) -> Result<DbHeader> {
    if buf.len() < HEADER_SIZE {
        bail!(
            "file too small: need at least {HEADER_SIZE} bytes for the header, got {}",
            buf.len()
        );
    }
    if &buf[0..16] != MAGIC {
        bail!("not a SQLite database: bad magic header");
    }

    // The page-size field is the one place the format is not a plain
    // power of two: 1 is reserved to mean 65536 because the field only
    // holds 16 bits (max 65535).
    let page_size_raw = read_u16(buf, 16);
    let page_size = if page_size_raw == 1 {
        65536
    } else {
        page_size_raw as u32
    };

    Ok(DbHeader {
        page_size,
        write_version: buf[18],
        read_version: buf[19],
        reserved_space: buf[20],
        freelist_pages: read_u32(buf, 36),
        schema_cookie: read_u32(buf, 40),
        schema_format: read_u32(buf, 44),
        page_cache_size: read_u32(buf, 48),
        text_encoding: read_u32(buf, 56),
        user_version: read_u32(buf, 60),
        application_id: read_u32(buf, 68),
        version_number: read_u32(buf, 96),
    })
}

/// Read the header directly from a database file on disk.
///
/// Opens the path, reads exactly the first [`HEADER_SIZE`] bytes, and
/// hands them to [`parse`].
///
/// # Examples
///
/// ```
/// use libnucleus::pager;
/// # use std::io::Write;
/// # let path = std::env::temp_dir().join("nucleus_doctest.db");
/// # let mut f = std::fs::File::create(&path).unwrap();
/// # f.write_all(b"SQLite format 3\0").unwrap();
/// # f.write_all(&4096u16.to_be_bytes()).unwrap();
/// # f.write_all(&[0u8; 82]).unwrap();
/// let header = pager::from_file(&path).unwrap();
/// assert_eq!(header.page_size, 4096);
/// # std::fs::remove_file(&path).ok();
/// ```
pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<DbHeader> {
    use std::io::Read;

    let path = path.as_ref();
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("failed to open database file {}", path.display()))?;

    let mut buf = [0u8; HEADER_SIZE];
    file.read_exact(&mut buf)
        .context("failed to read the 100-byte database header")?;

    parse(&buf)
}
