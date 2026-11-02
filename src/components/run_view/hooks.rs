//! The run map's zoom and its replay timer: hooks, called once from `RunView` in the same
//! order on every render.

use super::*;

/// The map's zoom, and a count bumped each time it changes, which flashes its percentage.
pub(super) fn use_zoom() -> (Signal<f64>, Signal<u32>) {
    // The map's zoom, remembered between runs and restarts. The map is laid
    // out for the room it would have at this zoom, then drawn scaled to it —
    // so a big flow fits zoomed out, and zoomed in everything grows, scrolls,
    // and stays sharp, being drawn rather than magnified.
    let mut zoom = use_signal(|| store::run_zoom().clamp(ZOOM_MIN, ZOOM_MAX));
    // ⌘+ / ⌘− / ⌘0, around the middle of the view as the buttons do.
    use_future(move || async move {
        let mut keys = document::eval(ZOOM_KEYS_JS);
        while let Ok(key) = keys.recv::<String>().await {
            let now = *zoom.peek();
            let next = zoom_key(now, &key);
            if next != now {
                document::eval(&zoom_anchor_js(None));
                zoom.set(next);
            }
        }
    });
    // Each change of zoom flashes its percentage over the map for a moment:
    // the count re-keys the label, which restarts its fade. Not on opening —
    // only when the zoom actually moves.
    let mut zoom_flash = use_signal(|| 0u32);
    let last_zoom = use_hook(|| Rc::new(Cell::new(f64::NAN)));
    // Saved once it settles, not on every tick of a scroll or pinch.
    use_effect(move || {
        let level = *zoom.read();
        let before = last_zoom.replace(level);
        if !before.is_nan() && before != level {
            *zoom_flash.write() += 1;
        }
        spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            if *zoom.peek() == level {
                store::save_run_zoom(level);
            }
        });
    });
    (zoom, zoom_flash)
}

/// Moves a replay on by one change of the run every step of the chosen pace, while it plays.
/// `len` is the run's history as this render has it.
pub(super) fn use_replay(
    len: usize,
    mut cursor: Signal<Option<(u64, usize)>>,
    playing: Signal<bool>,
    speed: Signal<f64>,
) {
    // Read by the timer below, which outlives any one render's props.
    let history_len = use_hook(|| Rc::new(Cell::new(0usize)));
    history_len.set(len);
    use_future({
        let history_len = history_len.clone();
        move || {
            let history_len = history_len.clone();
            async move {
                loop {
                    let wait = REPLAY_STEP_MS / *speed.peek();
                    tokio::time::sleep(std::time::Duration::from_millis(wait as u64)).await;
                    if !*playing.peek() {
                        continue;
                    }
                    let now = *cursor.peek();
                    if now.is_some() {
                        cursor.set(advance(now, history_len.get()));
                    }
                }
            }
        }
    });
}
