use std::io::{self, BufRead, Read, Write};

use crate::{Store, protocol::Session};

const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Serves newline-delimited MCP messages until EOF or a transport error.
/// A message may contain at most 1 MiB before its newline. Oversized input
/// terminates the connection without reading the remainder of the line.
pub fn serve<R: BufRead, W: Write>(store: Store, mut reader: R, mut writer: W) -> io::Result<()> {
    let mut session = Session::new(store);
    loop {
        let mut line = Vec::new();
        let read = reader.by_ref().take(1_048_577).read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(());
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.len() > MAX_LINE_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "MCP input line exceeds 1 MiB"));
        }
        if let Some(response) = session.handle(&line) {
            serde_json::to_writer(&mut writer, &response)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
    }
}
