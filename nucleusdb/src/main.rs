use anyhow::Result;
use libnucleus::pager;

fn main() -> Result<()> {
    // For now the CLI just reads and prints the SQLite header of a file.
    // Once the pager speaks pages, `header.page_size` is what every
    // other read will be measured against.
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: nucleusdb <database-file>"))?;

    let header = pager::from_file(&path)?;

    println!("SQLite database: {}", path);
    println!("  page size      : {} bytes", header.page_size);
    println!("  write version  : {}", header.write_version);
    println!("  read version   : {}", header.read_version);
    println!("  reserved space : {} bytes/page", header.reserved_space);
    println!("  freelist pages : {}", header.freelist_pages);
    println!("  schema cookie  : {}", header.schema_cookie);
    println!("  schema format  : {}", header.schema_format);
    println!("  text encoding  : {}", encoding_name(header.text_encoding));
    println!("  user version   : {}", header.user_version);
    println!("  application id : {}", header.application_id);
    println!("  library version: {}", header.version_number);

    Ok(())
}

fn encoding_name(code: u32) -> String {
    match code {
        1 => "UTF-8".to_string(),
        2 => "UTF-16LE".to_string(),
        3 => "UTF-16BE".to_string(),
        other => format!("unknown ({other})"),
    }
}
