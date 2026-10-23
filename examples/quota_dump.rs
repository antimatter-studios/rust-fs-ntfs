//! What a volume's `$Extend\$Quota` holds, read by this crate (#1085), one
//! tab-separated row each:
//!
//!   owner  SID  owner-id                          every $O entry
//!   quota  owner-id  flags  bytes-used  SID       every $Q record (`-`: none)
//!   file   PATH  owner-id  charged                each PATH given
//!
//!   cargo run --example quota_dump -- IMAGE [PATH...]

use fs_ntfs::block_io::PathIo;
use fs_ntfs::quota;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let image = args.next().ok_or("usage: quota_dump IMAGE [PATH...]")?;
    let mut io = PathIo::open_ro(Path::new(&image))?;
    let tables = quota::tables_io(&mut io)?;
    for owner in &tables.owners {
        println!(
            "owner\t{}\t{}",
            quota::sid_string(&owner.sid)?,
            owner.owner_id
        );
    }
    for (id, record) in &tables.records {
        let sid = match &record.sid {
            Some(sid) => quota::sid_string(sid)?,
            None => "-".to_string(),
        };
        println!(
            "quota\t{id}\t0x{:x}\t{}\t{sid}",
            record.flags, record.bytes_used
        );
    }
    for path in args {
        match quota::charge_io(&mut io, &path)? {
            Some((owner, charged)) => println!("file\t{path}\t{owner}\t{charged}"),
            None => println!("file\t{path}\t-\t-"),
        }
    }
    Ok(())
}
