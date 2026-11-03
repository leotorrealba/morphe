# Architecture

This document describes how morphe is put together. It's for contributors and
for anyone auditing its security properties. For usage, see the
[README](../README.md).

## The compile pipeline

```
.scad source
   │  morphe-lang      lexer → parser → AST (depth-limited, never panics on input)
   ▼
  AST
   │  morphe-eval      scopes, functions, modules, $-variables, builtins
   ▼
CSG tree (CsgNode)
   │  morphe-kernel    tessellation + Manifold booleans/hull/minkowski/offset…
   ▼
Solid / 2D profile ──► mesh buffer · STL (binary/ASCII) · GeometryStats
```

- **morphe-lang** parses the whole OpenSCAD grammar into an AST. Nesting depth
  and chain length are bounded, so hostile input produces a parse error rather
  than a stack overflow.
- **morphe-eval** evaluates the AST into a typed CSG tree. Semantics follow
  upstream OpenSCAD, and comments cite the upstream source files: dynamic
  scoping of `$` variables, `children()` evaluated in the caller's context,
  last-assignment-wins, `undef` propagation with reasons. Evaluation runs on a
  dedicated large-stack thread. Recursion depth, loop sizes and builtin
  allocations (for example `rands()`) are capped.
- **morphe-kernel** resolves the CSG tree with [Manifold]. It turns primitives
  into meshes (`$fn`/`$fa`/`$fs` are clamped as in upstream, and fragment and
  vertex counts are capped), runs booleans, `hull`, `minkowski`, `offset` and
  `projection`, extrudes 2D profiles, and loads `import()` STL and `surface()`
  heightmaps. File loads are capped in size, accept regular files only, and
  are read through a single handle.

## Desktop app (`apps/desktop`)

The desktop app is a Tauri 2 shell. Its Rust backend is in `src-tauri/` and its
React/TypeScript frontend in `src/`.

- **IPC trust boundary.** The frontend never sends filesystem paths. Open,
  Save As and Export show native dialogs from the Rust side; plain Save writes
  back to the path recorded when the file was opened. `import()`/`surface()`
  resolve relative to that recorded path.
- **Threading.** Commands that compile, show dialogs or touch the filesystem
  are `async` and run on worker threads. Compiles get a large stack. Nothing
  heavy runs on the UI thread, and a panic in the compile pipeline is reported
  as a compile error.
- **Wire format.** `compile_source` returns one binary frame: a little-endian
  `u32` metadata length, then JSON metadata (stats, errors, warnings), then the
  raw mesh buffer. A snapshot test checks that every serialized IPC enum and
  struct has the same shape in Rust and TypeScript
  (`wire-shape.expected.json`).
- **Content security.** The production CSP is `script-src 'self'`. Assistant
  markdown is sanitized with DOMPurify, and links in the chat are inert. The
  only capability granted is `core:default`.

## AI agent (`crates/morphe-ai`)

- **Providers.** A raw-HTTP client (no SDK) supports the Anthropic Messages API
  and any OpenAI-compatible Chat Completions endpoint, with SSE streaming.
  The base URL must be `https`, except for loopback hosts. The API key is
  redacted from `Debug` output and never sent back to the frontend.
  Transient overloads (429/503/529, `overloaded_error`) are retried with
  bounded backoff.
- **Tools.** `read_code`, `edit_code` (an exact-match replace that must match
  exactly once), `compile`, `get_geometry_stats`, `query_noos` and the
  `screenshot_viewport` placeholder. The tools work on an in-memory copy of
  the user's buffer.
- **Loop.** The agent runs for at most 12 rounds on its own OS thread, and its
  events stream to the UI as they happen. Tools run only when the model stops
  with `tool_use`. Thinking blocks are passed back to the model unchanged.
  Stop cancels between rounds and during retry waits.
- **Review.** Each `edit_code` call shows up in the chat as a diff. Accepting
  it applies the patch to the editor buffer, and only if the old text still
  occurs exactly once there. Rejecting it leaves the editor untouched. The
  user's editor buffer, not the agent's copy, is the source of truth.
- **Prompt-injection hygiene.** Every string that comes from a file or from
  memory (the current code, tool output, compile errors, project memory)
  reaches the model inside an `<untrusted_data>` wrapper that cannot be
  closed from inside. Its length is capped and it is sanitized, and the system
  prompt tells the model such content is data, not instructions. This lowers
  the risk but does not eliminate it, which is why every edit needs explicit
  user approval.

## Design memory (`.noos/`)

When a file is open, accepting an edit records a *decision* event in a
`.noos/` directory next to that file. Events go to `morphe-events.jsonl` plus
a graph file, written under an advisory file lock. At the start of each
session a digest of past decisions and constraints is added to the agent's
prompt. If the optional `noos` command-line tool is on `PATH`, the
`query_noos` tool can also search the project's knowledge graph. Without it,
the tool reports that memory search is unavailable.

## Conformance (`crates/morphe-conformance`)

- **Corpus gate.** Every file in `corpus/core/` must parse, evaluate and
  resolve without errors.
- **Coverage report.** A machine-checked inventory of the language constructs
  morphe supports, checked against the corpus.
- **Oracle.** If `openscad` is on `PATH`, each 3D corpus file is also
  rendered by OpenSCAD, and the two meshes are compared by signed volume and
  by a symmetric Hausdorff distance normalized to the bounding-box diagonal.
  A few files carry measured tolerance overrides, each with its reason.

[Manifold]: https://github.com/elalish/manifold
