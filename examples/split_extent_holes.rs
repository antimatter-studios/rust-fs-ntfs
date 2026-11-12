use fs_ntfs::block_io::BlockIo;
use fs_ntfs::{fsck, write};
use std::io::Write;

struct FixtureIo {
    bytes: Vec<u8>,
}

impl BlockIo for FixtureIo {
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), String> {
        let start = usize::try_from(offset).map_err(|error| error.to_string())?;
        let end = start.checked_add(buffer.len()).ok_or("read overflows")?;
        buffer.copy_from_slice(self.bytes.get(start..end).ok_or("read past fixture")?);
        Ok(())
    }

    fn write_all_at(&mut self, offset: u64, buffer: &[u8]) -> Result<(), String> {
        let start = usize::try_from(offset).map_err(|error| error.to_string())?;
        let end = start.checked_add(buffer.len()).ok_or("write overflows")?;
        self.bytes
            .get_mut(start..end)
            .ok_or("write past fixture")?
            .copy_from_slice(buffer);
        Ok(())
    }

    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [image, action, holes, cluster] = args.as_slice() else {
        return Err("usage: split_extent_holes IMAGE create|remove HOLES CLUSTER".into());
    };
    let holes: usize = holes.parse()?;
    let cluster: usize = cluster.parse()?;
    let count = holes.checked_mul(2).ok_or("hole count overflows")?;
    if holes == 0 || cluster == 0 || !matches!(action.as_str(), "create" | "remove") {
        return Err(
            "a create or remove action and positive hole and cluster counts are required".into(),
        );
    }
    let mut io = FixtureIo {
        bytes: std::fs::read(image)?,
    };
    fsck::prepare_rw_mount_io(&mut io)?;
    fsck::retire_usn_journal_io(&mut io)?;
    let per_dir = 500;
    if action == "create" {
        write::mkdir_io(&mut io, "/", "c")?;
        for directory in 0..=count / per_dir {
            write::mkdir_io(&mut io, "/c", &directory.to_string())?;
        }
        let payload = vec![0xc5; cluster];
        for index in 1..=count {
            let parent = format!("/c/{}", index / per_dir);
            let name = index.to_string();
            write::create_file_io(&mut io, &parent, &name)?;
            write::replace_file_contents_io(&mut io, &format!("{parent}/{name}"), &payload)?;
        }
    } else {
        for index in (2..=count).step_by(2) {
            write::unlink_io(&mut io, &format!("/c/{}/{index}", index / per_dir))?;
        }
    }
    let mut file = std::fs::OpenOptions::new().write(true).open(image)?;
    file.write_all(&io.bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_io_preserves_bytes_and_refuses_out_of_bounds_access() {
        let mut io = FixtureIo { bytes: vec![0; 8] };
        io.write_all_at(3, &[1, 2, 3]).unwrap();
        let mut buffer = [0; 8];
        io.read_exact_at(0, &mut buffer).unwrap();
        assert_eq!(buffer, [0, 0, 0, 1, 2, 3, 0, 0]);
        assert_eq!(io.size(), 8);
        assert!(io.read_exact_at(8, &mut [0]).is_err());
        assert!(io.write_all_at(8, &[1]).is_err());
        assert!(io.read_exact_at(u64::MAX, &mut [0]).is_err());
        assert!(io.write_all_at(u64::MAX, &[1]).is_err());
        assert_eq!(io.bytes, buffer);
    }
}
