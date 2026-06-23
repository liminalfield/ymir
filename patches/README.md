# Vendored dependency patches

We carry a small patch to `egui-snarl` because its embedded `Scene` is hardcoded to pan
on left-drag, and that drag is not configurable through snarl's public API. We need
left-drag on the empty canvas free for the app's own marquee box-select (#84), with
panning on the middle mouse button.

## What is patched

`egui-snarl` is vendored in `vendor/egui-snarl/` and the workspace points at it via
`[patch.crates-io]` in the root `Cargo.toml`. The single change moves the Scene's
drag-pan to the middle button:

```rust
Scene::new()
    .zoom_range(min_scale..=max_scale)
    .drag_pan_buttons(DragPanButtons::MIDDLE) // the patch
    .register_pan_and_zoom(&ui, &mut snarl_resp, &mut to_global);
```

`patches/egui-snarl-middle-pan.patch` is the durable record of that change as a unified
diff against the pristine crate source.

## Upgrading egui-snarl

When bumping the pinned version:

1. Re-vendor the new release into `vendor/egui-snarl/` (copy `src/`, `Cargo.toml`, and the
   `LICENSE-*` files from the new `egui-snarl-<version>` crate source under
   `~/.cargo/registry/src/`).
2. Re-apply the patch, from inside `vendor/egui-snarl/`:

   ```sh
   git apply -p1 ../../patches/egui-snarl-middle-pan.patch
   ```

3. If the hunk rejects because upstream moved the surrounding code, apply the change by
   hand (it is one import and one builder call), then regenerate the patch so it stays
   current:

   ```sh
   diff -u --label a/src/ui.rs --label b/src/ui.rs <pristine ui.rs> vendor/egui-snarl/src/ui.rs \
     > patches/egui-snarl-middle-pan.patch
   ```

4. Confirm the version in `vendor/egui-snarl/Cargo.toml` matches the pinned requirement,
   then `cargo build` to verify the patch took effect.

The proper long-term fix is to make the pan button a `SnarlStyle` option upstream; until
that lands, this carries the change.
