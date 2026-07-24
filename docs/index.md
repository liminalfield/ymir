---
status: draft
---

# Ymir

Ymir is a node-based procedural terrain generator for Linux, written in Rust. You build terrain by
wiring a graph of small, single-purpose nodes: generators lay down base shapes and noise, modifiers
sculpt and erode them, selectors and masks scope where effects apply, and endpoints export
heightmaps. One data type flows on every edge, so a node drops in anywhere and there is no fixed
build order.

![The Ymir node editor and 3D viewport](images/ymir-editor.png)

## Get started

- **[Install](install.md)** — build Ymir from source.
- **[Tutorial](tutorial/index.md)** — make your first terrain, start to finish.
- **[Reference](reference/index.md)** — every node, world setting, export format, and keyboard shortcut.
- **[Concepts](concepts/index.md)** — the ideas behind the tool: fields, masks, erosion, preview versus build.

Ymir is free software under the GPL-3.0. The source lives at
[github.com/liminalfield/ymir](https://github.com/liminalfield/ymir).
