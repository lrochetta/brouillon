use loro::{LoroDoc, LoroText};

/// Collaborative CRDT session wrapper utilizing Loro.
/// Manages a single `LoroDoc` and syncs text content via a LoroText container.
#[derive(Clone)]
pub struct CrdtSession {
    doc: LoroDoc,
    text_container: LoroText,
}

impl std::fmt::Debug for CrdtSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CrdtSession {{ text: {:?} }}", self.text())
    }
}


impl CrdtSession {
    /// Create a new collaborative document session
    pub fn new() -> Self {
        let doc = LoroDoc::new();
        // Get or create the LoroText container named "text"
        let text_container = doc.get_text("text");
        Self { doc, text_container }
    }

    /// Retrieve the current text content of the CRDT document
    pub fn text(&self) -> String {
        self.text_container.to_string()
    }

    /// Insert text at a character position
    pub fn insert(&self, char_pos: usize, text: &str) -> Result<(), loro::LoroError> {
        self.text_container.insert(char_pos, text)
    }

    /// Delete a range of characters
    pub fn delete(&self, char_pos: usize, len: usize) -> Result<(), loro::LoroError> {
        self.text_container.delete(char_pos, len)
    }

    /// Export the entire document snapshot as a binary blob
    pub fn export_snapshot(&self) -> Vec<u8> {
        self.doc.export(loro::ExportMode::Snapshot).unwrap_or_default()
    }

    /// Export incremental updates since the given version vector
    pub fn export_updates(&self) -> Vec<u8> {
        self.doc.export(loro::ExportMode::Updates { from: &Default::default() }).unwrap_or_default()
    }

    /// Import updates or a snapshot into the document, reconciling conflicts
    pub fn import_updates(&self, bytes: &[u8]) -> Result<(), loro::LoroError> {
        self.doc.import(bytes)?;
        Ok(())
    }

    /// Get the underlying LoroDoc reference
    pub fn doc(&self) -> &LoroDoc {
        &self.doc
    }
}

impl Default for CrdtSession {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crdt_local_insert_delete() {
        let session = CrdtSession::new();
        assert_eq!(session.text(), "");

        session.insert(0, "hello").unwrap();
        assert_eq!(session.text(), "hello");

        session.insert(5, " world").unwrap();
        assert_eq!(session.text(), "hello world");

        session.delete(5, 6).unwrap();
        assert_eq!(session.text(), "hello");
    }

    #[test]
    fn test_crdt_collaboration_sync() {
        let client_a = CrdtSession::new();
        let client_b = CrdtSession::new();

        // Client A makes edits
        client_a.insert(0, "Hello from A").unwrap();

        // Export A's updates and import to B
        let updates = client_a.export_snapshot();
        client_b.import_updates(&updates).unwrap();
        assert_eq!(client_b.text(), "Hello from A");

        // Concurrent edits
        client_a.insert(12, "!").unwrap();
        client_b.insert(0, "A: ").unwrap();

        // Sync A -> B
        let updates_a = client_a.export_snapshot();
        client_b.import_updates(&updates_a).unwrap();

        // Sync B -> A
        let updates_b = client_b.export_snapshot();
        client_a.import_updates(&updates_b).unwrap();

        // Verify divergence is resolved (both show same text)
        assert_eq!(client_a.text(), client_b.text());
    }
}
