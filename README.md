# morphe

> **μορφή** — "form". An OpenSCAD-compatible parametric CAD app with a built-in AI agent.

morphe compiles `.scad` source into printable geometry with its own Rust
interpreter and the [Manifold](https://github.com/elalish/manifold) CSG kernel.
It ships a live desktop editor with a 3D viewport and an AI chat that edits your
model from natural language. Every AI edit arrives as a reviewable diff you
accept or reject; the code stays visible and editable at all times.

> **Status: early (0.1, pre-release).** The core language and kernel are well
> tested, but expect rough edges, missing features and breaking changes. The
> desktop app is developed and tested on macOS first; Linux builds and runs,
> and Windows is untested.

## Features

- **OpenSCAD language core.** Lexer, parser and evaluator for the OpenSCAD
  dialect: variables, functions, modules, `children()`, control flow, list
  comprehensions, `let`, special variables (`$fn`, `$fa`, `$fs`, …), and the
  usual built-in functions.
- **Geometry.** 2D/3D primitives, booleans, transforms, `linear_extrude`,
  `rotate_extrude`, `hull`, `minkowski`, `offset`, `projection`, `surface`,
  `text` (bundled Fira Sans), `import` (STL), `polyhedron`, `resize`,
  `multmatrix` and `color`. CSG runs on Manifold.
- **Output.** Binary and ASCII STL export, plus geometry stats (bounding box,
  volume, surface area, manifoldness).
- **Desktop app** (Tauri 2 + React): a CodeMirror editor with `.scad`
  highlighting and inline errors, a three.js viewport, live recompile as you
  type, Open / Save / Save As, and STL export.
- **AI agent.** Bring your own key for Anthropic or any OpenAI-compatible
  endpoint (OpenAI, OpenRouter, local servers). The agent reads your code,
  proposes edits, compiles and inspects geometry in a bounded tool loop. Each
  edit shows as a diff you accept or reject. Text from your files reaches the
  model marked as untrusted data, never as instructions.
- **Design memory (optional).** Accepted edits and constraints can be recorded
  per project under `.noos/` and fed back to the agent in later sessions.
- **Headless CLI.** `morphe parse | compile | stats` for scripts and CI.
- **Conformance suite.** A corpus of `.scad` files that must compile
  end-to-end, plus an optional mesh comparison against a locally installed
  OpenSCAD binary (volume and Hausdorff distance).

### Known limitations

- `include <…>` / `use <…>` search the including file's directory, then
  `OPENSCADPATH` (not OpenSCAD's user or built-in library folders). An unsaved
  buffer can only include from `OPENSCADPATH`. A missing file or an include
  cycle is an error (OpenSCAD warns and skips it).
- In the desktop editor, an error inside an included or used file is marked on
  the `include`/`use` line of the open file (the CLI prints the library's own
  `file:line:col`).
- A failed `assert` (statement or expression) is reported as an error but only
  drops its own subtree or value; OpenSCAD aborts the whole evaluation.
- `import()` reads STL only (no 3MF/OFF/DXF/SVG). `text()` builds geometry
  only (no `textmetrics()`).
- `import()` and `surface()` only load files inside the directory of the open
  `.scad` file (or, when called from a library file, under `OPENSCADPATH`),
  and `include`/`use` only those under it or `OPENSCADPATH`.
  This is a deliberate divergence from OpenSCAD: the compile result is visible
  to the AI agent.
- The API key is stored in the OS credential store (macOS Keychain, Windows
  Credential Manager, or the Secret Service on Linux). If none is available,
  for example on Linux without a running Secret Service, it is kept in memory
  for the session only.
- The agent's `screenshot_viewport` tool is a placeholder.

## Getting started

### Prerequisites

- Rust **1.96** (pinned in `rust-toolchain.toml`; `rustup` installs it automatically).
- A C++ toolchain and **CMake**. The Manifold kernel is built from source the
  first time, which takes several minutes.
- [Bun](https://bun.sh) for the desktop frontend.
- For the desktop app on Linux, the WebKitGTK development packages. On
  Debian/Ubuntu:

  ```bash
  sudo apt-get install libwebkit2gtk-4.1-dev libgtk-3-dev \
    libayatana-appindicator3-dev librsvg2-dev
  ```

### CLI

```bash
# Compile a .scad file to STL (binary by default; --ascii for ASCII)
cargo run -p morphe-cli -- compile part.scad -o part.stl

# Geometry stats, human-readable or JSON
cargo run -p morphe-cli -- stats part.scad --json

# Syntax check
cargo run -p morphe-cli -- parse part.scad
```

### Desktop app

```bash
cd apps/desktop
bun install
bun run tauri dev           # development build with hot reload
bun run tauri build         # release bundle
```

To use the AI chat, open the chat pane, choose a provider and paste an API key.
Then describe a change, for example *"make a bracket with four M3 holes on
40 mm centres"*. Review each proposed diff and accept or reject it; the
viewport recompiles live.

| Shortcut | Action |
|---|---|
| ⌘S / Ctrl+S | Save (back to the open file) |
| ⇧⌘S / Ctrl+Shift+S | Save As… |
| ⌘E / Ctrl+E | Export STL |

## Architecture

```
crates/
  morphe-lang         lexer, parser and AST for the OpenSCAD dialect
  morphe-eval         evaluator: scopes, functions, modules, builtins → CSG tree
  morphe-kernel       CSG tree → Manifold solids → mesh / STL / stats
  morphe-ai           LLM client (Anthropic + OpenAI-compatible), tools, agent loop
  morphe-cli          the `morphe` command-line tool
  morphe-conformance  corpus gate, coverage report and OpenSCAD oracle
apps/desktop          Tauri 2 shell (src-tauri, Rust) + React/TypeScript frontend
corpus/core           conformance corpus (.scad files + data fixtures)
```

The pipeline is `.scad` → **parse** (`morphe-lang`) → **evaluate**
(`morphe-eval`, producing a CSG tree) → **resolve** (`morphe-kernel`, Manifold)
→ mesh, STL or stats. The desktop app and the AI agent drive the same
pipeline in-process. See [docs/architecture.md](docs/architecture.md) for
details on the IPC boundary, the agent loop and the security model.

## Testing

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Conformance corpus + coverage. With an `openscad` binary on PATH, this also
# compares each mesh against OpenSCAD's output; without one, that step is
# skipped.
cargo run -p morphe-conformance

# Frontend
cd apps/desktop && bun run test && bun run build
```

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md). Please
report security issues privately as described in [SECURITY.md](SECURITY.md).

## License

Copyright © 2026 Leonardo Torrealba and the morphe contributors.

morphe is free software: you can redistribute it and/or modify it under the
terms of the **GNU General Public License, version 3 or (at your option) any
later version**. See [LICENSE](LICENSE).

Parts of the evaluator and kernel follow, and in places port, the behaviour of
[OpenSCAD](https://openscad.org) (GPL-2.0-or-later); source comments cite the
upstream files. Third-party components:

- [Manifold](https://github.com/elalish/manifold) (Apache-2.0), the CSG kernel,
  via the `manifold3d` crate.
- [Fira Sans](https://github.com/mozilla/Fira) (SIL Open Font License 1.1),
  bundled for `text()`. See
  [`crates/morphe-kernel/src/assets/fonts/OFL.txt`](crates/morphe-kernel/src/assets/fonts/OFL.txt).

morphe is an independent project and is not affiliated with or endorsed by the
OpenSCAD project.
