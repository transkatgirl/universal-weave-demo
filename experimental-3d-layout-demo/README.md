# Experimental 3D layout demo

An experimental desktop GUI for
[`universal-weave`](https://github.com/transkatgirl/universal-weave). It extends
the native reference demo with a radial 3D graph powered by
[`universal-weave-layout`](https://github.com/transkatgirl/universal-weave-layout),
while retaining the topological 2D graph for comparison.

The demo supports `DependentWeave` trees, `IndependentWeave` DAGs, and
Loro-backed collaborative dependent trees. It provides node editing,
active-path navigation, bookmarks, structural operations, a reading view,
native open/save dialogs, and an action log.

## Run

```console
cargo run
```

The crate fetches `universal-weave` and `universal-weave-layout` from Git, so
the first build requires network access.

## Controls

Use the toolbar to open or save a `.uweave` document, choose a document kind,
create a document, add a root, and switch between **2D tree** and **3D radial**
views. Click a node to select it and double-click it to toggle its active state.
The inspector edits the selected node and exposes the operations supported by
its document kind.

The 2D graph sits in a scrollable canvas. In the 3D view:

- Drag to orbit.
- Shift-drag or middle-drag to pan.
- Scroll or pinch to zoom.
- Select **Reset view** in the toolbar to fit the graph and restore the initial
  camera angle.

Creating a collaborative dependent tree opens a second peer window. Toggle a
peer offline to edit the copies independently, then reconnect it to demonstrate
Loro CRDT synchronization. Both peers run in the same process; the demo does
not open a network connection.

## File compatibility

The demo uses the shared 32-byte `UNIVERSAL-WEAVE-DEMO` header followed by a
versioned rkyv payload:

- Version 1: dependent tree
- Version 2: independent DAG
- Version 3: Loro collaborative dependent tree

All three versions are read/write compatible with the native reference demo in
the repository root. Versions 1 and 2 are also compatible with the Ratatui and
Wio Terminal demos.

## Test

```console
cargo fmt --check
cargo test
```
