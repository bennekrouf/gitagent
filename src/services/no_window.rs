//! Commands run in the background with no console window flashing up on
//! Windows. A GUI build has no console, so Windows opens one for every console
//! program it starts (git, node, python...) unless told not to.

#[cfg_attr(not(windows), allow(dead_code))]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub trait NoWindow {
    fn no_window(&mut self) -> &mut Self;
}

impl NoWindow for std::process::Command {
    #[cfg_attr(not(windows), allow(unused_variables))]
    fn no_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(self, CREATE_NO_WINDOW);
        self
    }
}

impl NoWindow for tokio::process::Command {
    fn no_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        self.creation_flags(CREATE_NO_WINDOW);
        self
    }
}
