//! The GPU-compute seam: an abstract, GPU-type-free device handle.
//!
//! `ymir-core` is deliberately free of GPU types (see `CLAUDE.md`: heavy
//! dependencies live with the nodes, never in the engine). Yet an operator that
//! wants to run on the GPU needs a device handle, and that handle is threaded
//! through evaluation the same way the seed and resolution are, via
//! [`EvalContext`](crate::EvalContext).
//!
//! [`ComputeContext`] resolves that tension. It is an opaque capability marker:
//! `ymir-core` names the trait but knows nothing of what implements it. The GPU
//! crate (`ymir-gpu`) defines the concrete context, implements this trait for it,
//! and a GPU-capable operator recovers the concrete type with
//! [`as_any`](ComputeContext::as_any) and `downcast_ref`. When no handle is
//! present (a headless CPU-only run, or a golden test), a GPU-capable operator
//! falls back to its CPU path.
//!
//! This is the "trait defined in core, implemented in the GPU crate" shape: the
//! dependency arrow still points from the GPU crate to core, no `wgpu` type ever
//! reaches core, and the handle is still a first-class, named part of the eval
//! context rather than a bare `Arc<dyn Any>`.

use std::any::Any;
use std::fmt::Debug;

/// An opaque handle to a compute device, carried through evaluation by
/// [`EvalContext`](crate::EvalContext).
///
/// `ymir-core` treats this purely as a capability token: it never inspects it and
/// holds no GPU type. The concrete implementation lives in the GPU crate; a
/// GPU-capable operator downcasts back to it through [`as_any`](Self::as_any):
///
/// ```ignore
/// // Inside an operator's `eval`, where `GpuContext: ComputeContext` lives in ymir-gpu:
/// if let Some(gpu) = ctx.compute().and_then(|c| c.as_any().downcast_ref::<GpuContext>()) {
///     // run the GPU path
/// } else {
///     // fall back to the CPU path
/// }
/// ```
///
/// The [`Debug`] supertrait keeps [`EvalContext`](crate::EvalContext) and
/// `EvalRequest` deriving `Debug`; `Send + Sync` lets a context cross onto the
/// evaluation worker thread, which the GUI requires; `'static` allows the
/// downcast.
pub trait ComputeContext: Debug + Send + Sync + 'static {
    /// Returns `self` as a `dyn Any` so a GPU-capable operator can downcast to the
    /// concrete device type defined in the GPU crate. The bound is
    /// `Any + Send + Sync` so the recovered reference keeps those guarantees.
    fn as_any(&self) -> &(dyn Any + Send + Sync);
}
