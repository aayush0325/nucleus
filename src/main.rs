use nucleus::BPlusTree;

fn main() -> anyhow::Result<()> {
    let mut tree = BPlusTree::new(4);
    tree.insert(1, "one".to_string())?;
    tree.insert(2, "two".to_string())?;
    println!("{:?}", tree.get(&1)?);
    Ok(())
}
