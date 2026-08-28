//! Blocking LSP frame I/O (`Content-Length` headers + JSON body).

use std::io::{self, BufRead, Write};

pub struct FrameReader<R> {
    reader: R,
}

impl<R: BufRead> FrameReader<R> {
    pub fn new(reader: R) -> Self {
        Self { reader }
    }

    /// Reads one frame body. Returns `Ok(None)` on clean EOF before any byte.
    pub fn read_frame(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut content_length: Option<usize> = None;
        let mut line = Vec::new();
        let mut saw_any = false;
        loop {
            line.clear();
            let n = read_header_line(&mut self.reader, &mut line)?;
            if n == 0 && line.is_empty() && !saw_any {
                return Ok(None); // clean EOF
            }
            saw_any = true;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "eof in headers"));
            }
            if line.is_empty() {
                break; // blank line = end of headers
            }
            let text = String::from_utf8_lossy(&line);
            if let Some(value) = text
                .get(..14)
                .filter(|p| p.eq_ignore_ascii_case("content-length"))
                .and_then(|_| text.get(14..))
                .and_then(|rest| rest.trim_start_matches([':', ' ']).trim().parse().ok())
            {
                content_length = Some(value);
            }
        }
        let len = content_length
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
        let mut body = vec![0u8; len];
        self.reader.read_exact(&mut body)?;
        Ok(Some(body))
    }
}

/// Reads one header line terminated by `\r\n` (terminator excluded from `out`).
/// Returns the number of bytes consumed (0 at EOF).
fn read_header_line<R: BufRead>(reader: &mut R, out: &mut Vec<u8>) -> io::Result<usize> {
    let mut consumed = 0;
    let mut prev = 0u8;
    loop {
        let mut byte = [0u8; 1];
        let n = reader.read(&mut byte)?;
        if n == 0 {
            return Ok(consumed);
        }
        consumed += 1;
        if prev == b'\r' && byte[0] == b'\n' {
            out.pop(); // drop the '\r'
            return Ok(consumed);
        }
        out.push(byte[0]);
        prev = byte[0];
    }
}

/// Writes one framed message.
pub fn write_frame<W: Write>(writer: &mut W, body: &[u8]) -> io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn roundtrip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, br#"{"jsonrpc":"2.0","id":1}"#).unwrap();
        write_frame(&mut buf, b"{}").unwrap();
        let mut reader = FrameReader::new(BufReader::new(&buf[..]));
        assert_eq!(
            reader.read_frame().unwrap().unwrap(),
            br#"{"jsonrpc":"2.0","id":1}"#
        );
        assert_eq!(reader.read_frame().unwrap().unwrap(), b"{}");
        assert!(reader.read_frame().unwrap().is_none());
    }

    #[test]
    fn ignores_other_headers() {
        let data = b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: 2\r\n\r\n{}";
        let mut reader = FrameReader::new(BufReader::new(&data[..]));
        assert_eq!(reader.read_frame().unwrap().unwrap(), b"{}");
    }
}
