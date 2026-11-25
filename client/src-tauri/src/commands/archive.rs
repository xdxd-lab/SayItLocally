use std::io::{Seek, Write};
use zip::{write::SimpleFileOptions, CompressionMethod, DateTime, ZipWriter};

// [自訂] SC-51：延續既有檔案權限，固定時間，已知大檔使用 ZIP64。
pub(super) fn file_options(method: CompressionMethod, size: u64) -> SimpleFileOptions {
    SimpleFileOptions::DEFAULT
        .compression_method(method)
        .unix_permissions(0o644)
        .last_modified_time(DateTime::DEFAULT)
        .large_file(size >= u32::MAX as u64)
}

pub(super) fn write_deflated_entry<W: Write + Seek>(
    archive: &mut ZipWriter<W>,
    name: &str,
    bytes: &[u8],
) -> Result<(), String> {
    archive
        .start_file(name, file_options(CompressionMethod::Deflated, bytes.len() as u64))
        .map_err(|error| error.to_string())?;
    archive.write_all(bytes).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read};

    #[test]
    fn written_entries_keep_names_methods_modes_and_fixed_timestamp() {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        write_deflated_entry(&mut writer, "備註.json", b"{}").unwrap();
        writer.start_file("audio/test.wav", file_options(CompressionMethod::Stored, 4)).unwrap();
        writer.write_all(b"test").unwrap();
        let mut archive = zip::ZipArchive::new(writer.finish().unwrap()).unwrap();
        archive.validate_header_consistency().unwrap();
        for (name, method, expected) in [
            ("備註.json", CompressionMethod::Deflated, b"{}".as_slice()),
            ("audio/test.wav", CompressionMethod::Stored, b"test".as_slice()),
        ] {
            let mut entry = archive.by_name(name).unwrap();
            assert_eq!(entry.compression(), method);
            assert_eq!(entry.unix_mode(), Some(0o100644));
            assert_eq!(entry.last_modified(), Some(DateTime::DEFAULT));
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            assert_eq!(data, expected);
        }
    }
}
