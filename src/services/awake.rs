//! Keeping GitAgent working while its window is hidden.
//!
//! macOS puts an app it cannot see into App Nap, which throttles its main
//! thread to a crawl. That is where every run is driven from — the step that
//! waits on `git push` or a model, notices it finished and starts the next
//! one — and where the window notices what happens without a click: a run
//! Claude starts over MCP, an approval answered elsewhere, a trusted chain
//! moving on, the repositories read again. A hidden window looked stopped,
//! and caught up the moment it was shown again.
//!
//! Telling the system that work the person asked for is under way is the
//! documented way out. A window holds it for as long as it has a folder open,
//! so the welcome screen naps like any other app, and it still lets the Mac go
//! to sleep: watching repositories is not a reason to keep a laptop awake.

/// Held while a folder is open; dropping it lets the app nap again.
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
                &NSString::from_str("Watching repositories and running GitAgent flows"),
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
        // Released at the end of this scope. Not `drop()`: off macOS the type
        // holds nothing, and clippy rightly calls dropping it a no-op.
        {
            let held = Activity::begin();
            assert_eq!(held.is_some(), cfg!(target_os = "macos"));
        }
        // Taking it again after a release works: a window does exactly this
        // when another folder is opened in it.
        assert_eq!(Activity::begin().is_some(), cfg!(target_os = "macos"));
    }
}
