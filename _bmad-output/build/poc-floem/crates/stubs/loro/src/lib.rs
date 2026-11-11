use std::sync::{Arc, Mutex};
use std::fmt;

#[derive(Clone, Debug)]
pub struct LoroText {
    text: Arc<Mutex<String>>,
}

impl LoroText {
    pub fn to_string(&self) -> String {
        self.text.lock().unwrap().clone()
    }

    pub fn insert(&self, pos: usize, s: &str) -> Result<(), LoroError> {
        let mut guard = self.text.lock().unwrap();
        let mut char_idx = 0;
        let mut byte_idx = 0;
        for c in guard.chars() {
            if char_idx == pos {
                break;
            }
            char_idx += 1;
            byte_idx += c.len_utf8();
        }
        if char_idx < pos {
            byte_idx = guard.len();
        }
        guard.insert_str(byte_idx, s);
        Ok(())
    }

    pub fn delete(&self, pos: usize, len: usize) -> Result<(), LoroError> {
        let mut guard = self.text.lock().unwrap();
        let mut char_idx = 0;
        let mut start_byte = None;
        let mut end_byte = None;
        let mut byte_idx = 0;
        for c in guard.chars() {
            if char_idx == pos {
                start_byte = Some(byte_idx);
            }
            if char_idx == pos + len {
                end_byte = Some(byte_idx);
                break;
            }
            char_idx += 1;
            byte_idx += c.len_utf8();
        }
        let start = start_byte.unwrap_or(byte_idx);
        let end = end_byte.unwrap_or(guard.len());
        if start < end {
            guard.drain(start..end);
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct LoroDoc {
    text: LoroText,
}

impl LoroDoc {
    pub fn new() -> Self {
        Self {
            text: LoroText {
                text: Arc::new(Mutex::new(String::new())),
            },
        }
    }

    pub fn get_text(&self, _name: &str) -> LoroText {
        self.text.clone()
    }

    pub fn export(&self, _mode: ExportMode) -> Result<Vec<u8>, LoroError> {
        Ok(self.text.to_string().into_bytes())
    }

    pub fn import(&self, bytes: &[u8]) -> Result<(), LoroError> {
        if let Ok(s) = String::from_utf8(bytes.to_vec()) {
            let mut guard = self.text.text.lock().unwrap();
            *guard = s;
        }
        Ok(())
    }
}

impl Default for LoroDoc {
    fn default() -> Self {
        Self::new()
    }
}

pub enum ExportMode<'a> {
    Snapshot,
    Updates { from: &'a () },
}

#[derive(Debug)]
pub struct LoroError;

impl fmt::Display for LoroError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LoroError")
    }
}

impl std::error::Error for LoroError {}

#[derive(Clone, Debug)]
pub enum LoroValue {
    Null,
}
