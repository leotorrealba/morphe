# Contributing to morphe

Thanks for your interest in morphe! Bug reports, feature ideas, conformance
test cases and pull requests are all welcome.

## Ways to help

- **Report a bug.** Open an issue with the smallest `.scad` snippet that shows
  the problem, what you expected (ideally OpenSCAD's behaviour), what morphe
  did, and your OS and morphe version or commit.
- **Add conformance cases.** A small `.scad` file in `corpus/core/` that
  exercises a construct is one of the most valuable contributions. Every file
  there must compile end-to-end, and with OpenSCAD installed it is also
  compared mesh-for-mesh.
- **Close a known limitation.** See the list in the [README](README.md#known-limitations).
- **Improve docs.** Corrections to the README or `docs/` are always welcome.

## Development setup

See [Getting started](README.md#getting-started) for prerequisites. The first
build compiles the Manifold C++ kernel from source and takes several minutes;
later builds are incremental.

## Before you open a pull request

Run the same checks CI runs:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p morphe-conformance

cd apps/desktop
bun install --frozen-lockfile
bun run test
bun run build      # also type-checks
```

## Guidelines

- **One concern per PR.** Keep changes focused; split unrelated work.
- **Tests with fixes.** A bug fix should come with a regression test that fails
  without the fix. New language or geometry features need unit tests, and
  usually a corpus file too.
- **Follow OpenSCAD semantics.** When morphe implements an OpenSCAD construct,
  match upstream behaviour and cite the upstream source file in a comment.
  Document any deliberate divergence (and why) next to the code.
- **Treat inputs as untrusted.** `.scad` files, imported meshes and AI output
  can be hostile. Avoid panics on user input, bound allocations and recursion,
  and never let a frontend-supplied path cross the desktop IPC boundary (see
  [docs/architecture.md](docs/architecture.md)).
- **Match the surrounding code**: naming, idioms, comment style.
- **Commit messages** follow [Conventional Commits](https://www.conventionalcommits.org/)
  (`fix(kernel): …`, `feat(eval): …`, `docs: …`).
- **No secrets** in code, tests or fixtures, including test values shaped like
  real API keys.

## License

By contributing you agree that your contributions are licensed under the
project's license, **GPL-3.0-or-later** (see [LICENSE](LICENSE)).
