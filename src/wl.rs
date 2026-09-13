//! Wayland connection, globals, protocol handling, and the event loop.
//!
//! One layer surface per edge per output. The shell is a single-threaded state
//! machine: everything that happens — pointer motion, module output, commands —
//! arrives as an event and is applied in order.

use std::time::{Duration, Instant};

use calloop::{EventLoop, LoopHandle, RegistrationToken};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    reexports::{calloop::channel, calloop_wayland_source::WaylandSource},
    seat::{
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_seat, wl_surface},
    Connection, QueueHandle,
};

use crate::app::{App, Event, Update};
use crate::config::{self, Config};
use crate::edge::{Content, Edge, Surface};
use crate::notify;
use crate::render::Renderer;
use crate::script::Modules;

/// How often the slide is advanced while it is moving.
const FRAME: Duration = Duration::from_millis(16);

/// Enough to cover a side edge's strip before it ever needs to grow.
const INITIAL_POOL: usize = 64 * 1024;

/// Connect to the compositor and run until asked to quit.
pub fn run(config: Config) -> Result<(), String> {
    let connection = Connection::connect_to_env().map_err(|error| {
        format!("cannot connect to a Wayland display (is WAYLAND_DISPLAY set?): {error}")
    })?;
    let (globals, mut event_queue) =
        registry_queue_init(&connection).map_err(|error| format!("cannot read the Wayland registry: {error}"))?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|error| format!("the compositor is not available: {error}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|error| {
        format!("this compositor has no wlr-layer-shell (Hyprland, sway, niri and river have it): {error}")
    })?;
    let shm = Shm::bind(&globals, &qh)
        .map_err(|error| format!("shared memory is not available: {error}"))?;

    let mut event_loop: EventLoop<Shell> =
        EventLoop::try_new().map_err(|error| format!("cannot create an event loop: {error}"))?;
    let handle = event_loop.handle();

    let (events, event_source) = channel::channel::<Event>();
    let module_source = handle
        .insert_source(event_source, |event, _, shell| {
            if let channel::Event::Msg(event) = event {
                shell.apply_event(event);
            }
        })
        .map_err(|error| format!("cannot watch module updates: {error}"))?;
    let modules = Modules::start(&config, move |index, value| {
        let _ = events.send(Event::Module { index, value });
    });

    let mut shell = Shell {
        registry_state: RegistryState::new(&globals),
        compositor,
        layer_shell,
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        shm,
        renderer: Renderer::new(config.bar.font.clone()),
        app: App::new(config),
        surfaces: Vec::new(),
        handle,
        module_source: Some(module_source),
        modules,
        pointer: None,
        active_output: None,
        exit: false,
    };

    // Let the registry's initial events through, which is what reveals the
    // outputs and gives each edge somewhere to live.
    event_queue
        .roundtrip(&mut shell)
        .map_err(|error| format!("cannot talk to the compositor: {error}"))?;

    WaylandSource::new(connection, event_queue)
        .insert(shell.handle.clone())
        .map_err(|error| format!("cannot watch the Wayland connection: {error}"))?;

    while !shell.exit {
        let timeout = shell.wakeup_in();
        event_loop
            .dispatch(timeout, &mut shell)
            .map_err(|error| format!("event loop failed: {error}"))?;
        shell.tick(Instant::now())?;
    }

    // Stop the module processes before the senders go away.
    shell.modules.stop();
    Ok(())
}

struct Shell {
    registry_state: RegistryState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    renderer: Renderer,
    app: App,
    surfaces: Vec<Surface>,
    /// The event loop, so that a reload can re-register the module channel.
    handle: LoopHandle<'static, Shell>,
    module_source: Option<RegistrationToken>,
    modules: Modules,
    pointer: Option<wl_pointer::WlPointer>,
    /// Where the pointer was last seen, so notifications land on that output.
    active_output: Option<wl_output::WlOutput>,
    exit: bool,
}

impl Shell {
    /// How long the loop may sleep before something needs to happen again.
    ///
    /// `None` means "until the next event", which is what an idle shell with
    /// nothing on screen should be doing.
    fn wakeup_in(&self) -> Option<Duration> {
        let now = Instant::now();
        let mut next = self.app.next_deadline();
        if self.app.is_sliding() {
            let frame = now + FRAME;
            next = Some(next.map_or(frame, |at| at.min(frame)));
        }
        next.map(|at| at.saturating_duration_since(now))
    }

    /// Move time-driven state forward, then repaint what changed.
    fn tick(&mut self, now: Instant) -> Result<(), String> {
        let update = self.app.advance(now);
        let bar_moved = update.bar_moved;
        self.apply(update);

        if self.app.take_dirty() {
            self.draw_edges(&Edge::ALL)
        } else if bar_moved {
            // Only the bar is moving; the other edges would be identical work.
            self.draw_edges(&[Edge::Top])
        } else {
            Ok(())
        }
    }

    fn apply_event(&mut self, event: Event) {
        let update = self.app.handle(event, Instant::now());
        self.apply(update);
    }

    fn apply(&mut self, update: Update) {
        for (id, reason) in update.closed {
            self.report_closed(id, reason);
        }
        if update.reload {
            self.reload();
        }
        if update.quit {
            self.exit = true;
        }
    }

    /// Tell clients that a notification is gone.
    fn report_closed(&mut self, id: u32, reason: crate::notify::stack::ClosedReason) {
        let _ = (id, reason);
    }

    /// Read the config again and restart the modules against it.
    ///
    /// A bad file is reported and ignored: the running configuration stays.
    fn reload(&mut self) {
        let (config, warnings) = match config::load() {
            Ok(loaded) => loaded,
            Err(error) => {
                eprintln!("quickbar: {error}");
                return;
            }
        };
        for warning in warnings {
            eprintln!("quickbar: {warning}");
        }

        // The old modules go first, then their channel: a closed channel is how
        // the loop finds out they are gone.
        self.modules.stop();
        if let Some(token) = self.module_source.take() {
            self.handle.remove(token);
        }

        let (events, source) = channel::channel::<Event>();
        match self.handle.insert_source(source, |event, _, shell| {
            if let channel::Event::Msg(event) = event {
                shell.apply_event(event);
            }
        }) {
            Ok(token) => self.module_source = Some(token),
            Err(error) => eprintln!("quickbar: cannot watch module updates: {error}"),
        }
        self.modules = Modules::start(&config, move |index, value| {
            let _ = events.send(Event::Module { index, value });
        });

        self.renderer = Renderer::new(config.bar.font.clone());
        self.app.reload(config);
    }

    /// Add the four edges for one output.
    fn create_surfaces(&mut self, qh: &QueueHandle<Self>, output: &wl_output::WlOutput) {
        for edge in Edge::ALL {
            let surface = self.compositor.create_surface(qh);
            let layer = self.layer_shell.create_layer_surface(
                qh,
                surface,
                Layer::Top,
                Some("quickbar"),
                Some(output),
            );
            layer.set_anchor(edge.anchor());
            // Never reserve space: the shell overlays whatever is underneath.
            layer.set_exclusive_zone(0);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            let (width, height) = edge.requested_size(self.app.config());
            layer.set_size(width, height);
            // A layer surface is only mapped after a commit with no buffer.
            layer.commit();

            let pool = match SlotPool::new(INITIAL_POOL, &self.shm) {
                Ok(pool) => pool,
                Err(error) => {
                    eprintln!("quickbar: edge {edge:?}: cannot create a buffer pool: {error}");
                    continue;
                }
            };
            self.surfaces
                .push(Surface::new(edge, output.clone(), layer, pool));
        }
    }

    fn draw_edges(&mut self, edges: &[Edge]) -> Result<(), String> {
        for index in 0..self.surfaces.len() {
            if edges.contains(&self.surfaces[index].edge) {
                self.draw_surface(index)?;
            }
        }
        Ok(())
    }

    fn draw_surface(&mut self, index: usize) -> Result<(), String> {
        let Shell {
            app,
            renderer,
            surfaces,
            compositor,
            active_output,
            ..
        } = self;

        let active = active_output
            .clone()
            .or_else(|| surfaces.first().map(|surface| surface.output.clone()));
        let config = app.config();
        let surface = &mut surfaces[index];
        let notifications = if active.as_ref() == Some(&surface.output) {
            app.notifications()
        } else {
            &[]
        };
        let content = Content {
            modules: app.modules(),
            notifications,
            bar_progress: app.bar_progress(),
        };
        surface.draw(compositor, config, &content, renderer)
    }

    fn dismiss_at(&mut self, index: usize, x: f64, y: f64) {
        let size = self.surfaces[index].size();
        let cards = notify::layout(
            self.app.config(),
            self.app.notifications(),
            &self.renderer,
            size,
        );
        if let Some(id) = notify::hit(&cards, x as f32, y as f32) {
            self.apply_event(Event::DismissNotification(id));
        }
    }
}

impl CompositorHandler for Shell {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
        // The output scale is not applied yet: surfaces are drawn in logical
        // pixels, which is what a scale of 1 means.
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
        // Frames are not requested: the slide is advanced from the event loop's
        // timeout instead, which keeps every surface in step.
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Shell {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.create_surfaces(qh, &output);
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.surfaces.retain(|surface| surface.output != output);
        if self.active_output.as_ref() == Some(&output) {
            self.active_output = None;
        }
    }
}

impl LayerShellHandler for Shell {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        // The compositor can take a layer surface away; drop ours with it.
        if let Some(index) = self.surfaces.iter().position(|surface| &surface.layer == layer) {
            let surface = self.surfaces.remove(index);
            eprintln!("quickbar: the compositor closed the {:?} edge", surface.edge);
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(index) = self.surfaces.iter().position(|surface| &surface.layer == layer) else {
            return;
        };
        let edge = self.surfaces[index].edge;
        let (wanted_width, wanted_height) = edge.requested_size(self.app.config());
        // Zero means "you decide", so fall back to what we asked for. An axis we
        // left at zero is the stretched one, and the compositor fills it in.
        let width = if configure.new_size.0 == 0 {
            wanted_width
        } else {
            configure.new_size.0
        };
        let height = if configure.new_size.1 == 0 {
            wanted_height
        } else {
            configure.new_size.1
        };

        self.surfaces[index].configure(width, height);
        if let Err(error) = self.draw_surface(index) {
            eprintln!("quickbar: {error}");
        }
    }
}

impl SeatHandler for Shell {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer
            && let Some(pointer) = self.pointer.take()
        {
            pointer.release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for Shell {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some(index) = self
                .surfaces
                .iter()
                .position(|surface| surface.layer.wl_surface() == &event.surface)
            else {
                continue;
            };
            let edge = self.surfaces[index].edge;
            let output = self.surfaces[index].output.clone();

            match event.kind {
                PointerEventKind::Enter { .. } => {
                    self.active_output = Some(output);
                    if edge == Edge::Top {
                        self.apply_event(Event::PointerEnter);
                    }
                }
                PointerEventKind::Leave { .. } => {
                    if edge == Edge::Top {
                        self.apply_event(Event::PointerLeave);
                    }
                }
                PointerEventKind::Press { .. } => {
                    if edge == Edge::Bottom {
                        self.dismiss_at(index, event.position.0, event.position.1);
                    }
                }
                PointerEventKind::Motion { .. }
                | PointerEventKind::Release { .. }
                | PointerEventKind::Axis { .. } => {}
            }
        }
    }
}

impl ShmHandler for Shell {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(Shell);

impl ProvidesRegistryState for Shell {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(Shell);
