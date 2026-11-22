use std::{
    io::{self, BufWriter},
    path::Path,
};

use atomic_write_file::AtomicWriteFile;
use serde::Serialize;

/// Writes a JSON document atomically so a crash cannot leave it truncated.
pub fn write_json<T: Serialize>(path: &Path, data: &T) -> io::Result<()> {
    let mut writer = BufWriter::new(AtomicWriteFile::options().open(path)?);
    serde_json::to_writer_pretty(&mut writer, data)?;
    writer.into_inner().map_err(|e| e.into_error())?.commit()
}
