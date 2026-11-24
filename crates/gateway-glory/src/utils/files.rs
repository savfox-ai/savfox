use glory::render::{FileInfo, FileRead, MAX_FILE_CHUNK_BYTES};

pub(crate) async fn read(
    controller: &sapid::FileController,
    file: &FileInfo,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut offset = 0;
    while offset < file.size {
        let length = (file.size - offset).min(u64::from(MAX_FILE_CHUNK_BYTES)) as u32;
        let (send, receive) = futures::channel::oneshot::channel();
        controller
            .read(
                FileRead {
                    file: file.token,
                    offset,
                    length,
                },
                move |result| {
                    let _ = send.send(result);
                },
            )
            .map_err(|error| error.to_string())?;
        crate::ui::flush();
        let chunk = receive
            .await
            .map_err(|_| "File read cancelled")?
            .map_err(|error| error.to_string())?;
        if chunk.len() != length as usize {
            return Err("Cannot read complete file".into());
        }
        bytes.extend(chunk);
        offset += u64::from(length);
    }
    Ok(bytes)
}
