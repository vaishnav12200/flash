/// Events sent from background system tasks to the Wayland event loop.
#[derive(Debug, Clone, Copy)]
pub enum AppEvent {
    PtyActivity,
    PtyInputReady,
    FontFallbackReady,
    InitialFrameDeadline,
    RedrawRetry,
}
