
pub enum BPlusTreeNode {
    Leaf {

    },

    Internal {

    }
}

pub struct BPlusTree {
    page: [u8; 4],
    fan_out_factor: usize,
}