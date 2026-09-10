# Universal Weave demos

This repository contains four applications that exercise
[`universal-weave`](https://github.com/transkatgirl/universal-weave) across a
native GUI, an experimental 3D GUI, a desktop terminal, and an embedded device.
Each application is a separate Rust crate rather than a member of a shared
Cargo workspace.

The demos model a document as connected text nodes. They cover two core
structures:

- `DependentWeave`: a tree in which each node has at most one parent and its
  contents depend on that parent.
- `IndependentWeave`: a directed acyclic graph (DAG) in which a node can have
  multiple parents and its contents are independent of them.

All four applications provide node editing, active-path navigation,
bookmarks, structural operations, a reading view, and versioned document
persistence. Their presentation and platform-specific features differ.

## The applications

| Application | Location | Interface | Document kinds | Distinguishing features |
| --- | --- | --- | --- | --- |
| Native reference demo | Repository root | eframe/egui desktop GUI | Dependent tree, independent DAG, and collaborative dependent tree | Topological 2D view, native file dialogs, action log, and an in-process two-peer Loro collaboration demo |
| Experimental 3D layout demo | [`experimental-3d-layout-demo/`](experimental-3d-layout-demo/) | eframe/egui desktop GUI | Dependent tree, independent DAG, and collaborative dependent tree | Adds an interactive radial 3D view powered by `universal-weave-layout` to the native reference feature set |
| Ratatui demo | [`ratatui-demo/`](ratatui-demo/) | Desktop terminal UI | Dependent tree and independent DAG | Keyboard-driven editing, directional spatial navigation, responsive full/tabbed layouts, and native open/save dialogs |
| Wio Terminal demo | [`wio-terminal-demo/`](wio-terminal-demo/) | Seeed Studio Wio Terminal firmware | Dependent tree and independent DAG | `no_std` editor, joystick-driven on-screen keyboard, FAT16/FAT32 SD explorer, transactional saves, card-removal protection, and battery status |

### Native reference demo

The root crate is the native reference implementation. It starts with a sample
dependent document and can create or open any of the three supported document
kinds. Use the inspector to edit contents and perform operations such as adding,
splitting, merging, moving, sorting, bookmarking, and deleting nodes. The
central canvas renders a scrollable topological 2D graph; click a node to select
it and double-click it to change its active state.

For independent DAG documents, the reading view is also an active-path editor.
Typing is staged locally until **Apply** is clicked, then a character-level diff
turns each changed range into its own structural path patch. Patches are applied
from the end of the text backwards, so unchanged text between two changes stays
a single node shared by the old and new branches. Where a change could sit at
several equivalent positions, such as an inserted repeated word, it is aligned
to the nearest word, sentence, or line boundary so node contents stay readable.
Text removed or replaced by a patch is preserved on alternate DAG branches
instead of being destroyed, and inserted text continues only into the active
path, never into the text it replaced. **Reset** discards staged typing and
reloads the current path. If a structural operation changes the path while an
edit is staged, Apply rejects the stale edit until the buffer is reset.
Dependent and Loro reading views remain read-only.

Creating a `DependentLoroWeave` opens a second peer window. The two local peers
can be taken offline, edited independently, and reconnected to demonstrate
Loro-backed CRDT synchronization. This is an in-process simulation; it does not
open a network connection.

Run it from the repository root:

```console
cargo run
```

### Experimental 3D layout demo

The experimental desktop demo preserves the native reference application's 3D
radial visualization. It includes the same document editing, persistence, and
in-process Loro collaboration features as the root demo, and can switch between
the topological 2D graph and a radial layout powered by
[`universal-weave-layout`](https://github.com/transkatgirl/universal-weave-layout).

In the 3D view, drag to orbit, Shift-drag or middle-drag to pan, and scroll or
pinch to zoom. Use **Reset view** to fit the graph and restore the initial
camera angle.

```console
cd experimental-3d-layout-demo
cargo run
```

See the [experimental 3D layout demo README](experimental-3d-layout-demo/README.md)
for its controls and behavior notes.

### Ratatui demo

The terminal application brings the dependent-tree and independent-DAG editors
to a keyboard-driven TUI. Its graph uses the library's topological 2D layout,
and directional selection follows the rendered node positions. Large terminals
show the graph, inspector, bookmarks, reading view, and action log together;
smaller terminals provide tabbed secondary panels.

```console
cd ratatui-demo
cargo run
```

Use a terminal of at least 60 columns by 20 rows and press `?` for the complete
in-app help. See the [Ratatui demo README](ratatui-demo/README.md) for the full
key map and behavior notes.

### Wio Terminal demo

The embedded application is a `no_std` editor and SD-card file manager for the
[Seeed Studio Wio Terminal](https://wiki.seeedstudio.com/Wio-Terminal-Getting-Started/).
It renders a 53×24 terminal grid on the device display and maps the graph,
menus, text editor, and file explorer to the joystick and three top buttons.

The firmware creates `.UWE` files and opens `.UWE` or `.UWEAVE` files from a
FAT16/FAT32 SD card. Saves are staged, verified, and committed with a backup;
the editor also keeps an open document in memory if its card is removed. The
device has finite RAM and loads and serializes entire documents in memory, so
large documents can exhaust it.

Build the firmware with the embedded target:

```console
cd wio-terminal-demo
rustup target add thumbv7em-none-eabihf
cargo build --release
```

The resulting ELF is
`target/thumbv7em-none-eabihf/release/wio-terminal-sd-editor`. With `cargo-hf2`
installed, place the Wio Terminal in bootloader mode and flash it with:

```console
cargo hf2 --release --vid 0x2886 --pid 0x002d
```

See the [Wio Terminal demo README](wio-terminal-demo/README.md) before using the
firmware. It documents all controls, SD-card behavior, save recovery, memory
constraints, formatting warnings, and host-test instructions.

## Document compatibility

The applications share the same 32-byte `UNIVERSAL-WEAVE-DEMO` header and rkyv
payload format. File extensions differ by platform but do not change the data
format.

| Format version | Contents | Native GUI | Experimental 3D | Ratatui | Wio Terminal |
| --- | --- | --- | --- | --- | --- |
| 1 | Dependent tree | Read/write | Read/write | Read/write | Read/write |
| 2 | Independent DAG | Read/write | Read/write | Read/write | Read/write |
| 3 | Loro collaborative dependent tree | Read/write | Read/write | Rejected with an explanation | Rejected with an explanation |

This means version 1 and 2 documents can move between all four demos. The
desktop applications normally use `.uweave`; Wio-created `.UWE` files contain
the same format.

## Development

Install a current Rust toolchain with
[`rustup`](https://rustup.rs/). The crates fetch `universal-weave` from Git, and
the experimental 3D demo also fetches `universal-weave-layout`, so the first
build requires network access.

Run the desktop test suites from their respective crate directories:

```console
# Native reference demo
cargo test

# Experimental 3D layout demo
cargo test --manifest-path experimental-3d-layout-demo/Cargo.toml

# Ratatui demo
cargo test --manifest-path ratatui-demo/Cargo.toml
```

The Wio crate defaults to its embedded target. To run its library tests on the
development machine, provide your Rust host triple explicitly:

```console
cd wio-terminal-demo
cargo test --target <your-host-triple> --lib
```
