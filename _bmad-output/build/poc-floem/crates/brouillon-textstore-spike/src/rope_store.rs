//! Baseline: the `crop::Rope` approach the engine uses today, behind the same trait (streamed 4 MB load like
//! `file_io::read_utf8_rope`, streamed atomic save like `Buffer::save_to_path` after the memory pass).
use crate::store::{StoreError, TextStore};
use std::io::{self, Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

struct Rec {
    pos: usize,
    removed: String,
    inserted: String,
}

pub struct RopeStore {
    rope: crop::Rope,
    undo: Vec<Vec<Rec>>,
    redo: Vec<Vec<Rec>>,
    open: Option<Vec<Rec>>,
    version: u64,
    saved_version: u64,
    path: PathBuf,
}

const CHUNK: usize = 4 << 20;

impl RopeStore {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut file = std::fs::File::open(path)?;
        let mut builder = crop::RopeBuilder::new();
        let mut buf = vec![0u8; CHUNK];
        let mut carry = 0usize;
        loop {
            let mut filled = carry;
            let mut eof = false;
            while filled < buf.len() {
                let n = file.read(&mut buf[filled..])?;
                if n == 0 {
                    eof = true;
                    break;
                }
                filled += n;
            }
            let valid = match std::str::from_utf8(&buf[..filled]) {
                Ok(_) => filled,
                Err(e) if e.error_len().is_none() && !eof => e.valid_up_to(),
                Err(_) => return Err(io::Error::new(io::ErrorKind::InvalidData, "not valid UTF-8")),
            };
            let mut end = valid;
            if !eof && end > 0 && buf[end - 1] == b'\r' {
                end -= 1; // never split a CRLF between two builder appends
            }
            if end > 0 {
                builder.append(std::str::from_utf8(&buf[..end]).unwrap());
            }
            buf.copy_within(end..filled, 0);
            carry = filled - end;
            if eof {
                break;
            }
        }
        Ok(Self { rope: builder.build(), undo: vec![], redo: vec![], open: None, version: 0, saved_version: 0, path: path.to_path_buf() })
    }
    pub fn rope(&self) -> &crop::Rope {
        &self.rope
    }
    /// True when the text is empty or ends with a line feed (crop has no trailing empty line, the trait has).
    fn extra_line(&self) -> bool {
        self.rope.byte_len() == 0 || self.rope.chunks().next_back().map_or(true, |c| c.ends_with('\n'))
    }
    fn apply(&mut self, pos: usize, del: usize, text: &str) -> String {
        let removed = if del > 0 { self.rope.byte_slice(pos..pos + del).to_string() } else { String::new() };
        if del > 0 {
            self.rope.delete(pos..pos + del);
        }
        if !text.is_empty() {
            self.rope.insert(pos, text);
        }
        self.version += 1;
        removed
    }
}

impl TextStore for RopeStore {
    fn byte_len(&self) -> usize {
        self.rope.byte_len()
    }
    // NOTE crop counts lines as terminated records (a text ending in a line feed has no extra empty line, the
    // empty text has 0 lines); the trait uses the editor convention (line feeds + 1). The adapter bridges the +1.
    fn line_count(&self) -> usize {
        self.rope.line_len() + self.extra_line() as usize
    }
    fn byte_of_line(&self, line: usize) -> Option<usize> {
        let n = self.rope.line_len();
        if line < n {
            Some(self.rope.byte_of_line(line))
        } else if line == n && self.extra_line() {
            Some(self.rope.byte_len())
        } else {
            None
        }
    }
    fn line_of_byte(&self, byte: usize) -> usize {
        if byte == self.rope.byte_len() && self.extra_line() {
            self.rope.line_len()
        } else {
            self.rope.line_of_byte(byte)
        }
    }
    fn is_char_boundary(&self, byte: usize) -> bool {
        self.rope.is_char_boundary(byte)
    }
    fn visit(&self, r: Range<usize>, f: &mut dyn FnMut(&[u8]) -> bool) {
        for c in self.rope.byte_slice(r).chunks() {
            if !f(c.as_bytes()) {
                return;
            }
        }
    }
    fn replace_range(&mut self, r: Range<usize>, text: &str) -> Result<(), StoreError> {
        if r.end > self.byte_len() || r.start > r.end {
            return Err(StoreError::OutOfRange);
        }
        for p in [r.start, r.end] {
            if !self.rope.is_char_boundary(p) {
                return Err(StoreError::NotCharBoundary(p));
            }
        }
        let removed = self.apply(r.start, r.len(), text);
        let rec = Rec { pos: r.start, removed, inserted: text.to_string() };
        match self.open.as_mut() {
            Some(g) => g.push(rec),
            None => self.undo.push(vec![rec]),
        }
        self.redo.clear();
        Ok(())
    }
    fn begin_group(&mut self) {
        self.open.get_or_insert_with(Vec::new);
    }
    fn end_group(&mut self) {
        if let Some(g) = self.open.take() {
            if !g.is_empty() {
                self.undo.push(g);
            }
        }
    }
    fn undo(&mut self) -> bool {
        let Some(g) = self.undo.pop() else { return false };
        for rec in g.iter().rev() {
            self.apply(rec.pos, rec.inserted.len(), &rec.removed);
        }
        self.redo.push(g);
        true
    }
    fn redo(&mut self) -> bool {
        let Some(g) = self.redo.pop() else { return false };
        for rec in g.iter() {
            self.apply(rec.pos, rec.removed.len(), &rec.inserted);
        }
        self.undo.push(g);
        true
    }
    fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    fn version(&self) -> u64 {
        self.version
    }
    fn is_dirty(&self) -> bool {
        self.version != self.saved_version
    }
    fn save(&mut self, path: &Path) -> io::Result<()> {
        let tmp = path.with_extension("brouillon-tmp");
        {
            let mut w = io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&tmp)?);
            for c in self.rope.chunks() {
                w.write_all(c.as_bytes())?;
            }
            w.flush()?;
            w.get_ref().sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        self.saved_version = self.version;
        self.path = path.to_path_buf();
        Ok(())
    }
    fn heap_estimate(&self) -> usize {
        // crop does not expose its heap size; the rope is ~3.8% over the text (measured in the memory pass).
        self.rope.byte_len() + self.rope.byte_len() / 26
            + self.undo.iter().chain(self.redo.iter()).flatten().map(|r| r.removed.len() + r.inserted.len() + 48).sum::<usize>()
    }
}
