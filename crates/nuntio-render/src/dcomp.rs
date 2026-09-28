//! DirectComposition layers for DX12 surfaces on Windows.
//!
//! wgpu's own DirectComposition surfaces (`Dx12SwapchainKind::DxgiFromVisual`)
//! take the window's composition target for themselves and show their swap
//! chain right when it is configured, before anything is drawn into it. A
//! renderer replacing another one would first leave the window empty, then
//! show an undrawn frame. Here the renderers of a window share one tree
//! instead, and a layer only replaces the one shown so far once its first
//! frame is presented.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::rc::{Rc, Weak};

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice2, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::core::Interface;

/// The composition tree of a window: a window has only one target.
struct Tree {
    device: IDCompositionDevice,
    root: IDCompositionVisual,
    _target: IDCompositionTarget,
}

impl Tree {
    fn new(hwnd: isize) -> windows::core::Result<Self> {
        // SAFETY: plain COM calls; `hwnd` is the live window of the surface.
        unsafe {
            let device: IDCompositionDevice = DCompositionCreateDevice2(None)?;
            let target = device.CreateTargetForHwnd(HWND(hwnd as *mut c_void), false)?;
            let root = device.CreateVisual()?;
            target.SetRoot(&root)?;
            Ok(Self {
                device,
                root,
                _target: target,
            })
        }
    }
}

thread_local! {
    /// The trees of the windows, while a layer uses them. Once the last
    /// layer of a window is gone, so is its tree, and with it what it shows.
    static TREES: RefCell<HashMap<isize, Weak<Tree>>> = RefCell::default();
}

/// A visual in the window's tree for one surface. Not shown until its
/// first frame is presented; then it replaces the layer shown so far,
/// which stays up until then, with its last frame.
pub(crate) struct Layer {
    tree: Rc<Tree>,
    visual: IDCompositionVisual,
    /// The surface got a new swap chain that isn't committed yet.
    pending: bool,
}

impl Layer {
    pub(crate) fn new(hwnd: isize) -> windows::core::Result<Self> {
        let tree = TREES.with_borrow_mut(|trees| {
            trees.retain(|_, tree| tree.strong_count() > 0);
            if let Some(tree) = trees.get(&hwnd).and_then(Weak::upgrade) {
                return Ok(tree);
            }
            let tree = Rc::new(Tree::new(hwnd)?);
            trees.insert(hwnd, Rc::downgrade(&tree));
            Ok::<_, windows::core::Error>(tree)
        })?;
        // SAFETY: a plain COM call.
        let visual = unsafe { tree.device.CreateVisual() }?;
        Ok(Self {
            tree,
            visual,
            pending: false,
        })
    }

    /// What to create the surface from: the layer's visual.
    pub(crate) fn surface_target(&self) -> wgpu::SurfaceTargetUnsafe {
        wgpu::SurfaceTargetUnsafe::CompositionVisual(self.visual.as_raw())
    }

    /// The surface was configured: wgpu has set a swap chain as the
    /// visual's content, which only shows after the next commit.
    pub(crate) fn configured(&mut self) {
        self.pending = true;
    }

    /// A frame was presented: show it, in place of any other layer.
    pub(crate) fn presented(&mut self) {
        if !std::mem::take(&mut self.pending) {
            return;
        }
        let Tree { device, root, .. } = &*self.tree;
        // SAFETY: plain COM calls on objects this layer keeps alive.
        let result = unsafe {
            root.RemoveAllVisuals()
                .and_then(|()| root.AddVisual(&self.visual, false, None))
                .and_then(|()| device.Commit())
        };
        if let Err(err) = result {
            tracing::warn!("failed to show the DirectComposition layer: {err}");
        }
    }
}
