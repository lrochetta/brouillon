//! Frame layout: `u32 LE length | u8 kind | body`, `length` = 1 + body length.
//!
//! * kind 1 CONTROL: body = UTF-8 JSON.
//! * kind 2 DATA: body = `u64 LE request id | u64 LE offset | bytes` (bytes <= MAX_CHUNK)
//!
//! Copied from the remote helper protocol; to be extracted into a shared zero-dependency crate..
//!
//! The length is checked against the cap before any allocation.

use crate::limits::{MAX_CHUNK, MAX_FRAME};
use std::io::{self, Read, Write};

pub const KIND_CONTROL: u8 = 1;
pub const KIND_DATA: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Control(Vec<u8>),
    Data { id: u64, offset: u64, bytes: Vec<u8> },
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("frame of {0} bytes exceeds the cap")]
    TooLarge(usize),
    #[error("empty frame")]
    Empty,
    #[error("unknown frame kind {0}")]
    UnknownKind(u8),
    #[error("stream ended inside a frame")]
    Truncated,
    #[error("data frame shorter than its 16-byte header")]
    ShortData,
    #[error("data chunk larger than the cap")]
    ChunkTooLarge,
    #[error("invalid json: {0}")]
    Json(String),
}

pub fn write_frame<W: Write>(w: &mut W, f: &Frame) -> io::Result<()> {
    let (kind, body_len) = match f {
        Frame::Control(b) => (KIND_CONTROL, b.len()),
        Frame::Data { bytes, .. } => (KIND_DATA, 16 + bytes.len()),
    };
    if 1 + body_len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too large"));
    }
    let mut buf = Vec::with_capacity(5 + body_len);
    buf.extend_from_slice(&((1 + body_len) as u32).to_le_bytes());
    buf.push(kind);
    match f {
        Frame::Control(b) => buf.extend_from_slice(b),
        Frame::Data { id, offset, bytes } => {
            buf.extend_from_slice(&id.to_le_bytes());
            buf.extend_from_slice(&offset.to_le_bytes());
            buf.extend_from_slice(bytes);
        }
    }
    w.write_all(&buf)?;
    w.flush()
}

pub fn write_control<W: Write, T: serde::Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_frame(w, &Frame::Control(body))
}

pub fn write_data<W: Write>(w: &mut W, id: u64, offset: u64, bytes: &[u8]) -> io::Result<()> {
    write_frame(w, &Frame::Data { id, offset, bytes: bytes.to_vec() })
}

/// Read one frame. `Ok(None)` = clean end of stream exactly at a frame boundary.
pub fn read_frame<R: Read>(r: &mut R) -> Result<Option<Frame>, FrameError> {
    let mut len4 = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len4[got..]) {
            Ok(0) => {
                return if got == 0 { Ok(None) } else { Err(FrameError::Truncated) };
            }
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let len = u32::from_le_bytes(len4) as usize;
    if len == 0 {
        return Err(FrameError::Empty);
    }
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            FrameError::Truncated
        } else {
            FrameError::Io(e)
        }
    })?;
    let kind = body[0];
    match kind {
        KIND_CONTROL => Ok(Some(Frame::Control(body[1..].to_vec()))),
        KIND_DATA => {
            if body.len() < 17 {
                return Err(FrameError::ShortData);
            }
            if body.len() - 17 > MAX_CHUNK {
                return Err(FrameError::ChunkTooLarge);
            }
            let id = u64::from_le_bytes(body[1..9].try_into().unwrap());
            let offset = u64::from_le_bytes(body[9..17].try_into().unwrap());
            Ok(Some(Frame::Data { id, offset, bytes: body[17..].to_vec() }))
        }
        k => Err(FrameError::UnknownKind(k)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn rt(f: Frame) -> Frame {
        let mut v = Vec::new();
        write_frame(&mut v, &f).unwrap();
        read_frame(&mut Cursor::new(v)).unwrap().unwrap()
    }

    #[test]
    fn round_trips() {
        let c = Frame::Control(br#"{"id":1}"#.to_vec());
        assert_eq!(rt(c.clone()), c);
        let d = Frame::Data { id: 7, offset: 1 << 40, bytes: vec![0, 1, 2, 255] };
        assert_eq!(rt(d.clone()), d);
        let e = Frame::Data { id: 1, offset: 0, bytes: vec![] };
        assert_eq!(rt(e.clone()), e);
        let big = Frame::Data { id: 1, offset: 0, bytes: vec![9; MAX_CHUNK] };
        assert_eq!(rt(big.clone()), big);
    }

    #[test]
    fn clean_eof_and_truncation() {
        assert!(read_frame(&mut Cursor::new(Vec::new())).unwrap().is_none());
        assert!(matches!(read_frame(&mut Cursor::new(vec![1, 0])), Err(FrameError::Truncated)));
        // declares 10 bytes, provides 3
        let v = vec![10, 0, 0, 0, 1, b'{', b'}'];
        assert!(matches!(read_frame(&mut Cursor::new(v)), Err(FrameError::Truncated)));
    }

    #[test]
    fn rejects_hostile_lengths_without_allocating() {
        let v = (u32::MAX).to_le_bytes().to_vec();
        assert!(matches!(read_frame(&mut Cursor::new(v)), Err(FrameError::TooLarge(_))));
        let v = ((MAX_FRAME + 1) as u32).to_le_bytes().to_vec();
        assert!(matches!(read_frame(&mut Cursor::new(v)), Err(FrameError::TooLarge(_))));
        assert!(matches!(read_frame(&mut Cursor::new(vec![0, 0, 0, 0])), Err(FrameError::Empty)));
    }

    #[test]
    fn rejects_bad_kind_and_short_data() {
        assert!(matches!(read_frame(&mut Cursor::new(vec![1, 0, 0, 0, 9])), Err(FrameError::UnknownKind(9))));
        let mut v = vec![5, 0, 0, 0, KIND_DATA];
        v.extend_from_slice(&[0; 4]);
        assert!(matches!(read_frame(&mut Cursor::new(v)), Err(FrameError::ShortData)));
    }

    #[test]
    fn oversize_chunk_rejected() {
        let mut body = vec![KIND_DATA];
        body.extend_from_slice(&[0; 16]);
        body.extend(std::iter::repeat(0u8).take(MAX_CHUNK + 1));
        let mut v = (body.len() as u32).to_le_bytes().to_vec();
        v.extend(body);
        assert!(matches!(read_frame(&mut Cursor::new(v)), Err(FrameError::ChunkTooLarge)));
    }

    #[test]
    fn writer_refuses_oversize() {
        let mut v = Vec::new();
        let f = Frame::Control(vec![b' '; MAX_FRAME]);
        assert!(write_frame(&mut v, &f).is_err());
    }

    #[test]
    fn several_frames_in_a_row() {
        let mut v = Vec::new();
        write_control(&mut v, &serde_json::json!({"a": 1})).unwrap();
        write_data(&mut v, 3, 4, b"xyz").unwrap();
        let mut c = Cursor::new(v);
        assert!(matches!(read_frame(&mut c).unwrap(), Some(Frame::Control(_))));
        assert!(matches!(read_frame(&mut c).unwrap(), Some(Frame::Data { id: 3, offset: 4, .. })));
        assert!(read_frame(&mut c).unwrap().is_none());
    }
}
