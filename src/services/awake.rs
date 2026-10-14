//! Keeping a run going while the window is hidden.
//!
//! macOS puts an app it cannot see into App Nap, which throttles its main
//! thread to a crawl. That is where every run is driven from — the step that
//! waits on `git push` or a model, notices it finished and starts the next
//! one — so a hidden window looked like a stopped run, and caught up the
//! moment it was shown again. The commands themselves kept going; the app
//! stopped noticing.
//!
//! Telling the system that work the person started is under way is the
//! documented way out. It is held only while something is running, so an
//! idle GitAgent naps like any other app, and it still lets the Mac go to
//! sleep: a long run is not a reason to keep a laptop awake.

/// Held while work is running; dropping it lets the app nap again.
pub struct Activity {
    #[cfg(target_os = "macos")]
    token:
        objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2::runtime::NSObjectProtocol>>,
}

impl Activity {
    /// Starts holding off App Nap. `None` where there is no such thing.
    pub fn begin() -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
            let token = NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
                &NSString::from_str("Running a GitAgent flow"),
            );
            Some(Self { token })
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for Activity {
    fn drop(&mut self) {
        // SAFETY: `token` is exactly what `beginActivityWithOptions:reason:`
        // returned, ended once, here.
        unsafe { objc2_foundation::NSProcessInfo::processInfo().endActivity(&self.token) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_activity_is_held_where_the_os_has_one_and_released_on_drop() {
        let held = Activity::begin();
        assert_eq!(held.is_some(), cfg!(target_os = "macos"));
        drop(held);
        // Taking it again after a release works: the effect does exactly this
        // every time a run starts after the last one ended.
        assert_eq!(Activity::begin().is_some(), cfg!(target_os = "macos"));
    }
}
