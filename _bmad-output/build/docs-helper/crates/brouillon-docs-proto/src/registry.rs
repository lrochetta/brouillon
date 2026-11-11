//! Operation registry: one description of every operation, shared by the UI, the command palette,
//! the CLI and the MCP projection, so none of them re-declares names or parameters.
//!
//! `status` tells what this helper build can do; a `planned` operation is declared (so surfaces can
//! be generated and reviewed) but answers `unsupported`.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamType {
    String,
    Integer,
    Boolean,
    Object,
    Array,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Param {
    pub name: &'static str,
    #[serde(rename = "type")]
    pub ty: ParamType,
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Implemented,
    Planned,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Operation {
    pub name: &'static str,
    pub summary: &'static str,
    pub params: &'static [Param],
    /// Changes the document or discards data; surfaces must confirm or make it undoable.
    pub destructive: bool,
    pub status: Status,
}

use ParamType::{Array as A, Boolean as B, Integer as I, Object as O, String as S};

const fn p(name: &'static str, ty: ParamType, required: bool) -> Param {
    Param { name, ty, required }
}

const DOC: Param = p("doc_id", S, true);

const fn op(name: &'static str, summary: &'static str, params: &'static [Param], destructive: bool, status: Status) -> Operation {
    Operation { name, summary, params, destructive, status }
}

use Status::{Implemented as Ok_, Planned as Plan};

pub static OPERATIONS: &[Operation] = &[
    op("hello", "Negotiate protocol version, limits and features", &[p("proto", I, true), p("client", S, false)], false, Ok_),
    op("ping", "Liveness check", &[], false, Ok_),
    op("cancel", "Cancel an in-flight request", &[p("id", I, true)], false, Ok_),
    op("shutdown", "Finish in-flight work and exit", &[], false, Ok_),
    op("operations", "List the operation registry", &[], false, Ok_),
    op("debug.spin", "Cancellable test operation emitting progress", &[p("steps", I, true), p("step_ms", I, false)], false, Ok_),
    op("open", "Open a document from bytes streamed by the editor", &[DOC, p("kind_hint", S, false), p("size", I, true), p("ext", S, false), p("options", O, false)], false, Plan),
    op("close", "Free a document", &[DOC], false, Plan),
    op("get_outline", "Block index chunk", &[DOC, p("from", I, true), p("count", I, true), p("rev", I, false)], false, Plan),
    op("get_blocks", "Block window", &[DOC, p("from", I, true), p("count", I, true), p("rev", I, false)], false, Plan),
    op("apply_ops", "Apply editor ops to the package", &[DOC, p("base_rev", I, true), p("ops", A, true), p("seq", I, false)], true, Plan),
    op("render", "Render a page tile", &[DOC, p("page", I, true), p("scale", I, false), p("budget_ms", I, false)], false, Plan),
    op("hit_test", "Position to glyph", &[DOC, p("page", I, true), p("x", I, true), p("y", I, true)], false, Plan),
    op("text_layer", "Glyph quads and runs of a page", &[DOC, p("page", I, true)], false, Plan),
    op("layout", "Block heights for flow documents", &[DOC, p("width_px", I, true), p("from", I, true), p("count", I, true)], false, Plan),
    op("search", "Search with a cursor", &[DOC, p("query", S, true), p("cursor", S, false), p("limit", I, false)], false, Plan),
    op("export", "Serialize the document to bytes (the editor writes the file)", &[DOC, p("format", S, true), p("options", O, false)], false, Plan),
    op("annotate", "Add a PDF annotation", &[DOC, p("op", O, true)], true, Plan),
    op("metadata_get", "Itemised metadata report", &[DOC, p("classes", A, false)], false, Plan),
    op("metadata_strip", "Remove metadata classes", &[DOC, p("classes", A, true)], true, Plan),
    op("extract_text", "Plain text of a document", &[DOC, p("sanitize", B, false)], false, Plan),
];

pub fn find(name: &str) -> Option<&'static Operation> {
    OPERATIONS.iter().find(|o| o.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn names_are_unique_and_lookup_works() {
        let names: HashSet<_> = OPERATIONS.iter().map(|o| o.name).collect();
        assert_eq!(names.len(), OPERATIONS.len());
        assert!(find("apply_ops").unwrap().destructive);
        assert!(find("nope").is_none());
    }

    #[test]
    fn serializes_for_surfaces() {
        let v = serde_json::to_value(OPERATIONS).unwrap();
        assert_eq!(v[0]["name"], "hello");
        assert_eq!(v[0]["params"][0]["type"], "integer");
        assert_eq!(v[0]["status"], "implemented");
    }
}
