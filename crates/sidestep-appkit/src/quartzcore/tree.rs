//! The main thread's side of the render thread's layer trees
//! (`sidestep_engine::ca::tree`, which has the render thread's): what a
//! commit says of each changed layer, and sending it.

use std::collections::HashMap;

pub(crate) use sidestep_engine::ca::tree::*;

use super::layer::{CALayerImpl, LayerId};
use crate::protocol::ToRender;

thread_local! {
    /// The contents version each layer last sent (and whether a view backs
    /// it).
    static SENT_CONTENT: std::cell::RefCell<HashMap<LayerId, (u64, bool)>> = std::cell::RefCell::default();
}

/// A changed layer as the render thread takes it.
pub(crate) fn update_of(layer: &CALayerImpl) -> LayerUpdate {
    let id = layer.id();
    let (props, sublayers, mask, anims, version) = layer.read(|m| {
        (
            m.props.clone(),
            m.sublayers.iter().map(|s| super::layer::imp(s).id()).collect(),
            m.mask.as_ref().map(|s| super::layer::imp(s).id()),
            m.anims.iter().filter(|a| a.started).map(|a| a.spec.clone()).collect(),
            m.content_version,
        )
    });
    // Contents go again only when they changed: a view's canvas is kept by
    // its rectangle (the render thread keeps a canvas that stays the same),
    // images and nothing by the layer's contents version.
    let view = layer.view();
    let canvas = view.as_ref().is_some_and(|v| super::backing::draws_canvas(v));
    let content = match &view {
        Some(view) if canvas => super::backing::canvas_update(view, &props),
        _ => {
            let tag = (version, view.is_some());
            let changed = SENT_CONTENT.with(|s| s.borrow_mut().insert(id, tag) != Some(tag));
            if !changed {
                ContentUpdate::Keep
            } else {
                let image = match &view {
                    Some(view) => super::backing::given_contents(view, &props),
                    None => super::render::contents_image(layer),
                };
                match image {
                    Some((image, size)) => ContentUpdate::Image(image, size),
                    None => ContentUpdate::None,
                }
            }
        }
    };
    if canvas {
        SENT_CONTENT.with(|s| s.borrow_mut().remove(&id));
    }
    LayerUpdate { id, props, sublayers, mask, anims, content }
}

/// Wake the render thread (an animation begins now): an empty commit has
/// it draw what animates.
pub(crate) fn wake() {
    crate::app::send_if_running(ToRender::Commit(Box::new(Commit {
        updates: Vec::new(),
        gone: Vec::new(),
        time: super::math::media_now(),
        hold: Vec::new(),
    })));
}

/// Send a commit to the render thread (if it runs).
pub(crate) fn send_commit(updates: Vec<LayerUpdate>, gone: Vec<LayerId>, time: f64) {
    SENT_CONTENT.with(|s| {
        let mut s = s.borrow_mut();
        for id in &gone {
            s.remove(id);
        }
    });
    let hold = super::backing::take_holds();
    crate::app::send_if_running(ToRender::Commit(Box::new(Commit { updates, gone, time, hold })));
}
