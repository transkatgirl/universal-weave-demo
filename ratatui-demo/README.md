# universal-weave-ratatui-demo

An interactive terminal demo for [`universal-weave`](https://github.com/transkatgirl/universal-weave). It demonstrates both the tree-based `DependentWeave` and DAG-based `IndependentWeave` with a topological 2D renderer powered by the crate's `layout` feature.

The demo intentionally does not include the reference application's Loro collaboration or 3D renderer.

## Run

```console
cargo run
```

Use a terminal of at least 60 columns by 20 rows. At 100×30 and larger, the graph, inspector, bookmarks, reading view, and action log are shown together. Smaller terminals use a tabbed secondary panel.

Press `?` at any time for the complete in-app help.

## Controls

| Area | Keys |
| --- | --- |
| Documents | `n` new, `o` system open dialog, `s` system save dialog, `t` title, `L` clear log, `q` quit |
| Selection | `h`/`j`/`k`/`l` select left/down/up/right, `[`/`]` bookmarks |
| Viewport | arrows pan, `+`/`-` zoom, `0` fit, `f` focus selection |
| Nodes | `a` child, `r` root, `e` edit, `Space` active, `b` bookmark, `d` delete |
| Structure | `x` split, `M` merge, `m` move in a DAG, `c`/`i` sort children |
| Panels | `Tab` cycles compact panels, `PageUp`/`PageDown` scroll |

Single-line dialogs use `Enter` to confirm. The multiline content editor uses `Ctrl+S` to apply. `Esc` cancels any dialog. New, open, quit, and delete actions are immediate and do not prompt about unsaved work.

Directional selection uses node positions in the rendered layout rather than graph relationships. It chooses the closest node in the requested half-plane and does not wrap at an edge.

## File compatibility

The terminal demo reads and writes the same `.uweave` header and rkyv payloads as the reference `universal-weave-demo`:

- Version 1: dependent tree
- Version 2: independent DAG

Version 3 Loro documents are detected and rejected with an explanatory error because Loro support is outside this demo's scope.
