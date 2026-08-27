# Universal Weave demos

This repository contains three applications that exercise
[`universal-weave`](https://github.com/transkatgirl/universal-weave) across a
native GUI, a desktop terminal, and an embedded device. Each application is a
separate Rust crate rather than a member of a shared Cargo workspace.

The demos model a document as connected text nodes. They cover two core
structures:

- `DependentWeave`: a tree in which each node has at most one parent and its
  contents depend on that parent.
- `IndependentWeave`: a directed acyclic graph (DAG) in which a node can have
  multiple parents and its contents are independent of them.

All three applications provide node editing, active-path navigation,
bookmarks, structural operations, a reading view, and versioned document
persistence. Their presentation and platform-specific features differ.

## The applications

| Application | Location | Interface | Document kinds | Distinguishing features |
| --- | --- | --- | --- | --- |
| Native reference demo | Repository root | eframe/egui desktop GUI | Dependent tree, independent DAG, and collaborative dependent tree | Topological 2D and radial 3D views, native file dialogs, action log, and an in-process two-peer Loro collaboration demo |
| Ratatui demo | [`ratatui-demo/`](ratatui-demo/) | Desktop terminal UI | Dependent tree and independent DAG | Keyboard-driven editing, directional spatial navigation, responsive full/tabbed layouts, and native open/save dialogs |
| Wio Terminal demo | [`wio-terminal-demo/`](wio-terminal-demo/) | Seeed Studio Wio Terminal firmware | Dependent tree and independent DAG | `no_std` editor, joystick-driven on-screen keyboard, FAT16/FAT32 SD explorer, transactional saves, card-removal protection, and battery status |

### Native reference demo

The root crate is the most complete demonstration. It starts with a sample
dependent document and can create or open any of the three supported document
kinds. Use the inspector to edit contents and perform operations such as adding,
splitting, merging, moving, sorting, bookmarking, and deleting nodes. Click a
node to select it and double-click it to change its active state.

The central canvas can switch between a scrollable 2D graph and a radial 3D
layout powered by
[`universal-weave-layout`](https://github.com/transkatgirl/universal-weave-layout).
In the 3D view, drag to orbit, Shift-drag or middle-drag to pan, and scroll or
pinch to zoom.

Creating a `DependentLoroWeave` opens a second peer window. The two local peers
can be taken offline, edited independently, and reconnected to demonstrate
Loro-backed CRDT synchronization. This is an in-process simulation; it does not
open a network connection.

Run it from the repository root:

```console
cargo run
```

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

| Format version | Contents | Native GUI | Ratatui | Wio Terminal |
| --- | --- | --- | --- | --- |
| 1 | Dependent tree | Read/write | Read/write | Read/write |
| 2 | Independent DAG | Read/write | Read/write | Read/write |
| 3 | Loro collaborative dependent tree | Read/write | Rejected with an explanation | Rejected with an explanation |

This means version 1 and 2 documents can move between all three demos. The
desktop applications normally use `.uweave`; Wio-created `.UWE` files contain
the same format.

## Development

Install a current Rust toolchain with
[`rustup`](https://rustup.rs/). The crates fetch `universal-weave` (and, for the
native GUI, `universal-weave-layout`) from Git, so the first build also requires
network access.

Run the desktop test suites from their respective crate directories:

```console
# Native reference demo
cargo test

# Ratatui demo
cd ratatui-demo
cargo test
```

The Wio crate defaults to its embedded target. To run its library tests on the
development machine, provide your Rust host triple explicitly:

```console
cd wio-terminal-demo
cargo test --target <your-host-triple> --lib
```
