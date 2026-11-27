//! The Linux side of [`super`]: PipeWire's own list of who is capturing.
//!
//! Everything here runs on one thread, because the PipeWire objects are
//! `!Send`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use pipewire::context::ContextRc;
use pipewire::core::PW_ID_CORE;
use pipewire::main_loop::MainLoopRc;
use pipewire::node::{Node, NodeListener};
use pipewire::types::ObjectType;

use super::{MicUser, Shared, mic_user};

/// How long the Linux backend waits before it tries PipeWire again, after
/// failing to connect or after losing the connection. A sound server that
/// is not there is not about to be, and one that restarted is back within
/// seconds; half a minute costs nothing when the answer is "still no".
const RETRY: Duration = Duration::from_secs(30);

/// `PW_NODE_STATE_RUNNING` in `pipewire/node.h`: the node is moving
/// audio, not merely open.
const PW_NODE_STATE_RUNNING: i32 = 3;

pub fn spawn(shared: Arc<Shared>) {
    let spawned =
        std::thread::Builder::new().name("micwatch-pipewire".into()).spawn(move || run(shared));
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "micwatch: could not start the PipeWire thread");
    }
}

/// Connect, watch until the connection ends, forget everything seen,
/// wait, and again. Never returns and never panics on a PipeWire
/// failure.
fn run(shared: Arc<Shared>) {
    pipewire::init();
    loop {
        match watch_once(&shared) {
            Ok(()) => tracing::warn!("micwatch: lost the connection to PipeWire"),
            Err(e) => tracing::debug!(error = %e, "micwatch: PipeWire is not available"),
        }
        // What was seen on a connection that is gone is not to be
        // trusted: the streams' removals may never have arrived. The
        // new connection announces everything again from scratch.
        shared.users().clear();
        std::thread::sleep(RETRY);
    }
}

/// One connection's life: returns when the core reports that it is gone
/// (PipeWire stopped or restarted), or with the error that stopped it
/// from starting.
fn watch_once(shared: &Arc<Shared>) -> Result<(), pipewire::Error> {
    let main_loop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;
    tracing::debug!("micwatch: connected to PipeWire");

    let own_pid = std::process::id();

    // Weak, so the closure does not keep the loop alive in a cycle
    // through the listener it is stored in.
    let loop_weak = main_loop.downgrade();
    let _core_listener = core
        .add_listener_local()
        .error(move |id, seq, res, message| {
            tracing::debug!(id, seq, res, error = message, "micwatch: PipeWire core error");
            // Errors on other objects are theirs to deal with; an error
            // on the core itself (id 0) is the connection breaking.
            if id == PW_ID_CORE
                && let Some(main_loop) = loop_weak.upgrade()
            {
                main_loop.quit();
            }
        })
        .register();

    // Every candidate stream, bound so its state can be followed, kept
    // alive here -- a proxy or listener dropped stops reporting -- and
    // dropped when the stream goes. Only the running ones are in
    // `shared`'s list.
    let bound: Rc<RefCell<HashMap<u32, (Node, NodeListener)>>> = Rc::default();
    let registry_weak = registry.downgrade();
    let on_global = shared.clone();
    let on_global_bound = bound.clone();
    let on_remove = shared.clone();
    let on_remove_bound = bound.clone();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ != ObjectType::Node {
                return;
            }
            // The announcement carries only a handful of properties --
            // enough to know a capture stream, not whose it is: the
            // process id and binary `mic_user` needs arrive with the
            // node's own info. So every capture stream is bound, and
            // judged on that.
            let class = global.props.and_then(|p| p.get("media.class"));
            if class != Some("Stream/Input/Audio") {
                return;
            }
            let Some(registry) = registry_weak.upgrade() else { return };
            let node: Node = match registry.bind(global) {
                Ok(node) => node,
                Err(e) => {
                    tracing::debug!(id = global.id, error = %e, "micwatch: could not follow a stream");
                    return;
                }
            };
            let id = global.id;
            let users = on_global.clone();
            // Whether this stream counts, judged on every property it has
            // reported so far: an info event after the first carries
            // only the properties that changed, and judging on those
            // alone would find no `media.class` and drop the stream.
            let seen: RefCell<HashMap<String, String>> = RefCell::default();
            let judged: RefCell<Option<MicUser>> = RefCell::new(None);
            let listener = node
                .add_listener_local()
                .info(move |info| {
                    if let Some(props) = info.props() {
                        let mut seen = seen.borrow_mut();
                        for (key, value) in props.iter() {
                            seen.insert(key.to_owned(), value.to_owned());
                        }
                        let lookup = |key: &str| seen.get(key).cloned();
                        *judged.borrow_mut() = mic_user(&lookup, own_pid);
                    }
                    // Read raw rather than through `NodeInfoRef::state`,
                    // which panics on a state number it does not know --
                    // and a panic in a callback from C takes the process
                    // with it.
                    let running = info.as_raw().state == PW_NODE_STATE_RUNNING;
                    match (&*judged.borrow(), running) {
                        (Some(user), true) => {
                            // The node id tells streams apart for a handover.
                            // PipeWire allocates ids upwards and reuses one
                            // only after wrap-around or freeing, so a reused
                            // id could, rarely, hide a handover.
                            let user = MicUser { id: Some(id), ..user.clone() };
                            if users.users().insert(id, user.clone()).is_none() {
                                tracing::debug!(id, ?user, "micwatch: a stream is capturing");
                            }
                        }
                        _ => {
                            if users.users().remove(&id).is_some() {
                                tracing::debug!(id, "micwatch: a stream stopped capturing");
                            }
                        }
                    }
                })
                .register();
            on_global_bound.borrow_mut().insert(id, (node, listener));
        })
        .global_remove(move |id| {
            on_remove_bound.borrow_mut().remove(&id);
            if on_remove.users().remove(&id).is_some() {
                tracing::debug!(id, "micwatch: a capturing stream went away");
            }
        })
        .register();

    main_loop.run();
    Ok(())
}
