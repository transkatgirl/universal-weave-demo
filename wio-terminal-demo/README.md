# Universal Weave for Wio Terminal

A `no_std` Universal Weave demo for the Seeed Studio Wio Terminal. It combines
the dependent-tree and independent-DAG editors from the desktop reference demo
with the firmware's FAT16/FAT32 SD explorer, file actions, verified
transactional saves, card-removal protection, and battery display.

The firmware creates `.UWE` documents and opens both `.UWE` and `.UWEAVE`
documents (case-insensitive). `.TXT` and all other files remain visible in the
explorer but are not editable.

> **Memory warning:** every document is loaded into RAM and serialized in RAM.
> Memory use depends on graph shape, node count, titles, and node contents.
> There is no application-level file-size, node-count, title, or content limit;
> available memory and filesystem allocation are the limits. A sufficiently
> large file or edit can exhaust memory and crash or halt this demo, losing all
> unsaved changes.

## Explorer controls

- Joystick up/down: move selection
- Joystick right or click: open a folder or `.UWE`/`.UWEAVE` document
- Joystick left or top-left: parent folder
- Top-middle: create a Weave file or folder
- Top-right: rename, move, delete, or refresh

The 6×10 Unicode font matches the reference application and provides a 53×24
terminal grid. The explorer shows 20 entries at a time. New Weave names use an
eight-character FAT stem and receive `.UWE` automatically. The document-kind
chooser opens after naming; no placeholder is created. The first save uses
create-new semantics and refuses to overwrite a file that appeared under that
name in the meantime.

Existing long filenames are displayed, but files and folders created on the
device use FAT 8.3 names. Rename and move never replace an existing entry.
Folder actions are recursive, read-only trees are refused, and nesting deeper
than 32 levels is refused.

## Universal Weave controls

In the graph view:

- Joystick: select the nearest node in that direction
- Joystick click: toggle the selected node active
- Top-left: open the action menu
- Top-middle: cycle Inspector, Bookmarks, and Reading panels
- Top-right: exit; dirty documents offer Save & exit, Discard & exit, or Cancel

While Reading is selected, top-left/top-right scroll the active path and repeat
when held. Joystick directions repeat normally everywhere. The menu contains
node creation, editing, active/bookmark actions, bookmark navigation, split,
merge, independent-DAG move, sorting, deletion, title editing, pan/zoom, new
document, save, counter reset, help, and exit.

In pan/zoom view, the joystick pans, top-left/top-middle zoom, click fits, and
top-right returns. Text entry uses the same keyboard as the filesystem manager:
the joystick chooses a key, click types it, top-left cancels, top-middle hides
or shows the keyboard, and top-right applies. With the keyboard hidden,
left/right moves the text cursor and click shows the keyboard again.

Documents use a 32-byte `UNIVERSAL-WEAVE-DEMO` header followed by a 16-byte
aligned rkyv archive. Version 1 dependent trees and version 2 independent DAGs
are supported. Malformed files, unknown versions, and Loro collaboration
version 3 are rejected with an explanation. Loro collaboration and the 3D
renderer are out of scope.

## SD and save safety

The demo supports FAT16 and FAT32 in an MBR partition. Unsupported or invalid
readable media can be reformatted as MBR/FAT32 by holding top-right for two
seconds; **formatting erases the entire card**. I/O failures do not offer
formatting.

A save serializes the complete document, stages and verifies new contents as a
`~WIO*.TMP` file, backs up an existing target as `~WIO*.BAK`, commits, and
reads the target back before reporting success. An overwrite therefore needs
roughly twice the document size in free space. Interrupted saves can leave the
staging files for PC recovery.

Removing a card never discards the in-RAM document. Saving stays disabled until
a card with the original capacity, partition offset, and FAT volume serial is
mounted again. A failed Save & exit remains in the document and returns to the
exit confirmation instead of dropping changes. Battery chassis charge status,
when available, occupies the top-right eight columns.

The underlying `embedded-sdmmc` 0.10 limitations still apply: some deleted
clusters and orphaned long-filename entries require `chkdsk`/`fsck.fat`, FAT32
free-space counts can drift conservatively after truncation, moved entries do
not preserve attributes/timestamps, and damaged filesystems should be repaired
on a PC.

## Build and test

```sh
rustup target add thumbv7em-none-eabihf
cargo build --release
```

The ELF is written to
`target/thumbv7em-none-eabihf/release/wio-terminal-sd-editor`. To flash with
`cargo-hf2`, put the Wio Terminal in bootloader mode and run:

```sh
cargo hf2 --release --vid 0x2886 --pid 0x002d
```

The repository defaults to the embedded target, so host tests need an explicit
host triple:

```sh
cargo test --target aarch64-apple-darwin --lib
```

Replace that triple on other platforms.

## License

Licensed under either Apache-2.0 or MIT.
