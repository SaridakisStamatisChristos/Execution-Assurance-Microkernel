use std::{
    fs::{File, OpenOptions},
    io::{self, Read},
    path::Path,
};

/// Open an append-only JSONL file using newline as the durable frame marker.
///
/// A crash may leave bytes after the last newline even when earlier frames were
/// durably committed. Those bytes are an uncommitted torn tail and are removed
/// before the file is reopened for append. Newline-terminated frames are never
/// silently discarded; their JSON validity remains the reader's responsibility,
/// so committed corruption still fails closed.
pub(crate) fn open_durable_jsonl(path: &Path) -> io::Result<File> {
    let existed = path.exists();
    if existed {
        repair_torn_tail(path)?;
    }

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)?;

    if !existed {
        // Make the newly-created inode and directory entry durable before the
        // caller relies on subsequent fsync-backed frames.
        file.sync_all()?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
    }

    Ok(file)
}

fn repair_torn_tail(path: &Path) -> io::Result<()> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;

    if bytes.is_empty() || bytes.last() == Some(&b'\n') {
        return Ok(());
    }

    let committed_len = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    file.set_len(u64::try_from(committed_len).map_err(io::Error::other)?)?;
    file.sync_all()
}
