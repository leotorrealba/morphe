//! Source-file plumbing for `include <file>` and `use <file>`.
//!
//! Upstream OpenSCAD resolves both through `find_valid_path`
//! (`src/core/parsersettings.cc`): an absolute path is taken as is; a
//! relative one is tried against the directory of the file that names it,
//! then against each library directory (`OPENSCADPATH`, then the user and
//! built-in library folders). `include` is handled by the lexer
//! (`lexer.l includefile`), which splices the file's tokens in place, so it
//! behaves exactly like textual insertion; `use` is recorded per file
//! (`SourceFile::registerUse` / `usedlibs`) and only the used file's module
//! and function definitions become visible (`FileContext::lookup_local_*`).
//!
//! This module holds the evaluator-independent parts: the library-path
//! list, a size-capped regular-file reader, and the span map that lets one
//! evaluation carry ASTs from several files (each file is parsed at its own
//! base offset via [`morphe_lang::parser::parse_program_at`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use morphe_lang::lexer::{line_col, Span};

/// Largest `.scad` file `include`/`use` will read: 16 MiB. A real library
/// (BOSL2's biggest file is well under 1 MiB) never comes close; the cap
/// stops a hostile path from making the evaluator read an unbounded file.
pub const MAX_INCLUDE_BYTES: u64 = 16 * 1024 * 1024;

/// Maximum nesting of `include`/`use` loads (a file including a file
/// including ...). Upstream has no explicit limit; a cycle is caught
/// separately, so this only bounds pathological-but-acyclic chains.
pub const MAX_INCLUDE_DEPTH: usize = 64;

/// First byte offset handed to a loaded (non-main) file. Spans at or above
/// it belong to included/used files; spans below it to the main source,
/// which can never be this large (the reader caps files far below it).
pub(crate) const LOADED_SPAN_BASE: usize = 1 << (usize::BITS - 2);

/// The library search path: the entries of `OPENSCADPATH`, split with the
/// platform's path-list separator (`:` on Unix, `;` on Windows), matching
/// upstream `parsersettings.cc` `add_librarydir` over `OPENSCADPATH`.
/// Empty entries are skipped. The user and built-in library folders
/// upstream also searches are deliberately not consulted.
#[must_use]
pub fn library_paths_from_env() -> Vec<PathBuf> {
    match std::env::var_os("OPENSCADPATH") {
        Some(value) => std::env::split_paths(&value)
            .filter(|p| !p.as_os_str().is_empty())
            .collect(),
        None => Vec::new(),
    }
}

/// Read a regular file as UTF-8 text, refusing anything over `max` bytes.
///
/// The same discipline as the kernel's capped reader (which this crate
/// cannot depend on): the path must stat as a regular file before it is
/// opened (so a FIFO or device never blocks or streams), the opened handle
/// must again be a regular file, and the read itself is bounded so a file
/// that grows past the stat still fails. Messages name the path but never
/// echo file content.
pub(crate) fn read_capped_text(path: &Path, max: u64) -> Result<String, String> {
    use std::io::Read;
    let not_regular = || format!("`{}` is not a regular file", path.display());
    let stat =
        std::fs::metadata(path).map_err(|e| format!("cannot stat `{}`: {e}", path.display()))?;
    if !stat.is_file() {
        return Err(not_regular());
    }
    let file =
        std::fs::File::open(path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    let meta = file
        .metadata()
        .map_err(|e| format!("cannot stat `{}`: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(not_regular());
    }
    if meta.len() > max {
        return Err(format!(
            "`{}` is {} bytes (cap {max})",
            path.display(),
            meta.len()
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    file.take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    if bytes.len() as u64 > max {
        return Err(format!(
            "`{}` exceeds {max} bytes (cap {max})",
            path.display()
        ));
    }
    String::from_utf8(bytes).map_err(|_| format!("`{}` is not valid UTF-8", path.display()))
}

/// Where an [`EvalError`](crate::eval::EvalError) raised inside an
/// included or used file actually happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorOrigin {
    /// Canonical path of the file the error is in.
    pub file: PathBuf,
    /// The error's span, as byte offsets into that file.
    pub span: Span,
    /// 1-based line in that file.
    pub line: usize,
    /// 1-based column in that file.
    pub col: usize,
}

/// One loaded (included or used) source file.
#[derive(Debug)]
pub(crate) struct LoadedFile {
    /// Offset its spans were parsed at.
    pub base: usize,
    /// Canonical path.
    pub path: PathBuf,
    pub source: Arc<str>,
    /// Span, in the main source, of the top-level `include`/`use` that
    /// (directly or transitively) brought this file in.
    pub entry_span: Span,
}

/// Every loaded file, in increasing [`LoadedFile::base`] order.
#[derive(Debug, Default)]
pub(crate) struct SourceMap {
    files: Vec<LoadedFile>,
}

impl SourceMap {
    /// Register a file's text and return the base offset to parse it at.
    pub fn add(&mut self, path: PathBuf, source: Arc<str>, entry_span: Span) -> usize {
        let base = self
            .files
            .last()
            .map_or(LOADED_SPAN_BASE, |f| f.base + f.source.len() + 1);
        self.files.push(LoadedFile {
            base,
            path,
            source,
            entry_span,
        });
        base
    }

    /// The loaded file `span` lies in, or `None` for the main source.
    pub fn locate(&self, span: Span) -> Option<&LoadedFile> {
        if span.start < LOADED_SPAN_BASE {
            return None;
        }
        let index = self.files.partition_point(|f| f.base <= span.start);
        index.checked_sub(1).map(|i| &self.files[i])
    }

    /// The source text covered by a loaded-file span.
    pub fn text(&self, span: Span) -> Option<&str> {
        let file = self.locate(span)?;
        file.source
            .get(span.start - file.base..span.end.checked_sub(file.base)?)
    }

    /// Translate a loaded-file span into an [`ErrorOrigin`] plus the main
    /// source span to report it at (the entry `include`/`use`).
    pub fn origin(&self, span: Span) -> Option<(ErrorOrigin, Span)> {
        let file = self.locate(span)?;
        let local = Span {
            start: span.start - file.base,
            end: span
                .end
                .saturating_sub(file.base)
                .max(span.start - file.base),
        };
        let (line, col) = line_col(&file.source, local.start);
        Some((
            ErrorOrigin {
                file: file.path.clone(),
                span: local,
                line,
                col,
            },
            file.entry_span,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_map_locates_files_by_span() {
        let mut map = SourceMap::default();
        let entry = Span { start: 3, end: 9 };
        let a = map.add(PathBuf::from("/a.scad"), "x = 1;\ny = ;".into(), entry);
        let b = map.add(PathBuf::from("/b.scad"), "z".into(), entry);
        assert_eq!(a, LOADED_SPAN_BASE);
        assert!(b > a + 12);
        assert!(map.locate(Span { start: 5, end: 6 }).is_none());
        let (origin, at) = map
            .origin(Span {
                start: a + 11,
                end: a + 12,
            })
            .unwrap();
        assert_eq!(origin.file, PathBuf::from("/a.scad"));
        assert_eq!((origin.line, origin.col), (2, 5));
        assert_eq!(at, entry);
        assert_eq!(
            map.locate(Span {
                start: b,
                end: b + 1
            })
            .unwrap()
            .path,
            PathBuf::from("/b.scad")
        );
        assert_eq!(
            map.text(Span {
                start: a,
                end: a + 5
            }),
            Some("x = 1")
        );
    }

    #[test]
    fn capped_reader_refuses_directories_and_oversize() {
        let dir = std::env::temp_dir().join(format!("morphe-inc-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read_capped_text(&dir, 100)
            .unwrap_err()
            .contains("not a regular file"));
        let file = dir.join("big.scad");
        std::fs::write(&file, "cube(1);").unwrap();
        assert!(read_capped_text(&file, 4).unwrap_err().contains("cap 4"));
        assert_eq!(read_capped_text(&file, 100).unwrap(), "cube(1);");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
